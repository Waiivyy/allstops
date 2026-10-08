//! Build the routing network for one plan date from a loaded feed.

use std::collections::HashMap;
use std::ops::RangeInclusive;

use allstops_core::builder::{Call, NetworkBuilder};
use allstops_core::network::{self as net, Network, StationIdx};
use allstops_core::rules::{Rules, parse_clock, route_types_for_mode};
use chrono::NaiveDate;
use serde::Serialize;

use crate::calendar::{ServiceCalendar, check_plan_date, day_offset};
use crate::cluster::{Clustering, distance_m};
use crate::error::{Error, Result};
use crate::feed::Feed;
pub use crate::walks::{WalkOverride, WalkOverrides};

#[derive(Debug, Clone, Serialize)]
pub struct BuildReport {
    pub date: NaiveDate,
    pub window: (String, String),
    pub trips: usize,
    pub connections: usize,
    pub stations: usize,
    pub footpaths: usize,
    pub targets: usize,
    /// Target stations with no usable departure or arrival in the window.
    pub unserved_targets: Vec<String>,
    /// Trips left out because a time goes backwards along the stop sequence.
    pub trips_skipped_backwards: usize,
    /// Trips left out because they have more stops than a hop index holds.
    pub trips_skipped_too_long: usize,
    /// Stations whose change time transfers.txt raised above the rules.
    pub stations_with_transfer_minimum: usize,
    /// Walk links that transfers.txt made longer.
    pub walks_with_transfer_minimum: usize,
    /// Directed walk links whose time or existence walks.toml set.
    pub walk_overrides_applied: usize,
    /// Directed walks.toml entries that matched no generated link (the
    /// stations are farther apart than max_walk_m, or not in the network).
    pub walk_overrides_unused: usize,
}

/// More walk links than this from one station means the coordinates are
/// almost certainly wrong (thousands of stops in one place).
pub const MAX_WALKS_PER_STATION: usize = 2_000;

/// Upper limit on walk links in one network.
pub const MAX_FOOTPATHS: usize = 5_000_000;

/// Longest trip, in calls, that a network can hold (hop positions are u16).
pub const MAX_CALLS_PER_TRIP: usize = u16::MAX as usize + 2;

fn in_ranges(rt: u16, ranges: &[RangeInclusive<u16>]) -> bool {
    ranges.iter().any(|r| r.contains(&rt))
}

/// Walk time in whole seconds for a straight-line distance under the rules,
/// before the walk-link minimum is applied.
pub fn walk_seconds(metres: f64, rules: &Rules) -> i32 {
    let speed = rules.walking_speed_kmh / 3.6;
    (metres * rules.walk_detour_factor / speed).ceil() as i32
}

/// Duration of a walk link: the walk time, but never less than the
/// walk-link minimum transfer.
pub fn walk_duration(metres: f64, rules: &Rules) -> i32 {
    walk_seconds(metres, rules).max(rules.min_transfer_s.walk_link)
}

