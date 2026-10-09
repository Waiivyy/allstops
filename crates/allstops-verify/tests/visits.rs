//! Section 10 test 3, visit semantics as the verifier recomputes them from
//! the legs: pass-through rows count only when the rules say so, and a walk
//! visits nothing.

use allstops_gtfs::calendar::ServiceCalendar;
use allstops_gtfs::cluster::{ClusterConfig, cluster};
use allstops_gtfs::fixture::minimal_with;
use allstops_gtfs::{Feed, Limits};
use allstops_verify::{Context, parse, verify};
use serde_json::{Value, json};

/// T1 runs S1, S2 (a pass-through), S3; S4 is 300 m from S3.
fn check(targets: &[&str], count_pass_through: bool, legs: Value) -> Vec<&'static str> {
    let feed = Feed::from_zip_bytes(
        &minimal_with(&[
            (
                "stops.txt",
                "stop_id,stop_name,stop_lat,stop_lon,location_type,parent_station\n\
                 S1,One,48.100,11.50,1,\nS1a,One,48.100,11.50,0,S1\n\
                 S2,Two,48.110,11.50,1,\nS2a,Two,48.110,11.50,0,S2\n\
                 S3,Three,48.120,11.50,1,\nS3a,Three,48.120,11.50,0,S3\n\
                 S4,Four,48.1227,11.50,1,\nS4a,Four,48.1227,11.50,0,S4\n",
            ),
            (
                "stop_times.txt",
                "trip_id,arrival_time,departure_time,stop_id,stop_sequence,pickup_type,drop_off_type\n\
                 T1,08:00:00,08:00:00,S1a,1,0,1\nT1,08:05:00,08:05:00,S2a,2,1,1\n\
                 T1,08:10:00,08:10:00,S3a,3,1,0\n",
            ),
        ]),
        &Limits::default(),
    )
    .unwrap();
    let cal = ServiceCalendar::new(&feed);
    let c = cluster(&feed, &ClusterConfig::default());
    let targets: Vec<String> = targets.iter().map(|s| s.to_string()).collect();
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
    let doc = json!({
        "schema": "allstops-itinerary/0",
        "feed": {"id": "t", "sha256": "x", "feed_version": "", "attribution": "test"},
        "timezone": "Europe/Berlin",
        "date": "2026-11-12",
        "rules": {
            "earliest_start": "04:30", "latest_end": "26:00", "allow_walking": true,
            "walking_speed_kmh": 4.5, "walk_detour_factor": 1.3, "max_walk_m": 1200.0,
            "connector_modes": [], "min_transfer_s": {"same_station": 60, "walk_link": 120},
            "count_pass_through": count_pass_through, "stay_aboard_through_terminus": false
        },
        "targets": targets,
        "legs": legs,
        "summary": {"duration_s": 600}
    });
    let mut codes = verify(&ctx, &parse(&doc.to_string()).unwrap()).codes();
    codes.sort();
    codes.dedup();
    codes
}

fn ride(stations: &[&str]) -> Value {
    json!({
        "type": "ride", "trip_id": "T1", "service_date": "2026-11-12",
        "board_stop_id": "S1a", "board_time": "08:00:00",
        "alight_stop_id": "S3a", "alight_time": "08:10:00", "stations": stations
    })
}

#[test]
fn a_pass_through_counts_only_when_the_rules_say_so() {
    let all = ["S1", "S2", "S3"];
    assert_eq!(
        check(&all, false, json!([ride(&["S1", "S3"])])),
        vec!["MISSING_TARGET"]
    );
    assert_eq!(
        check(&all, true, json!([ride(&["S1", "S2", "S3"])])),
        Vec::<&str>::new()
    );
    // The leg must not claim the pass-through either.
    assert_eq!(
        check(&["S1", "S3"], false, json!([ride(&["S1", "S2", "S3"])])),
        vec!["STATIONS_MISMATCH"]
    );
}

#[test]
fn a_walk_visits_nothing() {
    let walk = json!({
        "type": "walk", "from_station": "S3", "to_station": "S4",
        "start": "08:11:00", "end": "08:20:00"
    });
    assert_eq!(
        check(
            &["S1", "S3", "S4"],
            false,
            json!([ride(&["S1", "S3"]), walk])
        ),
        vec!["MISSING_TARGET"]
    );
}
