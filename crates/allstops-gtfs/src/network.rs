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
}

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
) -> Result<(Network, BuildReport)> {
    let bad = |m: String| Error::File {
        file: "rules".into(),
        message: m,
    };
    let date = NaiveDate::parse_from_str(&rules.date, "%Y-%m-%d")
        .map_err(|_| bad(format!("bad date {:?}", rules.date)))?;
    check_plan_date(feed, cal, date)?;
    if rules.stay_aboard_through_terminus {
        return Err(bad(
            "stay_aboard_through_terminus is not supported yet".into()
        ));
    }
    let ws = parse_clock(&rules.earliest_start).ok_or_else(|| bad("bad earliest_start".into()))?;
    let we = parse_clock(&rules.latest_end).ok_or_else(|| bad("bad latest_end".into()))?;
    if we <= ws {
        return Err(bad("latest_end must be after earliest_start".into()));
    }
    let mut connector: Vec<RangeInclusive<u16>> = Vec::new();
    for m in &rules.connector_modes {
        connector.extend(
            route_types_for_mode(m).ok_or_else(|| bad(format!("unknown connector mode {m:?}")))?,
        );
    }
    let tz = feed.timezone()?;

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
    for delta in -1..=1i64 {
        let day = date + chrono::Duration::days(delta);
        let offset = day_offset(&tz, date, day);
        let day_str = day.format("%Y-%m-%d").to_string();
        for (ti, trip) in feed.trips.iter().enumerate() {
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
            let mut calls = Vec::with_capacity(st.len());
            for s in st {
                let cs = clustering.station_of_stop[s.stop as usize];
                if cs == u32::MAX {
                    continue;
                }
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
    if rules.allow_walking {
        add_footpaths(&mut network, rules);
    }

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
    };
    Ok((network, report))
}

/// Generate walking links between every pair of network stations within
/// `rules.max_walk_m` straight-line distance.
fn add_footpaths(network: &mut Network, rules: &Rules) {
    let max = rules.max_walk_m;
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
    let mut per_station: Vec<Vec<net::Footpath>> = vec![Vec::new(); network.stations.len()];
    for (i, s) in network.stations.iter().enumerate() {
        if !(s.lat.is_finite() && s.lon.is_finite()) {
            continue;
        }
        let (gy, gx) = key(s.lat, s.lon);
        for dy in -1..=1 {
            for dx in -1..=1 {
                for &j in grid.get(&(gy + dy, gx + dx)).into_iter().flatten() {
                    if j as usize == i {
                        continue;
                    }
                    let o = &network.stations[j as usize];
                    let d = distance_m(s.lat, s.lon, o.lat, o.lon);
                    if d <= max {
                        per_station[i].push(net::Footpath {
                            to: j,
                            duration: walk_duration(d, rules),
                            metres: d as f32,
                        });
                    }
                }
            }
        }
    }
    let mut fp_start = vec![0u32; network.stations.len() + 1];
    let mut footpaths = Vec::new();
    for (i, mut list) in per_station.into_iter().enumerate() {
        list.sort_by_key(|f| f.to);
        footpaths.extend(list);
        fp_start[i + 1] = footpaths.len() as u32;
    }
    network.fp_start = fp_start;
    network.footpaths = footpaths;
}

#[cfg(test)]
mod tests {
    use super::*;

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
