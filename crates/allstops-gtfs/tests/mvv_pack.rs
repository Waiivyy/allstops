//! Stage 1 check-in criterion: the Munich U-Bahn pack builds byte-identically
//! twice from the real MVV feed, and the network built from it equals the
//! one built from the full feed. Needs the pinned feed in data/cache (run
//! `allstops fetch mvv`), so it is ignored by default:
//! `cargo test --release -p allstops-gtfs --test mvv_pack -- --ignored`.

use std::path::Path;

use allstops_core::rules::{Rules, route_types_for_mode};
use allstops_gtfs::calendar::ServiceCalendar;
use allstops_gtfs::cluster::{ClusterConfig, cluster};
use allstops_gtfs::network::build_network;
use allstops_gtfs::pack::{self, PackHeader, PackSource};
use allstops_gtfs::select::{Selection, select, visit_route_types};
use allstops_gtfs::walks::WalkOverrides;
use allstops_gtfs::{Feed, Limits};

fn data(rel: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data")
        .join(rel)
}

#[test]
#[ignore = "needs data/cache/mvv.gtfs.zip; run `allstops fetch mvv` first"]
fn munich_pack_is_deterministic_and_plans_like_the_feed() {
    let rules: Rules =
        toml::from_str(&std::fs::read_to_string(data("rules/mvv-ubahn.toml")).unwrap()).unwrap();
    let selection: Selection =
        toml::from_str(&std::fs::read_to_string(data("selections/mvv-ubahn.toml")).unwrap())
            .unwrap();
    let mut connector_types = Vec::new();
    for m in &rules.connector_modes {
        connector_types.extend(route_types_for_mode(m).unwrap());
    }
    let walks = WalkOverrides::default();

    let zip = std::fs::read(data("cache/mvv.gtfs.zip")).expect("pinned MVV feed in data/cache");
    let build = || {
        // Load everything afresh each time, so nothing carries over.
        let feed = Feed::from_zip_bytes(&zip, &Limits::default()).unwrap();
        let c = cluster(&feed, &ClusterConfig::default());
        let targets = select(&feed, &c, &selection).unwrap();
        let visit_types = visit_route_types(&feed, &selection).unwrap();
        let bytes = pack::build(
            &PackSource {
                zip: &zip,
                feed: &feed,
                clustering: &c,
                targets: &targets,
                visit_types: &visit_types,
                connector_types: &connector_types,
                walks: &walks,
                header: PackHeader {
                    feed_id: "mvv".into(),
                    ..PackHeader::default()
                },
            },
            &Limits::default(),
        )
        .unwrap();
        (feed, c, targets, visit_types, bytes)
    };
    let (feed, c, targets, visit_types, first) = build();
    let (_, _, _, _, second) = build();
    assert!(first == second, "two builds differ");

    let p = pack::read(&first, &Limits::default()).unwrap();
    assert_eq!(p.targets.len(), 96);
    let types: Vec<_> = visit_types.iter().map(|&t| t..=t).collect();
    for date in ["2026-11-12", "2026-11-14"] {
        let rules = Rules {
            date: date.into(),
            ..rules.clone()
        };
        let from_feed = build_network(
            &feed,
            &ServiceCalendar::new(&feed),
            &c,
            &targets,
            &types,
            &rules,
            &walks,
        )
        .unwrap()
        .0;
        let from_pack = build_network(
            &p.feed,
            &ServiceCalendar::new(&p.feed),
            &p.clustering,
            &p.targets,
            &types,
            &rules,
            &p.walks,
        )
        .unwrap()
        .0;
        assert!(
            postcard::to_allocvec(&from_feed).unwrap()
                == postcard::to_allocvec(&from_pack).unwrap(),
            "{date}: networks differ"
        );
    }
}
