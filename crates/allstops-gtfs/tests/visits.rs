//! Section 10 test 3, visit semantics, from GTFS rows to visit labels:
//! pass-through rows do not count unless the rules say so; boarding and
//! arriving aboard count; walking to or past a station does not. Staying
//! aboard through a terminus is covered in `blocks.rs` (network) and in the
//! verifier's `stay_aboard.rs`.

use allstops_core::csa::{Csa, Origin};
use allstops_core::network::{INF, Network};
use allstops_core::rules::Rules;
use allstops_gtfs::calendar::ServiceCalendar;
use allstops_gtfs::cluster::{ClusterConfig, cluster};
use allstops_gtfs::fixture::minimal_with;
use allstops_gtfs::network::build_network;
use allstops_gtfs::{Feed, Limits};

/// T1 runs S1, S2 (a pass-through: no pickup, no drop-off), S3. S4 is
/// 300 m from S1 and served by nothing.
fn network(count_pass_through: bool) -> Network {
    let feed = Feed::from_zip_bytes(
        &minimal_with(&[
            (
                "stops.txt",
                "stop_id,stop_name,stop_lat,stop_lon,location_type,parent_station\n\
                 S1,One,48.100,11.50,1,\nS1a,One,48.100,11.50,0,S1\n\
                 S2,Two,48.110,11.50,1,\nS2a,Two,48.110,11.50,0,S2\n\
                 S3,Three,48.120,11.50,1,\nS3a,Three,48.120,11.50,0,S3\n\
                 S4,Four,48.1027,11.50,1,\nS4a,Four,48.1027,11.50,0,S4\n",
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
    let c = cluster(&feed, &ClusterConfig::default());
    let targets: Vec<u32> = ["S1a", "S2a", "S3a", "S4a"]
        .iter()
        .map(|s| c.station_of_stop[feed.stop_index[*s] as usize])
        .collect();
    let rules = Rules {
        date: "2026-11-12".into(),
        count_pass_through,
        ..Rules::default()
    };
    build_network(
        &feed,
        &ServiceCalendar::new(&feed),
        &c,
        &targets,
        &[1..=1],
        &rules,
        &Default::default(),
    )
    .unwrap()
    .0
}

/// Earliest visit of each station from S1 at 07:50, by station ID.
fn visits(net: &Network) -> Vec<(String, i32)> {
    let s1 = net.stations.iter().position(|s| s.id == "S1").unwrap() as u32;
    let mut csa = Csa::new(net);
    let l = csa.run(
        Origin::At {
            station: s1,
            time: 7 * 3600 + 50 * 60,
        },
        None,
    );
    net.stations
        .iter()
        .zip(&l.visit)
        .map(|(s, &v)| (s.id.clone(), v))
        .collect()
}

fn at(v: &[(String, i32)], id: &str) -> i32 {
    v.iter().find(|(s, _)| s == id).unwrap().1
}

#[test]
fn boarding_counts_and_arriving_aboard_counts() {
    let v = visits(&network(false));
    assert_eq!(at(&v, "S1"), 8 * 3600, "boarded at 08:00");
    assert_eq!(at(&v, "S3"), 8 * 3600 + 600, "arrived aboard at 08:10");
}

#[test]
fn a_pass_through_counts_only_when_the_rules_say_so() {
    assert_eq!(at(&visits(&network(false)), "S2"), INF);
    assert_eq!(at(&visits(&network(true)), "S2"), 8 * 3600 + 300);
}

#[test]
fn walking_to_a_station_does_not_visit_it() {
    let net = network(false);
    let s1 = net.stations.iter().position(|s| s.id == "S1").unwrap();
    let s4 = net.stations.iter().position(|s| s.id == "S4").unwrap() as u32;
    assert!(
        net.footpaths_from(s1 as u32).iter().any(|f| f.to == s4),
        "S4 is within walking distance"
    );
    assert_eq!(at(&visits(&net), "S4"), INF);
}
