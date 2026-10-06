//! Section 10 test 5: verifier mutations. A valid itinerary passes; each
//! single mutation is rejected with the expected violation code.

use allstops_gtfs::calendar::ServiceCalendar;
use allstops_gtfs::cluster::{ClusterConfig, cluster};
use allstops_gtfs::fixture::minimal_with;
use allstops_gtfs::select::{Rule, Selection, select};
use allstops_gtfs::{Feed, Limits};
use allstops_verify::{Context, parse, verify};
use serde_json::{Value, json};

fn feed() -> Feed {
    Feed::from_zip_bytes(
        &minimal_with(&[
            (
                "stops.txt",
                "stop_id,stop_name,stop_lat,stop_lon,location_type,parent_station\n\
                 A,Alpha,48.0900,11.5000,1,\nA1,Alpha,48.0900,11.5000,0,A\n\
                 B,Bravo,48.0950,11.5000,1,\nB1,Bravo,48.0950,11.5000,0,B\n\
                 C,Charlie,48.1000,11.5000,1,\nC1,Charlie 1,48.1000,11.5000,0,C\nC2,Charlie 2,48.1000,11.5001,0,C\n\
                 D,Delta,48.1027,11.5000,1,\nD1,Delta,48.1027,11.5000,0,D\n\
                 E,Echo,48.1100,11.5000,1,\nE1,Echo,48.1100,11.5000,0,E\n",
            ),
            (
                "routes.txt",
                "route_id,agency_id,route_short_name,route_type\nU1,A,U1,1\nU2,A,U2,1\n",
            ),
            (
                "trips.txt",
                "route_id,service_id,trip_id\nU1,WD,T1\nU1,WD,T5\nU2,WD,T6\nU1,WD,T7\nU2,WD,T8\n",
            ),
            (
                "stop_times.txt",
                "trip_id,arrival_time,departure_time,stop_id,stop_sequence,pickup_type,drop_off_type\n\
                 T1,08:00:00,08:00:00,A1,1,0,1\nT1,08:05:00,08:05:00,B1,2,0,0\nT1,08:10:00,08:10:00,C1,3,1,0\n\
                 T5,08:20:00,08:20:00,C2,1,0,1\nT5,08:25:00,08:25:00,D1,2,1,0\n\
                 T6,08:30:00,08:30:00,D1,1,0,1\nT6,08:35:00,08:35:00,E1,2,1,0\n\
                 T7,08:10:30,08:10:30,C1,1,0,1\nT7,08:16:00,08:16:00,D1,2,1,0\n\
                 T8,08:30:00,08:30:00,D1,1,1,0\nT8,08:35:00,08:35:00,E1,2,1,0\n",
            ),
            (
                "transfers.txt",
                "from_stop_id,to_stop_id,transfer_type,min_transfer_time\nC1,C2,3,\n",
            ),
        ]),
        &Limits::default(),
    )
    .unwrap()
}

fn rules() -> Value {
    json!({
        "mode": "stops", "selection": "s.toml", "date": "2026-11-12",
        "earliest_start": "04:30", "latest_end": "26:00", "start": "any", "end": "any",
        "allow_walking": true, "walking_speed_kmh": 4.5, "walk_detour_factor": 1.3,
        "max_walk_m": 1200.0, "connector_modes": ["tram", "bus"],
        "min_transfer_s": {"same_station": 60, "walk_link": 120},
        "tight_transfer_s": 120, "count_pass_through": false,
        "stay_aboard_through_terminus": false
    })
}

fn ride(trip: &str, b: &str, bt: &str, a: &str, at: &str, stations: &[&str]) -> Value {
    json!({
        "type": "ride", "trip_id": trip, "service_date": "2026-11-12",
        "route": "U", "headsign": "", "board_stop_id": b, "board_time": bt,
        "alight_stop_id": a, "alight_time": at, "stations": stations
    })
}

fn valid() -> Value {
    json!({
        "schema": "allstops-itinerary/0",
        "feed": {"id": "t", "sha256": "abc", "feed_version": "", "attribution": "test"},
        "timezone": "Europe/Berlin",
        "date": "2026-11-12",
        "rules": rules(),
        "targets": ["A", "B", "C", "D", "E"],
        "legs": [
            ride("T1", "A1", "08:00:00", "C1", "08:10:00", &["A", "B", "C"]),
            {"type": "walk", "from_station": "C", "to_station": "D", "start": "08:11:00", "end": "08:17:00", "metres": 300.2, "walking_speed_kmh": 4.5},
            {"type": "wait", "station": "D", "start": "08:17:00", "end": "08:30:00"},
            ride("T6", "D1", "08:30:00", "E1", "08:35:00", &["D", "E"]),
        ],
        "summary": {"first_visit": "08:00:00", "last_visit": "08:35:00", "duration_s": 2100,
                    "stations_visited": 5, "transfers": 1, "walk_m": 300.2},
        "lower_bound_s": null, "gap": null
    })
}

fn check(doc: &Value) -> Vec<&'static str> {
    let feed = feed();
    let cal = ServiceCalendar::new(&feed);
    let c = cluster(&feed, &ClusterConfig::default());
    let sel = Selection {
        name: "t".into(),
        include: vec![Rule {
            route_types: vec![1],
            ..Rule::default()
        }],
        exclude_stations: vec![],
    };
    let targets: Vec<String> = select(&feed, &c, &sel)
        .unwrap()
        .into_iter()
        .map(|i| c.stations[i as usize].id.clone())
        .collect();
    let ctx = Context {
        feed: &feed,
        calendar: &cal,
        clustering: &c,
        targets: &targets,
        visit_types: &[1..=1],
        feed_sha256: Some("abc"),
    };
    let it = parse(&doc.to_string()).expect("parses");
    let mut codes = verify(&ctx, &it).codes();
    codes.sort();
    codes.dedup();
    codes
}

