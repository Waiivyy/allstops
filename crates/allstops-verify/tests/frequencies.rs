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

/// Ride the run T1@06:20:00 to S2, change platforms and ride T2 on at 06:30,
/// under a transfers.txt row for changes from trip T1 (the template).
fn change_after_run(row: &str) -> Vec<&'static str> {
    let feed = Feed::from_zip_bytes(
        &minimal_with(&[
            (
                "stops.txt",
                "stop_id,stop_name,stop_lat,stop_lon,location_type,parent_station\n\
                 S1,One,48.1,11.5,1,\nS1a,One,48.1,11.5,0,S1\n\
                 S2,Two,48.11,11.51,1,\nS2a,Two,48.11,11.51,0,S2\nS2b,Two,48.11,11.51,0,S2\n\
                 S3,Three,48.12,11.52,1,\nS3a,Three,48.12,11.52,0,S3\n",
            ),
            (
                "trips.txt",
                "route_id,service_id,trip_id,trip_headsign\nR,WD,T1,Two\nR,WD,T2,Three\n",
            ),
            (
                "stop_times.txt",
                "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
                 T1,08:00:00,08:00:00,S1a,1\nT1,08:05:00,08:05:00,S2a,2\n\
                 T2,06:30:00,06:30:00,S2b,1\nT2,06:35:00,06:35:00,S3a,2\n",
            ),
            (
                "frequencies.txt",
                "trip_id,start_time,end_time,headway_secs,exact_times\nT1,06:00:00,07:00:00,1200,1\n",
            ),
            (
                "transfers.txt",
                &format!("from_stop_id,to_stop_id,transfer_type,min_transfer_time,from_trip_id\n{row}\n"),
            ),
        ]),
        &Limits::default(),
    )
    .unwrap();
    let cal = ServiceCalendar::new(&feed);
    let c = cluster(&feed, &ClusterConfig::default());
    let targets = vec!["S1".to_string(), "S2".to_string(), "S3".to_string()];
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
    let ride = |trip: &str, b: &str, bt: &str, a: &str, at: &str, st: &[&str]| {
        json!({
            "type": "ride", "trip_id": trip, "service_date": "2026-11-12",
            "board_stop_id": b, "board_time": bt,
            "alight_stop_id": a, "alight_time": at, "stations": st
        })
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
        "targets": ["S1", "S2", "S3"],
        "legs": [
            ride("T1@06:20:00", "S1a", "06:20:00", "S2a", "06:25:00", &["S1", "S2"]),
            ride("T2", "S2b", "06:30:00", "S3a", "06:35:00", &["S2", "S3"]),
        ],
        "summary": {"duration_s": 900}
    });
    let mut codes = verify(&ctx, &parse(&doc.to_string()).unwrap()).codes();
    codes.dedup();
    codes
}

#[test]
fn transfer_rows_for_the_template_apply_to_its_runs() {
    assert_eq!(change_after_run("S2a,S2b,2,120,T1"), Vec::<&str>::new());
    assert_eq!(
        change_after_run("S2a,S2b,2,1800,T1"),
        vec!["TRANSFER_TOO_SHORT"]
    );
    assert_eq!(
        change_after_run("S2a,S2b,3,,T1"),
        vec!["FORBIDDEN_TRANSFER"]
    );
}
