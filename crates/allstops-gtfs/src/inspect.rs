//! A feed profile: what a feed contains and what looks wrong with it.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::NaiveDate;
use serde::Serialize;

use crate::calendar::{ServiceCalendar, validity};
use crate::feed::{Feed, LoadWarnings, LocationType, StopIdx};
use crate::time::{ServiceSeconds, format_time};

pub const OPTIONAL_FILES: [&str; 8] = [
    "calendar.txt",
    "calendar_dates.txt",
    "transfers.txt",
    "frequencies.txt",
    "shapes.txt",
    "feed_info.txt",
    "pathways.txt",
    "levels.txt",
];

#[derive(Debug, Serialize)]
pub struct Profile {
    pub publisher: Option<String>,
    pub feed_version: Option<String>,
    pub timezone: String,
    pub declared_validity: Option<(NaiveDate, NaiveDate)>,
    pub service_range: Option<(NaiveDate, NaiveDate)>,
    pub validity: Option<(NaiveDate, NaiveDate)>,
    pub files_present: Vec<String>,
    pub optional_files_missing: Vec<String>,
    /// Optional files present but holding a header only.
    pub files_empty: Vec<String>,
    pub agencies: usize,
    pub routes_by_type: BTreeMap<u16, RouteTypeCount>,
    pub stops_by_location_type: BTreeMap<String, usize>,
    pub stops_with_parent: usize,
    pub hierarchy_depth: usize,
    pub id_scheme: IdScheme,
    pub trips: usize,
    pub stop_times: usize,
    pub services: usize,
    pub trips_past_midnight: usize,
    pub latest_time: String,
    pub frequencies_rows: usize,
    pub frequencies_exact_times: usize,
    pub transfers_rows: usize,
    pub pickup_drop_off: BTreeMap<String, usize>,
    pub warnings: Warnings,
}

#[derive(Debug, Default, Serialize)]
pub struct RouteTypeCount {
    pub routes: usize,
    pub trips: usize,
}

#[derive(Debug, Serialize)]
pub struct IdScheme {
    /// Share of stop IDs shaped like a German DHID (`de:<5-digit area>:...`).
    pub dhid_share: f64,
    pub examples: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Warnings {
    pub load: LoadWarningsView,
    /// Stops and stations no trip serves, directly or through a child stop.
    pub orphan_stops: usize,
    /// Trips whose service never runs inside the validity range.
    pub trips_without_service_days: usize,
    /// Trips where a time goes backwards along the stop sequence.
    pub trips_non_monotonic: usize,
    /// Trips identical to another trip in route, service and every stop time.
    pub duplicate_trips: usize,
    /// Stops with missing or invalid coordinates.
    pub stops_without_coordinates: usize,
}

#[derive(Debug, Serialize)]
pub struct LoadWarningsView {
    pub stop_times_unknown_trip: u64,
    pub stop_times_unknown_stop: u64,
    pub stop_times_filled_time: u64,
    pub stop_times_interpolated: u64,
    pub stop_times_missing_time: u64,
    pub trips_unknown_route: u64,
    pub trips_without_stop_times: u64,
    pub stops_unknown_parent: u64,
    pub calendar_dates_bad_exception: u64,
    pub frequencies_unknown_trip: u64,
    pub transfers_unknown_stop: u64,
}

impl From<&LoadWarnings> for LoadWarningsView {
    fn from(w: &LoadWarnings) -> Self {
        LoadWarningsView {
            stop_times_unknown_trip: w.stop_times_unknown_trip,
            stop_times_unknown_stop: w.stop_times_unknown_stop,
            stop_times_filled_time: w.stop_times_filled_time,
            stop_times_interpolated: w.stop_times_interpolated,
            stop_times_missing_time: w.stop_times_missing_time,
            trips_unknown_route: w.trips_unknown_route,
            trips_without_stop_times: w.trips_without_stop_times,
            stops_unknown_parent: w.stops_unknown_parent,
            calendar_dates_bad_exception: w.calendar_dates_bad_exception,
            frequencies_unknown_trip: w.frequencies_unknown_trip,
            transfers_unknown_stop: w.transfers_unknown_stop,
        }
    }
}

fn location_name(l: LocationType) -> &'static str {
    match l {
        LocationType::Platform => "stop_or_platform",
        LocationType::Station => "station",
        LocationType::Entrance => "entrance",
        LocationType::GenericNode => "generic_node",
        LocationType::BoardingArea => "boarding_area",
    }
}