#[test]
fn unmutated_itinerary_passes() {
    assert_eq!(check(&valid()), Vec::<&str>::new());
}

#[test]
fn departure_too_soon_after_arrival() {
    let mut d = valid();
    d["legs"] = json!([
        ride("T1", "A1", "08:00:00", "C1", "08:10:00", &["A", "B", "C"]),
        ride("T7", "C1", "08:10:30", "D1", "08:16:00", &["C", "D"]),
        ride("T6", "D1", "08:30:00", "E1", "08:35:00", &["D", "E"]),
    ]);
    assert_eq!(check(&d), vec!["TRANSFER_TOO_SHORT"]);
}

#[test]
fn wrong_service_date() {
    let mut d = valid();
    // 2026-11-14 is a Saturday; the weekday service does not run.
    d["legs"][0]["service_date"] = json!("2026-11-14");
    let codes = check(&d);
    assert!(codes.contains(&"SERVICE_NOT_RUNNING"), "{codes:?}");
}

#[test]
fn trip_does_not_stop_at_alight_stop() {
    let mut d = valid();
    d["legs"][0]["alight_stop_id"] = json!("E1");
    let codes = check(&d);
    assert!(codes.contains(&"ALIGHT_STOP_NOT_ON_TRIP"), "{codes:?}");
}

#[test]
fn boarding_where_pickup_is_not_allowed() {
    let mut d = valid();
    d["legs"][3]["trip_id"] = json!("T8");
    assert_eq!(check(&d), vec!["PICKUP_NOT_ALLOWED"]);
}

#[test]
fn walk_faster_than_rules_allow() {
    let mut d = valid();
    d["legs"][1]["end"] = json!("08:12:00");
    d["legs"][2]["start"] = json!("08:12:00");
    assert_eq!(check(&d), vec!["WALK_TOO_FAST"]);
}

#[test]
fn forbidden_transfer() {
    let mut d = valid();
    d["legs"] = json!([
        ride("T1", "A1", "08:00:00", "C1", "08:10:00", &["A", "B", "C"]),
        ride("T5", "C2", "08:20:00", "D1", "08:25:00", &["C", "D"]),
        ride("T6", "D1", "08:30:00", "E1", "08:35:00", &["D", "E"]),
    ]);
    assert_eq!(check(&d), vec!["FORBIDDEN_TRANSFER"]);
}

#[test]
fn missing_target() {
    let mut d = valid();
    d["legs"] = json!([ride(
        "T1",
        "A1",
        "08:00:00",
        "C1",
        "08:10:00",
        &["A", "B", "C"]
    )]);
    assert_eq!(check(&d), vec!["MISSING_TARGET"]);
}

#[test]
fn overlapping_legs() {
    let mut d = valid();
    d["legs"][1]["end"] = json!("08:31:00");
    d["legs"][2]["start"] = json!("08:31:00");
    d["legs"][2]["end"] = json!("08:31:00");
    let codes = check(&d);
    assert!(codes.contains(&"TIME_TRAVEL"), "{codes:?}");
}

#[test]
fn claimed_summary_must_match() {
    let mut d = valid();
    d["summary"]["duration_s"] = json!(2000);
    assert_eq!(check(&d), vec!["SUMMARY_MISMATCH"]);
}

#[test]
fn walking_disallowed_and_chained_walks() {
    let mut d = valid();
    d["rules"]["allow_walking"] = json!(false);
    assert!(check(&d).contains(&"WALK_NOT_ALLOWED"));
    let mut d = valid();
    d["legs"] = json!([
        ride("T1", "A1", "08:00:00", "C1", "08:10:00", &["A", "B", "C"]),
        {"type": "walk", "from_station": "C", "to_station": "D", "start": "08:11:00", "end": "08:17:00"},
        {"type": "walk", "from_station": "D", "to_station": "C", "start": "08:17:00", "end": "08:23:00"},
        {"type": "walk", "from_station": "C", "to_station": "D", "start": "08:23:00", "end": "08:29:00"},
        ride("T6", "D1", "08:30:00", "E1", "08:35:00", &["D", "E"]),
    ]);
    assert_eq!(check(&d), vec!["WALK_CHAINED"]);
}

#[test]
fn feed_hash_and_stations_list_are_checked() {
    let mut d = valid();
    d["feed"]["sha256"] = json!("other");
    assert_eq!(check(&d), vec!["FEED_MISMATCH"]);
    let mut d = valid();
    d["legs"][0]["stations"] = json!(["A", "C"]);
    assert_eq!(check(&d), vec!["STATIONS_MISMATCH"]);
}

/// The verifier must stay independent of the solver.
#[test]
fn verifier_does_not_use_the_solver_crate() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs")).unwrap();
    assert!(
        !src.contains("allstops_core"),
        "allstops-verify must not import allstops_core"
    );
    let manifest =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
    assert!(
        !manifest.contains("allstops-core"),
        "allstops-verify must not depend on allstops-core"
    );
    assert!(
        !manifest.contains("network"),
        "allstops-verify must not enable the solver-facing network feature of allstops-gtfs"
    );
}
