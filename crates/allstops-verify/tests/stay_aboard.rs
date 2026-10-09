//! Staying aboard through a terminus: a ride marked `stay_aboard` continues
//! the previous ride on the trip the same vehicle runs next, without
//! alighting, boarding or a change time, and only when the rules allow it.

use allstops_gtfs::calendar::ServiceCalendar;
use allstops_gtfs::cluster::{ClusterConfig, cluster};
use allstops_gtfs::fixture::minimal_with;
use allstops_gtfs::{Feed, Limits};
use allstops_verify::{Context, parse, verify};
use serde_json::{Value, json};

const STOPS: &str = "stop_id,stop_name,stop_lat,stop_lon,location_type,parent_station\n\
    S1,One,48.10,11.50,1,\nS1a,One,48.10,11.50,0,S1\n\
    S2,Two,48.11,11.50,1,\nS2a,Two,48.11,11.50,0,S2\nS2b,Two,48.11,11.50,0,S2\n\
    S3,Three,48.12,11.50,1,\nS3a,Three,48.12,11.50,0,S3\n";

/// A runs S1, S2, S2' (08:00 to 08:10); B runs S2 to S3 (08:10:30 to
/// 08:30). Nobody may get off A at its terminus or on B at its first stop:
/// the only way from A to B is to stay aboard.
const STOP_TIMES: &str = "trip_id,arrival_time,departure_time,stop_id,stop_sequence,pickup_type,drop_off_type\n\
    A,08:00:00,08:00:00,S1a,1,0,1\nA,08:05:00,08:05:00,S2b,2,0,0\nA,08:10:00,08:10:00,S2a,3,1,1\n\
    B,08:10:30,08:10:30,S2b,1,1,1\nB,08:30:00,08:30:00,S3a,2,1,0\n";

fn check(trips: &str, doc: &Value) -> Vec<&'static str> {
    check_with(trips, STOP_TIMES, None, doc)
}

fn check_with(
    trips: &str,
    stop_times: &str,
    transfers: Option<&str>,
    doc: &Value,
) -> Vec<&'static str> {
    let mut files = vec![
        ("stops.txt", STOPS),
        ("trips.txt", trips),
        ("stop_times.txt", stop_times),
    ];
    if let Some(t) = transfers {
        files.push(("transfers.txt", t));
    }
    let feed = Feed::from_zip_bytes(&minimal_with(&files), &Limits::default()).unwrap();
    let cal = ServiceCalendar::new(&feed);
    let c = cluster(&feed, &ClusterConfig::default());
    let targets: Vec<String> = ["S1", "S2", "S3"].iter().map(|s| s.to_string()).collect();
    let ctx = Context {
        feed: &feed,
        calendar: &cal,
        clustering: &c,
        targets: &targets,
        visit_types: &[1..=1],
        feed_sha256: None,
        expected_rules: None,
        walks: &Default::default(),
    };
    let mut codes = verify(&ctx, &parse(&doc.to_string()).unwrap()).codes();
    codes.sort();
    codes.dedup();
    codes
}

const BLOCK: &str =
    "route_id,service_id,trip_id,trip_headsign,block_id\nR,WD,A,Two,X\nR,WD,B,Three,X\n";

fn ride(trip: &str, b: &str, bt: &str, a: &str, at: &str, st: &[&str], stay: bool) -> Value {
    let mut v = json!({
        "type": "ride", "trip_id": trip, "service_date": "2026-11-12",
        "board_stop_id": b, "board_time": bt,
        "alight_stop_id": a, "alight_time": at, "stations": st
    });
    if stay {
        v["stay_aboard"] = json!(true);
    }
    v
}