fn is_dhid(id: &str) -> bool {
    let mut parts = id.split(':');
    let (Some(country), Some(area)) = (parts.next(), parts.next()) else {
        return false;
    };
    country.len() == 2
        && country.bytes().all(|b| b.is_ascii_lowercase())
        && area.len() == 5
        && area.bytes().all(|b| b.is_ascii_digit())
        && parts.next().is_some()
}

pub fn profile(feed: &Feed) -> Profile {
    let cal = ServiceCalendar::new(feed);
    let validity_range = validity(feed, &cal);

    let mut routes_by_type: BTreeMap<u16, RouteTypeCount> = BTreeMap::new();
    for r in &feed.routes {
        routes_by_type.entry(r.route_type).or_default().routes += 1;
    }
    for t in &feed.trips {
        let rt = feed.routes[t.route as usize].route_type;
        routes_by_type.entry(rt).or_default().trips += 1;
    }

    let mut stops_by_location_type = BTreeMap::new();
    let mut depth = 0;
    for (i, s) in feed.stops.iter().enumerate() {
        *stops_by_location_type
            .entry(location_name(s.location_type).to_string())
            .or_insert(0) += 1;
        let mut d = 0;
        let mut cur = i as StopIdx;
        while let Some(p) = feed.stops[cur as usize].parent {
            d += 1;
            if d > 8 || p == cur {
                break;
            }
            cur = p;
        }
        depth = depth.max(d);
    }
    let stops_with_parent = feed.stops.iter().filter(|s| s.parent.is_some()).count();
    let dhid = feed.stops.iter().filter(|s| is_dhid(&s.id)).count();
    let id_scheme = IdScheme {
        dhid_share: if feed.stops.is_empty() {
            0.0
        } else {
            dhid as f64 / feed.stops.len() as f64
        },
        examples: feed
            .stops
            .iter()
            .filter(|s| s.location_type == LocationType::Platform && s.parent.is_some())
            .take(3)
            .map(|s| s.id.clone())
            .collect(),
    };

    let mut latest: ServiceSeconds = 0;
    let mut past_midnight = 0;
    let mut non_monotonic = 0;
    let mut used = vec![false; feed.stops.len()];
    let mut pickup_drop_off: BTreeMap<String, usize> = BTreeMap::new();
    for (ti, _) in feed.trips.iter().enumerate() {
        let st = feed.trip_stop_times(ti as u32);
        let mut prev = i32::MIN;
        let mut bad = false;
        for s in st {
            if s.arrival < prev || s.departure < s.arrival {
                bad = true;
            }
            prev = s.departure;
            used[s.stop as usize] = true;
            latest = latest.max(s.departure).max(s.arrival);
            *pickup_drop_off
                .entry(format!(
                    "pickup={} drop_off={}",
                    s.pickup_type, s.drop_off_type
                ))
                .or_insert(0) += 1;
        }
        if bad {
            non_monotonic += 1;
        }
        if st.last().is_some_and(|s| s.arrival >= 24 * 3600) {
            past_midnight += 1;
        }
    }
    // A station counts as used when any of its descendants is.
    for i in 0..feed.stops.len() {
        if used[i] {
            let mut cur = i as StopIdx;
            for _ in 0..8 {
                match feed.stops[cur as usize].parent {
                    Some(p) if p != cur => {
                        used[p as usize] = true;
                        cur = p;
                    }
                    _ => break,
                }
            }
        }
    }
    let orphan_stops = feed
        .stops
        .iter()
        .enumerate()
        .filter(|(i, s)| {
            !used[*i]
                && matches!(
                    s.location_type,
                    LocationType::Platform | LocationType::Station
                )
        })
        .count();

    let mut service_days: HashMap<u32, bool> = HashMap::new();
    let mut without_days = 0;
    for t in &feed.trips {
        let runs = *service_days.entry(t.service).or_insert_with(|| {
            validity_range.is_some_and(|(s, e)| cal.active_days(t.service, s, e) > 0)
        });
        if !runs {
            without_days += 1;
        }
    }

    let mut seen: HashSet<(u32, u32, Vec<(StopIdx, i32, i32)>)> = HashSet::new();
    let mut duplicates = 0;
    for (ti, t) in feed.trips.iter().enumerate() {
        let key: Vec<(StopIdx, i32, i32)> = feed
            .trip_stop_times(ti as u32)
            .iter()
            .map(|s| (s.stop, s.arrival, s.departure))
            .collect();
        if !key.is_empty() && !seen.insert((t.route, t.service, key)) {
            duplicates += 1;
        }
    }

    let present: HashSet<&str> = feed.files.iter().map(String::as_str).collect();
    Profile {
        publisher: feed.feed_info.as_ref().map(|f| f.publisher_name.clone()),
        feed_version: feed.feed_info.as_ref().map(|f| f.version.clone()),
        timezone: feed
            .agencies
            .first()
            .map(|a| a.timezone.clone())
            .unwrap_or_default(),
        declared_validity: feed
            .feed_info
            .as_ref()
            .and_then(|f| Some((f.start_date?, f.end_date?))),
        service_range: cal.service_range(),
        validity: validity_range,
        files_present: feed.files.clone(),
        optional_files_missing: OPTIONAL_FILES
            .iter()
            .filter(|f| !present.contains(**f))
            .map(|f| f.to_string())
            .collect(),
        files_empty: feed.empty_files.clone(),
        agencies: feed.agencies.len(),
        routes_by_type,
        stops_by_location_type,
        stops_with_parent,
        hierarchy_depth: depth,
        id_scheme,
        trips: feed.trips.len(),
        stop_times: feed.stop_times.len(),
        services: feed.service_ids.len(),
        trips_past_midnight: past_midnight,
        latest_time: format_time(latest),
        frequencies_rows: feed.frequencies.len(),
        frequencies_exact_times: feed.frequencies.iter().filter(|f| f.exact_times).count(),
        transfers_rows: feed.transfers.len(),
        pickup_drop_off,
        warnings: Warnings {
            load: (&feed.warnings).into(),
            orphan_stops,
            trips_without_service_days: without_days,
            trips_non_monotonic: non_monotonic,
            duplicate_trips: duplicates,
            stops_without_coordinates: feed
                .stops
                .iter()
                .filter(|s| {
                    matches!(
                        s.location_type,
                        LocationType::Platform | LocationType::Station
                    ) && !(s.lat.is_finite() && s.lon.is_finite())
                })
                .count(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Limits;
    use crate::fixture::minimal_with;

    #[test]
    fn profiles_minimal_feed() {
        let feed = Feed::from_zip_bytes(&minimal_with(&[]), &Limits::default()).unwrap();
        let p = profile(&feed);
        assert_eq!(p.trips, 1);
        assert_eq!(p.routes_by_type[&1].routes, 1);
        assert_eq!(p.hierarchy_depth, 1);
        assert_eq!(p.warnings.orphan_stops, 0);
        assert!(
            p.optional_files_missing
                .contains(&"transfers.txt".to_string())
        );
    }

    #[test]
    fn flags_duplicates_non_monotonic_and_orphans() {
        let feed = Feed::from_zip_bytes(
            &minimal_with(&[
                (
                    "stops.txt",
                    "stop_id,stop_name,stop_lat,stop_lon\nS1a,A,48.1,11.5\nS2a,B,48.1,11.6\nZ,Lonely,48.2,11.7\n",
                ),
                (
                    "trips.txt",
                    "route_id,service_id,trip_id\nR,WD,T1\nR,WD,T2\nR,NEVER,T3\n",
                ),
                (
                    "stop_times.txt",
                    "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
                     T1,08:00:00,08:00:00,S1a,1\nT1,08:05:00,08:05:00,S2a,2\n\
                     T2,08:00:00,08:00:00,S1a,1\nT2,08:05:00,08:05:00,S2a,2\n\
                     T3,09:00:00,09:00:00,S1a,1\nT3,08:55:00,08:55:00,S2a,2\n",
                ),
            ]),
            &Limits::default(),
        )
        .unwrap();
        let p = profile(&feed);
        assert_eq!(p.warnings.duplicate_trips, 1);
        assert_eq!(p.warnings.trips_non_monotonic, 1);
        assert_eq!(p.warnings.orphan_stops, 1);
        assert_eq!(p.warnings.trips_without_service_days, 1);
    }

    #[test]
    fn recognises_dhid_ids() {
        assert!(is_dhid("de:09162:6:41:41"));
        assert!(is_dhid("de:09162:6"));
        assert!(!is_dhid("at:45"));
        assert!(!is_dhid("12345"));
        assert!(!is_dhid("DE:09162:6"));
    }
}