/// Build the network for `rules.date`.
///
/// `visit_types` are the route types whose trips count as visits (the
/// target mode); `rules.connector_modes` add route types that may be ridden
/// but never count.
pub fn build_network(
    feed: &Feed,
    cal: &ServiceCalendar,
    clustering: &Clustering,
    targets: &[u32],
    visit_types: &[RangeInclusive<u16>],
    rules: &Rules,
    walks: &WalkOverrides,
) -> Result<(Network, BuildReport)> {
    let bad = |m: String| Error::File {
        file: "rules".into(),
        message: m,
    };
    // Entries naming stations outside the network count as unused; callers
    // check the stations against the full feed (`WalkOverrides::check_stations`).
    walks.validate().map_err(|m| Error::File {
        file: "walks.toml".into(),
        message: m,
    })?;
    rules
        .validate()
        .map_err(|m| bad(format!("invalid rules: {m}")))?;
    let date = NaiveDate::parse_from_str(&rules.date, "%Y-%m-%d")
        .map_err(|_| bad(format!("bad date {:?}", rules.date)))?;
    check_plan_date(feed, cal, date)?;
    if rules.stay_aboard_through_terminus {
        return Err(bad(
            "stay_aboard_through_terminus is not supported yet".into()
        ));
    }
    if rules.end != "any" {
        return Err(bad(format!(
            "end = {:?}: a fixed end station is not supported yet; use \"any\"",
            rules.end
        )));
    }
    if rules.start != "any"
        && !targets
            .iter()
            .any(|&t| clustering.stations[t as usize].id == rules.start)
    {
        return Err(bad(format!(
            "start = {:?} is not one of the selection's target stations",
            rules.start
        )));
    }
    let ws = parse_clock(&rules.earliest_start).ok_or_else(|| bad("bad earliest_start".into()))?;
    let we = parse_clock(&rules.latest_end).ok_or_else(|| bad("bad latest_end".into()))?;
    let mut connector: Vec<RangeInclusive<u16>> = Vec::new();
    for m in &rules.connector_modes {
        connector.extend(
            route_types_for_mode(m).ok_or_else(|| bad(format!("unknown connector mode {m:?}")))?,
        );
    }
    let tz = feed.timezone()?;

    // Trips whose times go backwards are left out: the network and the
    // verifier would otherwise disagree about when they call where.
    let backwards: Vec<bool> = (0..feed.trips.len() as u32)
        .map(|t| {
            let st = feed.trip_stop_times(t);
            st.iter().any(|s| s.departure < s.arrival)
                || st.windows(2).any(|w| w[1].arrival < w[0].departure)
        })
        .collect();

    // Service days whose trips can reach the window: a trip of day D+k runs
    // from about k days (an hour either way across daylight-saving
    // changes) to k days plus the latest time in the feed.
    let latest = feed
        .stop_times
        .iter()
        .map(|s| s.arrival.max(s.departure))
        .max()
        .unwrap_or(0);
    let day = 86_400i64;
    let k_lo = (i64::from(ws) - i64::from(latest) - 3600).div_euclid(day);
    let k_hi = (i64::from(we) + 3600).div_euclid(day);

    let mut b = NetworkBuilder::new(ws, we, rules.min_transfer_s.same_station);
    let mut station_map: HashMap<u32, StationIdx> = HashMap::new();
    let mut stop_map: HashMap<u32, u32> = HashMap::new();
    let mut add_station = |b: &mut NetworkBuilder, cs: u32| -> StationIdx {
        *station_map.entry(cs).or_insert_with(|| {
            let s = &clustering.stations[cs as usize];
            b.add_station(net::Station {
                id: s.id.clone(),
                name: s.name.clone(),
                lat: s.lat,
                lon: s.lon,
            })
        })
    };
    // Targets first, so they exist even without service.
    for &t in targets {
        let s = add_station(&mut b, t);
        b.add_target(s);
    }

    let mut trips_added = 0;
    let mut skipped_backwards = 0;
    let mut skipped_too_long = 0;
    for delta in k_lo..=k_hi {
        let day = date + chrono::Duration::days(delta);
        let Some(offset) = day_offset(&tz, date, day) else {
            if delta == 0 {
                return Err(Error::DateNotInTimeZone {
                    date,
                    tz: tz.name().to_string(),
                });
            }
            // A neighbouring date the time zone skipped has no service day.
            continue;
        };
        let day_str = day.format("%Y-%m-%d").to_string();
        for (ti, trip) in feed.trips.iter().enumerate() {
            if trip.frequency_template {
                continue;
            }
            let route = &feed.routes[trip.route as usize];
            let visits = in_ranges(route.route_type, visit_types);
            if !visits && !in_ranges(route.route_type, &connector) {
                continue;
            }
            let st = feed.trip_stop_times(ti as u32);
            let (Some(first), Some(last)) = (st.first(), st.last()) else {
                continue;
            };
            if first.departure + offset > we || last.arrival + offset < ws {
                continue;
            }
            if !cal.is_active(trip.service, day) {
                continue;
            }
            if backwards[ti] {
                skipped_backwards += 1;
                continue;
            }
            if st.len() > MAX_CALLS_PER_TRIP {
                skipped_too_long += 1;
                continue;
            }
            let mut calls = Vec::with_capacity(st.len());
            for s in st {
                let cs = clustering.station_of_stop[s.stop as usize];
                let station = add_station(&mut b, cs);
                let stop = *stop_map.entry(s.stop).or_insert_with(|| {
                    let fs = &feed.stops[s.stop as usize];
                    b.add_stop(net::Stop {
                        id: fs.id.clone(),
                        station,
                        platform: fs.platform_code.clone(),
                    })
                });
                calls.push(Call {
                    stop,
                    station,
                    arr: s.arrival + offset,
                    dep: s.departure + offset,
                    pickup: s.pickup_allowed(),
                    drop_off: s.drop_off_allowed(),
                    counts: !s.is_pass_through() || rules.count_pass_through,
                });
            }
            let name = if route.short_name.is_empty() {
                route.long_name.clone()
            } else {
                route.short_name.clone()
            };
            let added = b.add_trip(
                net::Trip {
                    gtfs_id: trip.id.clone(),
                    service_date: day_str.clone(),
                    offset,
                    route: name,
                    headsign: trip.headsign.clone(),
                    route_type: route.route_type,
                    visits,
                    conns_start: 0,
                    conns_end: 0,
                },
                &calls,
            );
            if added.is_some() {
                trips_added += 1;
            }
        }
    }

    let mut network = b.build();
    let mut walk_overrides: HashMap<(StationIdx, StationIdx), Option<i32>> = HashMap::new();
    let mut walk_overrides_unused = 0;
    for ((from, to), v) in walks.directed() {
        let net_idx = |id: &str| {
            clustering
                .stations
                .iter()
                .position(|s| s.id == id)
                .and_then(|cs| station_map.get(&(cs as u32)).copied())
        };
        match (net_idx(&from), net_idx(&to)) {
            (Some(a), Some(z)) => {
                walk_overrides.insert((a, z), v);
            }
            _ => walk_overrides_unused += 1,
        }
    }
    let mut walk_overrides_applied = 0;
    if rules.allow_walking {
        walk_overrides_applied = add_footpaths(&mut network, rules, &walk_overrides)?;
        walk_overrides_unused += walk_overrides.len() - walk_overrides_applied;
    }
    let route_in_network: Vec<bool> = feed
        .routes
        .iter()
        .map(|r| in_ranges(r.route_type, visit_types) || in_ranges(r.route_type, &connector))
        .collect();
    let (stations_raised, walks_raised) = apply_transfers(
        feed,
        clustering,
        &station_map,
        &route_in_network,
        &mut network,
    )?;
    network.validate().map_err(|m| Error::File {
        file: "network".into(),
        message: format!("internal check failed: {m}"),
    })?;

    // Targets that no visiting trip serves inside the window.
    let mut served = vec![false; network.stations.len()];
    for c in &network.connections {
        if c.dep >= ws && c.dep <= we && c.has(net::flag::VISIT_DEP) && c.has(net::flag::PICKUP) {
            served[c.dep_station as usize] = true;
        }
        if c.arr >= ws && c.arr <= we && c.has(net::flag::VISIT_ARR) {
            served[c.arr_station as usize] = true;
        }
    }
    let unserved_targets = network
        .targets
        .iter()
        .filter(|&&t| !served[t as usize])
        .map(|&t| {
            format!(
                "{} ({})",
                network.stations[t as usize].name, network.stations[t as usize].id
            )
        })
        .collect();

    let report = BuildReport {
        date,
        window: (rules.earliest_start.clone(), rules.latest_end.clone()),
        trips: trips_added,
        connections: network.connections.len(),
        stations: network.stations.len(),
        footpaths: network.footpaths.len(),
        targets: network.targets.len(),
        unserved_targets,
        trips_skipped_backwards: skipped_backwards,
        trips_skipped_too_long: skipped_too_long,
        stations_with_transfer_minimum: stations_raised,
        walks_with_transfer_minimum: walks_raised,
        walk_overrides_applied,
        walk_overrides_unused,
    };
    Ok((network, report))
}