fn doc(stay_rule: bool, legs: Vec<Value>) -> Value {
    json!({
        "schema": "allstops-itinerary/0",
        "feed": {"id": "t", "sha256": "x", "feed_version": "", "attribution": "test"},
        "timezone": "Europe/Berlin",
        "date": "2026-11-12",
        "rules": {
            "earliest_start": "04:30", "latest_end": "26:00", "allow_walking": true,
            "walking_speed_kmh": 4.5, "walk_detour_factor": 1.3, "max_walk_m": 1200.0,
            "connector_modes": [], "min_transfer_s": {"same_station": 60, "walk_link": 120},
            "count_pass_through": false, "stay_aboard_through_terminus": stay_rule
        },
        "targets": ["S1", "S2", "S3"],
        "legs": legs,
        "summary": {"duration_s": 1800}
    })
}

fn through() -> Vec<Value> {
    vec![
        ride(
            "A",
            "S1a",
            "08:00:00",
            "S2a",
            "08:10:00",
            &["S1", "S2"],
            false,
        ),
        // B's first call is a pass-through, so B visits only S3.
        ride("B", "S2b", "08:10:30", "S3a", "08:30:00", &["S3"], true),
    ]
}

#[test]
fn staying_aboard_along_the_block_passes() {
    assert_eq!(check(BLOCK, &doc(true, through())), Vec::<&str>::new());
}

#[test]
fn the_same_legs_as_a_change_break_the_rules() {
    let mut legs = through();
    legs[1]["stay_aboard"] = json!(false);
    let codes = check(BLOCK, &doc(true, legs));
    for c in [
        "DROP_OFF_NOT_ALLOWED",
        "PICKUP_NOT_ALLOWED",
        "TRANSFER_TOO_SHORT",
    ] {
        assert!(codes.contains(&c), "{c} missing from {codes:?}");
    }
}

#[test]
fn staying_aboard_needs_the_rule() {
    assert_eq!(
        check(BLOCK, &doc(false, through())),
        vec!["STAY_ABOARD_NOT_ALLOWED"]
    );
}

#[test]
fn staying_aboard_needs_a_real_continuation() {
    let other =
        "route_id,service_id,trip_id,trip_headsign,block_id\nR,WD,A,Two,X\nR,WD,B,Three,Y\n";
    assert_eq!(
        check(other, &doc(true, through())),
        vec!["NOT_A_CONTINUATION"]
    );
    // The previous ride must end at its trip's last stop.
    let mut legs = through();
    legs[0] = ride(
        "A",
        "S1a",
        "08:00:00",
        "S2b",
        "08:05:00",
        &["S1", "S2"],
        false,
    );
    let codes = check(BLOCK, &doc(true, legs));
    assert!(codes.contains(&"NOT_A_CONTINUATION"), "{codes:?}");
    // A first leg cannot stay aboard anything.
    let legs = vec![ride(
        "B",
        "S2b",
        "08:10:30",
        "S3a",
        "08:30:00",
        &["S3"],
        true,
    )];
    let codes = check(BLOCK, &doc(true, legs));
    assert!(codes.contains(&"NOT_A_CONTINUATION"), "{codes:?}");
}

#[test]
fn a_repeated_linked_trips_row_is_one_link() {
    let none = "route_id,service_id,trip_id,trip_headsign\nR,WD,A,Two\nR,WD,B,Three\n";
    let twice = "from_stop_id,to_stop_id,from_trip_id,to_trip_id,transfer_type,min_transfer_time\n\
                 ,,A,B,4,\n,,A,B,4,\n";
    assert_eq!(
        check_with(none, STOP_TIMES, Some(twice), &doc(true, through())),
        Vec::<&str>::new()
    );
}

#[test]
fn a_one_stop_trip_in_the_block_comes_between() {
    // The vehicle runs C, a single call at S2, between A and B: A does not
    // continue as B.
    let trips = "route_id,service_id,trip_id,trip_headsign,block_id\n\
                 R,WD,A,Two,X\nR,WD,C,Two,X\nR,WD,B,Three,X\n";
    let stop_times = format!("{STOP_TIMES}C,08:10:10,08:10:10,S2a,1,1,1\n");
    assert_eq!(
        check_with(trips, &stop_times, None, &doc(true, through())),
        vec!["NOT_A_CONTINUATION"]
    );
}
