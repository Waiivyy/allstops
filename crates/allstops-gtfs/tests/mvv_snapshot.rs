//! Snapshot of the Munich U-Bahn target stations from the real MVV feed.
//! Needs the pinned feed in data/cache (run `allstops fetch mvv`), so it is
//! ignored by default: `cargo test -p allstops-gtfs -- --ignored`.

use std::path::Path;

use allstops_gtfs::cluster::{ClusterConfig, cluster};
use allstops_gtfs::select::{Rule, Selection, select};
use allstops_gtfs::{Feed, Limits};

const SNAPSHOT: &str = include_str!("snapshots/mvv-ubahn-stations.tsv");

/// The agreed station count for the Munich U-Bahn: every station counted
/// once, however many lines or platform halls it has.
const MUNICH_UBAHN_STATIONS: usize = 96;

#[test]
#[ignore = "needs data/cache/mvv.gtfs.zip; run `allstops fetch mvv` first"]
fn munich_ubahn_station_list_matches_snapshot() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/cache/mvv.gtfs.zip");
    let bytes = std::fs::read(&path).expect("pinned MVV feed in data/cache");
    let feed = Feed::from_zip_bytes(&bytes, &Limits::default()).unwrap();
    let c = cluster(&feed, &ClusterConfig::default());
    let sel = Selection {
        name: "Munich U-Bahn".into(),
        include: vec![Rule {
            route_types: vec![1],
            ..Rule::default()
        }],
        exclude_stations: vec![],
    };
    let ids = select(&feed, &c, &sel).unwrap();
    let mut got: Vec<String> = ids
        .iter()
        .map(|&i| {
            let s = &c.stations[i as usize];
            format!("{}\t{}", s.id, s.name)
        })
        .collect();
    got.sort();
    let want: Vec<&str> = SNAPSHOT.lines().filter(|l| !l.starts_with('#')).collect();
    assert_eq!(got.len(), MUNICH_UBAHN_STATIONS);
    assert_eq!(got, want);
}
