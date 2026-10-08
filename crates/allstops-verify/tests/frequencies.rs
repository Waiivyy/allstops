//! Runs expanded from frequencies.txt can be ridden; the template trip
//! itself never runs and is rejected.

use allstops_gtfs::calendar::ServiceCalendar;
use allstops_gtfs::cluster::{ClusterConfig, cluster};
use allstops_gtfs::fixture::minimal_with;
use allstops_gtfs::{Feed, Limits};
use allstops_verify::{Context, parse, verify};
use serde_json::json;

fn check(trip: &str, board: &str, alight: &str) -> Vec<&'static str> {
    let feed = Feed::from_zip_bytes(
        &minimal_with(&[(
            "frequencies.txt",
            "trip_id,start_time,end_time,headway_secs,exact_times\nT1,06:00:00,07:00:00,1200,1\n",
        )]),
        &Limits::default(),
    )
    .unwrap();
    let cal = ServiceCalendar::new(&feed);
    let c = cluster(&feed, &ClusterConfig::default());
    let targets = vec!["S1".to_string(), "S2".to_string()];
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
            "count_pass_through": false, "stay_aboard_through_terminus": false
        },
        "targets": ["S1", "S2"],
        "legs": [{
            "type": "ride", "trip_id": trip, "service_date": "2026-11-12",
            "board_stop_id": "S1a", "board_time": board,
            "alight_stop_id": "S2a", "alight_time": alight, "stations": ["S1", "S2"]
        }],
        "summary": {"duration_s": 300}
    });
    let mut codes = verify(&ctx, &parse(&doc.to_string()).unwrap()).codes();
    codes.dedup();
    codes
}

#[test]
fn an_expanded_run_can_be_ridden() {
    assert_eq!(
        check("T1@06:20:00", "06:20:00", "06:25:00"),
        Vec::<&str>::new()
    );
}

#[test]
fn the_template_trip_is_rejected() {
    assert_eq!(
        check("T1", "08:00:00", "08:05:00"),
        vec!["TRIP_NOT_RUN", "MISSING_TARGET"]
    );
}
