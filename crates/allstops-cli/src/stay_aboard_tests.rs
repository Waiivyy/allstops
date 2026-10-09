//! End to end: planning with staying aboard through a terminus gives an
//! itinerary the independent verifier accepts.

use allstops_core::csa::Csa;
use allstops_core::itinerary::{FeedRef, Leg, to_itinerary};
use allstops_core::plan::greedy_from;
use allstops_core::rules::Rules;
use allstops_gtfs::calendar::ServiceCalendar;
use allstops_gtfs::cluster::{ClusterConfig, cluster};
use allstops_gtfs::fixture::minimal_with;
use allstops_gtfs::network::build_network;
use allstops_gtfs::select::{Rule, Selection, select};
use allstops_gtfs::{Feed, Limits};

use crate::check::{check_json, rules_for_verifier};
use crate::plan_input::Basis;

/// A runs S1, S2, S2' and B runs S2' to S3 as the same vehicle (block X).
/// Nobody may get off at A's terminus or on at B's first stop, so S3 can
/// only be reached by staying aboard.
fn basis() -> Basis {
    let feed = Feed::from_zip_bytes(
        &minimal_with(&[
            (
                "stops.txt",
                "stop_id,stop_name,stop_lat,stop_lon,location_type,parent_station\n\
                 S1,One,48.10,11.50,1,\nS1a,One,48.10,11.50,0,S1\n\
                 S2,Two,48.11,11.50,1,\nS2a,Two,48.11,11.50,0,S2\nS2b,Two,48.11,11.50,0,S2\n\
                 S3,Three,48.12,11.50,1,\nS3a,Three,48.12,11.50,0,S3\n",
            ),
            (
                "trips.txt",
                "route_id,service_id,trip_id,trip_headsign,block_id\nR,WD,A,Two,X\nR,WD,B,Three,X\n",
            ),
            (
                "stop_times.txt",
                "trip_id,arrival_time,departure_time,stop_id,stop_sequence,pickup_type,drop_off_type\n\
                 A,08:00:00,08:00:00,S1a,1,0,1\nA,08:05:00,08:05:00,S2b,2,0,0\nA,08:10:00,08:10:00,S2a,3,1,1\n\
                 B,08:10:30,08:10:30,S2a,1,1,1\nB,08:30:00,08:30:00,S3a,2,1,0\n",
            ),
        ]),
        &Limits::default(),
    )
    .unwrap();
    let clustering = cluster(&feed, &ClusterConfig::default());
    let sel = Selection {
        name: "U".into(),
        include: vec![Rule {
            route_types: vec![1],
            ..Rule::default()
        }],
        exclude_stations: vec![],
    };
    let targets = select(&feed, &clustering, &sel).unwrap();
    Basis {
        visit_types: vec![1..=1],
        feed_ref: FeedRef {
            id: "t".into(),
            sha256: "x".into(),
            feed_version: String::new(),
            attribution: "test".into(),
        },
        timezone: "Europe/Berlin".into(),
        feed,
        clustering,
        targets,
        walks: Default::default(),
        load_ms: 0.0,
        pack: None,
    }
}

fn plan(b: &Basis, stay: bool) -> Option<String> {
    let rules = Rules {
        date: "2026-11-12".into(),
        stay_aboard_through_terminus: stay,
        ..Rules::default()
    };
    let (net, _) = build_network(
        &b.feed,
        &ServiceCalendar::new(&b.feed),
        &b.clustering,
        &b.targets,
        &b.visit_types,
        &rules,
        &b.walks,
    )
    .unwrap();
    let start = net.stations.iter().position(|s| s.id == "S1").unwrap() as u32;
    let best = greedy_from(&mut Csa::new(&net), start, 7 * 3600)?;
    let it = to_itinerary(&net, &best.plan, &rules, b.feed_ref.clone(), &b.timezone).unwrap();
    assert!(
        it.legs
            .iter()
            .any(|l| matches!(l, Leg::Ride { stay_aboard: true, trip_id, .. } if trip_id == "B")),
        "{:#?}",
        it.legs
    );
    let json = serde_json::to_string(&it).unwrap();
    let report = check_json(b, Some(&rules_for_verifier(&rules).unwrap()), &json).unwrap();
    assert!(report.passed, "{:?}", report.violations);
    assert_eq!(report.duration_s, Some(i64::from(best.last - best.first)));
    Some(json)
}

#[test]
fn a_route_that_stays_aboard_through_a_terminus_verifies() {
    let b = basis();
    assert!(plan(&b, true).is_some());
    assert!(
        plan(&b, false).is_none(),
        "without the rule S3 is out of reach"
    );
}
