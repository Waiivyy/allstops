//! Section 10 test 2, overrides: stations.overrides.toml merges, splits and
//! renames stations after automatic clustering.

use allstops_gtfs::cluster::{
    ClusterConfig, MergeReason, StationOverrides, apply_overrides, cluster,
};
use allstops_gtfs::fixture::minimal_with;
use allstops_gtfs::{Feed, Limits};

/// Stations P (two platforms), Q and R, each with platforms, plus the
/// fixture's S1 and S2.
fn feed() -> Feed {
    Feed::from_zip_bytes(
        &minimal_with(&[(
            "stops.txt",
            "stop_id,stop_name,stop_lat,stop_lon,location_type,parent_station\n\
             P,Plaza,48.10,11.50,1,\nP1,Plaza 1,48.10,11.50,0,P\nP2,Plaza 2,48.1001,11.5001,0,P\n\
             Q,Quay,48.11,11.51,1,\nQ1,Quay 1,48.11,11.51,0,Q\n\
             R,Ring,48.12,11.52,1,\nR1,Ring 1,48.12,11.52,0,R\n\
             S1,One,48.13,11.53,1,\nS1a,One,48.13,11.53,0,S1\nS2,Two,48.14,11.54,1,\nS2a,Two,48.14,11.54,0,S2\n",
        )]),
        &Limits::default(),
    )
    .unwrap()
}

fn parse(toml_text: &str) -> StationOverrides {
    toml::from_str(toml_text).expect("overrides parse")
}

fn station_of<'c>(c: &'c allstops_gtfs::cluster::Clustering, feed: &Feed, stop: &str) -> &'c str {
    let i = c.station_of_stop[feed.stop_index[stop] as usize];
    &c.stations[i as usize].id
}

#[test]
fn merge_joins_stations_and_records_why() {
    let f = feed();
    let base = cluster(&f, &ClusterConfig::default());
    let ov = parse(
        r#"
[[merge]]
stations = ["P", "Q"]
name = "Plaza and Quay"
"#,
    );
    let c = apply_overrides(&f, base.clone(), &ov).unwrap();
    assert_eq!(c.stations.len(), base.stations.len() - 1);
    assert_eq!(station_of(&c, &f, "Q1"), "P");
    let p = c.stations.iter().find(|s| s.id == "P").unwrap();
    assert_eq!(p.name, "Plaza and Quay");
    assert_eq!(p.members.len(), 5);
    let q1 = p.members.iter().find(|m| m.stop_id == "Q1").unwrap();
    assert_eq!(q1.reason, MergeReason::Override);
    assert!(c.stations.iter().all(|s| s.id != "Q"));
}

#[test]
fn merge_can_name_the_new_station() {
    let f = feed();
    let ov = parse("[[merge]]\nstations = [\"P\", \"Q\", \"R\"]\nid = \"PQR\"\n");
    let c = apply_overrides(&f, cluster(&f, &ClusterConfig::default()), &ov).unwrap();
    for stop in ["P1", "Q1", "R1"] {
        assert_eq!(station_of(&c, &f, stop), "PQR");
    }
}

#[test]
fn split_moves_stops_into_a_new_station() {
    let f = feed();
    let ov = parse(
        r#"
[[split]]
station = "P"
stops = ["P2"]
id = "P-east"
name = "Plaza East"
"#,
    );
    let c = apply_overrides(&f, cluster(&f, &ClusterConfig::default()), &ov).unwrap();
    assert_eq!(station_of(&c, &f, "P2"), "P-east");
    assert_eq!(station_of(&c, &f, "P1"), "P");
    let east = c.stations.iter().find(|s| s.id == "P-east").unwrap();
    assert_eq!(east.name, "Plaza East");
    assert!((east.lat - 48.1001).abs() < 1e-9, "positioned at its stops");
    assert_eq!(east.members[0].reason, MergeReason::Override);
}

#[test]
fn rename_changes_only_the_name() {
    let f = feed();
    let ov = parse("[[rename]]\nstation = \"R\"\nname = \"Ring Road\"\n");
    let c = apply_overrides(&f, cluster(&f, &ClusterConfig::default()), &ov).unwrap();
    let r = c.stations.iter().find(|s| s.id == "R").unwrap();
    assert_eq!(r.name, "Ring Road");
}

#[test]
fn split_then_merge_then_rename_in_that_order() {
    let f = feed();
    let ov = parse(
        r#"
[[rename]]
station = "P-east"
name = "East"

[[merge]]
stations = ["P-east", "Q"]

[[split]]
station = "P"
stops = ["P2"]
id = "P-east"
"#,
    );
    let c = apply_overrides(&f, cluster(&f, &ClusterConfig::default()), &ov).unwrap();
    assert_eq!(station_of(&c, &f, "Q1"), "P-east");
    assert_eq!(station_of(&c, &f, "P2"), "P-east");
    assert_eq!(
        c.stations.iter().find(|s| s.id == "P-east").unwrap().name,
        "East"
    );
}

#[test]
fn mistakes_are_errors() {
    let f = feed();
    let base = cluster(&f, &ClusterConfig::default());
    let cases: &[(&str, &str)] = &[
        ("[[merge]]\nstations = [\"P\", \"nope\"]\n", "nope"),
        ("[[merge]]\nstations = [\"P\"]\n", "at least two"),
        ("[[merge]]\nstations = [\"P\", \"Q\"]\nid = \"R\"\n", "R"),
        (
            "[[split]]\nstation = \"P\"\nstops = [\"Q1\"]\nid = \"X\"\n",
            "Q1",
        ),
        (
            "[[split]]\nstation = \"P\"\nstops = [\"P\", \"P1\", \"P2\"]\nid = \"X\"\n",
            "empty",
        ),
        (
            "[[split]]\nstation = \"P\"\nstops = [\"P2\"]\nid = \"Q\"\n",
            "Q",
        ),
        ("[[rename]]\nstation = \"nope\"\nname = \"x\"\n", "nope"),
        (
            "[[split]]\nstation = \"P\"\nstops = []\nid = \"X\"\n",
            "no stops",
        ),
    ];
    for (text, needle) in cases {
        let err = apply_overrides(&f, base.clone(), &parse(text))
            .unwrap_err()
            .to_string();
        assert!(err.contains(needle), "{text:?}: {err}");
    }
    assert!(
        toml::from_str::<StationOverrides>("[[merge]]\nstation = [\"P\"]\n").is_err(),
        "unknown field"
    );
}

#[test]
fn every_stop_still_has_a_station_after_overrides() {
    let f = feed();
    let ov = parse(
        "[[merge]]\nstations = [\"P\", \"Q\"]\n[[split]]\nstation = \"R\"\nstops = [\"R1\"]\nid = \"R2\"\n",
    );
    let c = apply_overrides(&f, cluster(&f, &ClusterConfig::default()), &ov).unwrap();
    assert!(
        c.station_of_stop
            .iter()
            .all(|&s| (s as usize) < c.stations.len())
    );
    for (i, s) in c.stations.iter().enumerate() {
        for m in &s.members {
            assert_eq!(c.station_of_stop[m.stop as usize] as usize, i);
        }
    }
}
