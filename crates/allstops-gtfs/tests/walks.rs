//! walks.toml: measured walk times replace the straight-line estimate, and
//! links can be forbidden.

use allstops_core::network::Network;
use allstops_core::rules::Rules;
use allstops_gtfs::calendar::ServiceCalendar;
use allstops_gtfs::cluster::{ClusterConfig, cluster};
use allstops_gtfs::fixture::minimal_with;
use allstops_gtfs::network::{BuildReport, WalkOverrides, build_network};
use allstops_gtfs::{Feed, Limits};

/// S1 and S2 about 1,112 m apart (an estimate of 1,157 s at the default
/// rules), S3 far away.
fn build(walks: &str) -> Result<(Network, BuildReport), String> {
    let feed = Feed::from_zip_bytes(
        &minimal_with(&[(
            "stops.txt",
            "stop_id,stop_name,stop_lat,stop_lon,location_type,parent_station\n\
             S1,One,48.10,11.50,1,\nS1a,One,48.10,11.50,0,S1\n\
             S2,Two,48.11,11.50,1,\nS2a,Two,48.11,11.50,0,S2\n\
             S3,Three,48.30,11.50,1,\nS3a,Three,48.30,11.50,0,S3\n",
        )]),
        &Limits::default(),
    )
    .unwrap();
    let cal = ServiceCalendar::new(&feed);
    let c = cluster(&feed, &ClusterConfig::default());
    let targets = vec![
        c.station_of_stop[feed.stop_index["S1a"] as usize],
        c.station_of_stop[feed.stop_index["S2a"] as usize],
    ];
    let walks: WalkOverrides = toml::from_str(walks).map_err(|e| e.to_string())?;
    let rules = Rules {
        date: "2026-11-12".into(),
        ..Rules::default()
    };
    build_network(&feed, &cal, &c, &targets, &[1..=1], &rules, &walks).map_err(|e| e.to_string())
}

fn walk(net: &Network, from: &str, to: &str) -> Option<i32> {
    let a = net.stations.iter().position(|s| s.id == from)? as u32;
    let b = net.stations.iter().position(|s| s.id == to)? as u32;
    net.footpaths_from(a)
        .iter()
        .find(|f| f.to == b)
        .map(|f| f.duration)
}

#[test]
fn without_overrides_the_estimate_is_used() {
    let (net, _) = build("").unwrap();
    let est = walk(&net, "S1", "S2").unwrap();
    assert!((1100..1200).contains(&est), "{est}");
}

#[test]
fn a_measured_time_replaces_the_estimate_both_ways() {
    let (net, rep) = build("[[walk]]\nfrom = \"S1\"\nto = \"S2\"\nseconds = 600\n").unwrap();
    assert_eq!(walk(&net, "S1", "S2"), Some(600));
    assert_eq!(walk(&net, "S2", "S1"), Some(600));
    assert_eq!(rep.walk_overrides_applied, 2);
}

#[test]
fn one_way_overrides_and_the_walk_link_minimum() {
    let (net, _) =
        build("[[walk]]\nfrom = \"S1\"\nto = \"S2\"\nseconds = 30\nboth_ways = false\n").unwrap();
    assert_eq!(
        walk(&net, "S1", "S2"),
        Some(120),
        "never below the walk-link minimum"
    );
    assert!(
        walk(&net, "S2", "S1").unwrap() > 1100,
        "the other direction keeps the estimate"
    );
}

#[test]
fn a_forbidden_link_is_removed() {
    let (net, _) = build("[[walk]]\nfrom = \"S1\"\nto = \"S2\"\nforbid = true\n").unwrap();
    assert_eq!(walk(&net, "S1", "S2"), None);
    assert_eq!(walk(&net, "S2", "S1"), None);
}

#[test]
fn an_override_beyond_the_walk_limit_adds_nothing_and_is_reported() {
    let (net, rep) = build("[[walk]]\nfrom = \"S1\"\nto = \"S3\"\nseconds = 60\n").unwrap();
    assert_eq!(walk(&net, "S1", "S3"), None);
    assert_eq!(rep.walk_overrides_unused, 2);
}

#[test]
fn mistakes_are_errors() {
    for (text, needle) in [
        (
            "[[walk]]\nfrom = \"S1\"\nto = \"nope\"\nseconds = 60\n",
            "nope",
        ),
        (
            "[[walk]]\nfrom = \"S1\"\nto = \"S2\"\n",
            "seconds or forbid",
        ),
        (
            "[[walk]]\nfrom = \"S1\"\nto = \"S2\"\nseconds = 60\nforbid = true\n",
            "seconds or forbid",
        ),
        (
            "[[walk]]\nfrom = \"S1\"\nto = \"S2\"\nseconds = 0\n",
            "seconds",
        ),
        (
            "[[walk]]\nfrom = \"S1\"\nto = \"S1\"\nseconds = 60\n",
            "itself",
        ),
        (
            "[[walk]]\nfrom = \"S1\"\nto = \"S2\"\nsecs = 60\n",
            "unknown field",
        ),
    ] {
        let err = build(text).unwrap_err();
        assert!(err.contains(needle), "{text:?}: {err}");
    }
}