/// Apply transfers.txt conservatively at station level. A minimum time
/// (`transfer_type = 2`) between two stops of one station raises that
/// station's change time; between two stations it lengthens the walk link.
/// The verifier picks the most specific row for each change, and that row's
/// requirement is never above the largest minimum applied here, so plans
/// satisfy it. Forbidden transfers (`transfer_type = 3`) need per-stop
/// labels that the scan does not have yet; a feed that forbids a transfer
/// inside the network is refused rather than planned wrongly. A row that
/// names a route or trip outside the network can match no change in it and
/// is skipped.
fn apply_transfers(
    feed: &Feed,
    clustering: &Clustering,
    station_map: &HashMap<u32, StationIdx>,
    route_in_network: &[bool],
    network: &mut Network,
) -> Result<(usize, usize)> {
    let net_station = |stop: Option<u32>| -> Option<StationIdx> {
        let cs = clustering.station_of_stop[stop? as usize];
        station_map.get(&cs).copied()
    };
    let mut forbidden = 0;
    let mut raised_stations = std::collections::BTreeSet::new();
    let mut raised_walks = 0;
    let route_ok = |r: Option<u32>| r.is_none_or(|r| route_in_network[r as usize]);
    let trip_ok =
        |t: Option<u32>| t.is_none_or(|t| route_in_network[feed.trips[t as usize].route as usize]);
    for t in &feed.transfers {
        let (Some(a), Some(z)) = (net_station(t.from_stop), net_station(t.to_stop)) else {
            continue;
        };
        if !(route_ok(t.from_route)
            && route_ok(t.to_route)
            && trip_ok(t.from_trip)
            && trip_ok(t.to_trip))
        {
            continue;
        }
        match (t.transfer_type, t.min_transfer_time) {
            (3, _) => forbidden += 1,
            (2, Some(m)) if a == z => {
                let c = &mut network.change_time[a as usize];
                if m > *c {
                    *c = m;
                    raised_stations.insert(a);
                }
            }
            (2, Some(m)) => {
                let (lo, hi) = (
                    network.fp_start[a as usize],
                    network.fp_start[a as usize + 1],
                );
                for f in &mut network.footpaths[lo as usize..hi as usize] {
                    if f.to == z && m > f.duration {
                        f.duration = m;
                        raised_walks += 1;
                    }
                }
            }
            _ => {}
        }
    }
    if forbidden > 0 {
        return Err(Error::File {
            file: "transfers.txt".into(),
            message: format!(
                "{forbidden} rows forbid transfers between stations of this network; planning with forbidden transfers is not supported yet"
            ),
        });
    }
    Ok((raised_stations.len(), raised_walks))
}

/// Generate walking links between every pair of network stations within
/// `rules.max_walk_m` straight-line distance, applying walks.toml overrides
/// (keyed by network station). Returns how many overrides matched a link.
fn add_footpaths(
    network: &mut Network,
    rules: &Rules,
    overrides: &HashMap<(StationIdx, StationIdx), Option<i32>>,
) -> Result<usize> {
    let max = rules.max_walk_m;
    let n = network.stations.len();
    if max < 1.0 {
        network.fp_start = vec![0; n + 1];
        network.footpaths = Vec::new();
        return Ok(0);
    }
    let mut applied = 0usize;
    // Grid of cells at least `max` metres across in both directions, so
    // every pair within `max` lies in neighbouring cells. One longitude
    // width for the whole network, sized for its most poleward station.
    let cell_lat = max / 111_000.0;
    let max_abs_lat = network
        .stations
        .iter()
        .filter(|s| s.lat.is_finite())
        .map(|s| s.lat.abs())
        .fold(0.0f64, f64::max);
    let cell_lon = cell_lat / max_abs_lat.min(85.0).to_radians().cos();
    let mut grid: HashMap<(i64, i64), Vec<u32>> = HashMap::new();
    let key = |lat: f64, lon: f64| -> (i64, i64) {
        (
            (lat / cell_lat).floor() as i64,
            (lon / cell_lon).floor() as i64,
        )
    };
    for (i, s) in network.stations.iter().enumerate() {
        if s.lat.is_finite() && s.lon.is_finite() {
            grid.entry(key(s.lat, s.lon)).or_default().push(i as u32);
        }
    }
    let too_dense = |i: usize| Error::File {
        file: "stops.txt".into(),
        message: format!(
            "more than {MAX_WALKS_PER_STATION} stations lie within {max} m of {:?}; check the stop coordinates",
            network.stations[i].name
        ),
    };
    let mut per_station: Vec<Vec<net::Footpath>> = vec![Vec::new(); n];
    let mut total = 0usize;
    for (i, s) in network.stations.iter().enumerate() {
        if !(s.lat.is_finite() && s.lon.is_finite()) {
            continue;
        }
        let (gy, gx) = key(s.lat, s.lon);
        for dy in -1..=1i64 {
            for dx in -1..=1i64 {
                let cell = (gy.saturating_add(dy), gx.saturating_add(dx));
                for &j in grid.get(&cell).into_iter().flatten() {
                    if j as usize == i {
                        continue;
                    }
                    let o = &network.stations[j as usize];
                    let d = distance_m(s.lat, s.lon, o.lat, o.lon);
                    if d <= max {
                        if per_station[i].len() >= MAX_WALKS_PER_STATION {
                            return Err(too_dense(i));
                        }
                        let duration = match overrides.get(&(i as StationIdx, j)) {
                            Some(None) => {
                                applied += 1;
                                continue;
                            }
                            Some(Some(measured)) => {
                                applied += 1;
                                (*measured).max(rules.min_transfer_s.walk_link)
                            }
                            None => walk_duration(d, rules),
                        };
                        per_station[i].push(net::Footpath {
                            to: j,
                            duration,
                            metres: d as f32,
                        });
                        total += 1;
                        if total > MAX_FOOTPATHS {
                            return Err(Error::File {
                                file: "stops.txt".into(),
                                message: format!(
                                    "more than {MAX_FOOTPATHS} walk links; check the stop coordinates or lower max_walk_m"
                                ),
                            });
                        }
                    }
                }
            }
        }
    }
    let mut fp_start = vec![0u32; n + 1];
    let mut footpaths = Vec::with_capacity(total);
    for (i, mut list) in per_station.into_iter().enumerate() {
        list.sort_by_key(|f| f.to);
        footpaths.extend(list);
        fp_start[i + 1] = footpaths.len() as u32;
    }
    network.fp_start = fp_start;
    network.footpaths = footpaths;
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_date_the_time_zone_skipped_is_an_error_not_a_panic() {
        use crate::calendar::ServiceCalendar;
        use crate::cluster::{ClusterConfig, cluster};
        use crate::fixture::minimal_with;
        // Samoa skipped 2011-12-30 when it moved across the date line.
        let feed = crate::Feed::from_zip_bytes(
            &minimal_with(&[
                (
                    "agency.txt",
                    "agency_id,agency_name,agency_url,agency_timezone\nA,Agency,https://example.org,Pacific/Apia\n",
                ),
                (
                    "calendar.txt",
                    "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\n\
                     WD,1,1,1,1,1,1,1,20111201,20120131\n",
                ),
            ]),
            &crate::Limits::default(),
        )
        .unwrap();
        let cal = ServiceCalendar::new(&feed);
        let c = cluster(&feed, &ClusterConfig::default());
        let targets: Vec<u32> = vec![c.station_of_stop[feed.stop_index["S1a"] as usize]];
        for (d, ok) in [
            ("2011-12-29", true),
            ("2011-12-30", false),
            ("2011-12-31", true),
        ] {
            let rules = Rules {
                date: d.into(),
                ..Rules::default()
            };
            let r = build_network(
                &feed,
                &cal,
                &c,
                &targets,
                &[1..=1],
                &rules,
                &WalkOverrides::default(),
            );
            assert_eq!(r.is_ok(), ok, "{d}: {:?}", r.err());
        }
    }

    fn build(files: &[(&str, &str)], rules: Rules) -> Result<(Network, BuildReport)> {
        use crate::calendar::ServiceCalendar;
        use crate::cluster::{ClusterConfig, cluster};
        use crate::fixture::minimal_with;
        let feed =
            crate::Feed::from_zip_bytes(&minimal_with(files), &crate::Limits::default()).unwrap();
        let cal = ServiceCalendar::new(&feed);
        let c = cluster(&feed, &ClusterConfig::default());
        let sel = crate::select::Selection {
            name: "t".into(),
            include: vec![crate::select::Rule {
                route_types: vec![1],
                ..Default::default()
            }],
            exclude_stations: vec![],
        };
        let targets = crate::select::select(&feed, &c, &sel).unwrap();
        build_network(
            &feed,
            &cal,
            &c,
            &targets,
            &[1..=1],
            &rules,
            &WalkOverrides::default(),
        )
    }

    fn rules(date: &str) -> Rules {
        Rules {
            date: date.into(),
            ..Rules::default()
        }
    }

    const ALL_WEEK: &str = "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\n\
                            WD,1,1,1,1,1,1,1,20261001,20261213\n";

    #[test]
    fn windows_past_the_next_day_load_the_right_service_days() {
        let st = "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
                  T1,00:20:00,00:20:00,S1a,1\nT1,00:50:00,00:50:00,S2a,2\n";
        for (start, end) in [("24:00", "26:00"), ("48:00", "50:00")] {
            let r = Rules {
                earliest_start: start.into(),
                latest_end: end.into(),
                ..rules("2026-11-02")
            };
            let (_, rep) = build(&[("calendar.txt", ALL_WEEK), ("stop_times.txt", st)], r).unwrap();
            assert!(
                rep.unserved_targets.is_empty(),
                "{start}-{end}: {:?}",
                rep.unserved_targets
            );
            assert_eq!(rep.trips, 1, "{start}-{end}");
        }
    }

    #[test]
    fn trips_going_backwards_are_left_out_and_counted() {
        let st = "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
                  T1,08:00:30,08:00:30,S1a,1\nT1,08:00:00,08:00:00,S2a,2\n";
        let (_, rep) = build(&[("stop_times.txt", st)], rules("2026-11-12")).unwrap();
        assert_eq!(rep.trips, 0);
        assert_eq!(rep.trips_skipped_backwards, 1);
        assert_eq!(rep.unserved_targets.len(), 2);
    }

    #[test]
    fn transfer_minimums_raise_change_and_walk_times() {
        let stops = "stop_id,stop_name,stop_lat,stop_lon,location_type,parent_station\n\
                     S1,One,48.1,11.5,1,\nS1a,One,48.1,11.5,0,S1\nS1b,One,48.1,11.5,0,S1\n\
                     S2,Two,48.101,11.5,1,\nS2a,Two,48.101,11.5,0,S2\n";
        let tr =
            "from_stop_id,to_stop_id,transfer_type,min_transfer_time\nS1a,S1b,2,400\nS1,S2,2,900\n";
        let (net, rep) = build(
            &[("stops.txt", stops), ("transfers.txt", tr)],
            rules("2026-11-12"),
        )
        .unwrap();
        let one = net.stations.iter().position(|s| s.id == "S1").unwrap();
        assert_eq!(net.change_time[one], 400);
        let walk = net.footpaths_from(one as u32)[0];
        assert_eq!(walk.duration, 900);
        assert_eq!(
            (
                rep.stations_with_transfer_minimum,
                rep.walks_with_transfer_minimum
            ),
            (1, 1)
        );
    }

    #[test]
    fn forbidden_transfers_are_refused_not_ignored() {
        let tr = "from_stop_id,to_stop_id,transfer_type,min_transfer_time\nS1a,S1a,3,\n";
        let err = build(&[("transfers.txt", tr)], rules("2026-11-12")).unwrap_err();
        assert!(err.to_string().contains("forbid"), "{err}");
    }

    #[test]
    fn unsupported_end_and_unknown_start_are_errors() {
        let r = Rules {
            end: "S2".into(),
            ..rules("2026-11-12")
        };
        assert!(
            build(&[], r)
                .unwrap_err()
                .to_string()
                .contains("not supported")
        );
        let r = Rules {
            start: "nowhere".into(),
            ..rules("2026-11-12")
        };
        assert!(
            build(&[], r)
                .unwrap_err()
                .to_string()
                .contains("not one of")
        );
        let r = Rules {
            start: "S1".into(),
            ..rules("2026-11-12")
        };
        assert!(build(&[], r).is_ok());
    }

    #[test]
    fn frequency_runs_are_routed_and_templates_are_not() {
        let freq =
            "trip_id,start_time,end_time,headway_secs,exact_times\nT1,06:00:00,07:00:00,1200,1\n";
        let (net, rep) = build(&[("frequencies.txt", freq)], rules("2026-11-12")).unwrap();
        assert_eq!(rep.trips, 3, "three runs, not the template");
        let ids: Vec<&str> = net.trips.iter().map(|t| t.gtfs_id.as_str()).collect();
        assert_eq!(ids, vec!["T1@06:00:00", "T1@06:20:00", "T1@06:40:00"]);
    }

    #[test]
    fn a_zero_walk_limit_means_no_walks() {
        let r = Rules {
            max_walk_m: 0.0,
            ..rules("2026-11-12")
        };
        let (net, _) = build(&[], r).unwrap();
        assert!(net.footpaths.is_empty());
    }

    #[test]
    fn walk_times_follow_the_rules() {
        let r = Rules::default();
        // 1000 m * 1.3 / (4.5 km/h = 1.25 m/s) = 1040 s.
        assert_eq!(walk_seconds(1000.0, &r), 1040);
        assert_eq!(walk_duration(1000.0, &r), 1040);
        // Short walks are raised to the walk-link minimum of 120 s.
        assert_eq!(walk_duration(50.0, &r), 120);
    }
}
