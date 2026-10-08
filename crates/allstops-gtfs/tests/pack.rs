//! Section 10 test 9, pack determinism and versioning: the same input gives
//! the same bytes, a pack of another format version is rejected with a clear
//! message, and a network built from a pack equals one built from the feed.

use allstops_core::rules::Rules;
use allstops_gtfs::calendar::ServiceCalendar;
use allstops_gtfs::cluster::{ClusterConfig, Clustering, cluster};
use allstops_gtfs::fixture::minimal_with;
use allstops_gtfs::network::build_network;
use allstops_gtfs::pack::{self, FORMAT_VERSION, PackHeader, PackSource};
use allstops_gtfs::select::{Rule, Selection, select};
use allstops_gtfs::walks::WalkOverrides;
use allstops_gtfs::{Feed, Limits};

/// A U-Bahn line (S1 to S2), a tram (S2 to S3) with a frequency-based
/// variant, and a bus (S3 to S4) on its own service. Station S2 has a
/// U-Bahn platform, a tram platform and an entrance.
fn zip() -> Vec<u8> {
    minimal_with(&[
        (
            "stops.txt",
            "stop_id,stop_name,stop_lat,stop_lon,location_type,parent_station\n\
             S1,One,48.100,11.500,1,\nS1a,One,48.100,11.500,0,S1\n\
             S2,Two,48.105,11.500,1,\nS2a,Two U,48.105,11.500,0,S2\nS2b,Two Tram,48.1051,11.5001,0,S2\nS2e,Two exit,48.1052,11.5002,2,S2\n\
             S3,Three,48.110,11.500,1,\nS3a,Three,48.110,11.500,0,S3\n\
             S4,Four,48.115,11.500,1,\nS4a,Four,48.115,11.500,0,S4\n",
        ),
        (
            "routes.txt",
            "route_id,agency_id,route_short_name,route_long_name,route_type\n\
             R,A,U1,,1\nTR,A,17,,0\nB,A,100,,3\n",
        ),
        (
            "trips.txt",
            "route_id,service_id,trip_id,trip_headsign\n\
             R,WD,T1,Two\nTR,WD,T2,Three\nB,BUS,T3,Four\nTR,WD,TF,Three\n",
        ),
        (
            "stop_times.txt",
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
             T1,08:00:00,08:00:00,S1a,1\nT1,08:05:00,08:05:00,S2a,2\n\
             T2,08:10:00,08:10:00,S2b,1\nT2,08:15:00,08:15:00,S3a,2\n\
             T3,08:20:00,08:20:00,S3a,1\nT3,08:25:00,08:25:00,S4a,2\n\
             TF,06:00:00,06:00:00,S2b,1\nTF,06:05:00,06:05:00,S3a,2\n",
        ),
        (
            "calendar.txt",
            "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\n\
             WD,1,1,1,1,1,0,0,20261001,20261213\nBUS,1,1,1,1,1,1,1,20261001,20261213\n",
        ),
        (
            "calendar_dates.txt",
            "service_id,date,exception_type\nWD,20261114,1\nBUS,20261112,2\n",
        ),
        (
            "frequencies.txt",
            "trip_id,start_time,end_time,headway_secs,exact_times\nTF,06:00:00,07:00:00,1200,1\n",
        ),
        (
            "transfers.txt",
            "from_stop_id,to_stop_id,transfer_type,min_transfer_time\n\
             S2a,S2b,2,180\nS3a,S4a,2,240\n",
        ),
        (
            "feed_info.txt",
            "feed_publisher_name,feed_publisher_url,feed_lang,feed_version\nTest,https://example.org,de,v1\n",
        ),
        (
            "shapes.txt",
            "shape_id,shape_pt_lat,shape_pt_lon,shape_pt_sequence\nX,48.1,11.5,1\n",
        ),
    ])
}

struct Input {
    zip: Vec<u8>,
    feed: Feed,
    clustering: Clustering,
    targets: Vec<u32>,
}

fn input() -> Input {
    let zip = zip();
    let feed = Feed::from_zip_bytes(&zip, &Limits::default()).unwrap();
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
    Input {
        zip,
        feed,
        clustering,
        targets,
    }
}

fn header() -> PackHeader {
    PackHeader {
        feed_id: "test".into(),
        feed_sha256: "00".repeat(32),
        attribution: "Timetable data: Test".into(),
        timezone: "Europe/Berlin".into(),
        selection_name: "U".into(),
        connector_modes: vec!["tram".into()],
        ..PackHeader::default()
    }
}

fn build_with(walks: &WalkOverrides) -> Vec<u8> {
    let i = input();
    pack::build(
        &PackSource {
            zip: &i.zip,
            feed: &i.feed,
            clustering: &i.clustering,
            targets: &i.targets,
            visit_types: &[1],
            connector_types: &[0..=0],
            walks,
            header: header(),
        },
        &Limits::default(),
    )
    .unwrap()
}

fn build() -> Vec<u8> {
    build_with(&WalkOverrides::default())
}

#[test]
fn building_twice_gives_identical_bytes() {
    let a = build();
    let b = build();
    assert_eq!(a, b);
    assert!(a.starts_with(pack::MAGIC));
}

#[test]
fn the_pack_keeps_what_plans_can_use_and_drops_the_rest() {
    let p = pack::read(&build(), &Limits::default()).unwrap();
    let trips: Vec<&str> = p.feed.trips.iter().map(|t| t.id.as_str()).collect();
    assert!(trips.contains(&"T1") && trips.contains(&"T2"), "{trips:?}");
    assert!(
        trips.contains(&"TF@06:20:00"),
        "frequencies kept: {trips:?}"
    );
    assert!(
        !trips.contains(&"T3"),
        "the bus is not a connector: {trips:?}"
    );
    let stops: Vec<&str> = p.feed.stops.iter().map(|s| s.id.as_str()).collect();
    for kept in ["S1", "S1a", "S2", "S2a", "S2b", "S2e", "S3", "S3a"] {
        assert!(stops.contains(&kept), "{kept} missing from {stops:?}");
    }
    assert!(
        !stops.contains(&"S4a") && !stops.contains(&"S4"),
        "{stops:?}"
    );
    assert_eq!(p.feed.service_ids, vec!["WD".to_string()]);
    assert_eq!(p.feed.calendar_dates.len(), 1);
    assert_eq!(
        p.feed.transfers.len(),
        1,
        "only the transfer between kept stops"
    );
    assert!(!p.feed.files.iter().any(|f| f == "shapes.txt"));
    assert_eq!(p.feed.feed_info.as_ref().unwrap().version, "v1");
    // Stations and targets come back as built.
    let ids: Vec<&str> = p
        .targets
        .iter()
        .map(|&t| p.clustering.stations[t as usize].id.as_str())
        .collect();
    assert_eq!(ids, vec!["S1", "S2"]);
    assert_eq!(p.visit_types, vec![1]);
    for (stop, &st) in p.feed.stops.iter().zip(&p.clustering.station_of_stop) {
        let s = &p.clustering.stations[st as usize];
        assert!(
            s.members.iter().any(|m| m.stop_id == stop.id),
            "{} not in {}",
            stop.id,
            s.id
        );
    }
    assert_eq!(p.header.counts.trips, p.feed.trips.len());
    assert_eq!(p.header.counts.stations, p.clustering.stations.len());
}

#[test]
fn a_network_from_the_pack_equals_one_from_the_feed() {
    let walks: WalkOverrides = toml::from_str(
        "[[walk]]\nfrom = \"S2\"\nto = \"S3\"\nseconds = 300\n[[walk]]\nfrom = \"S3\"\nto = \"S4\"\nforbid = true\n",
    )
    .unwrap();
    let i = input();
    let bytes = build_with(&walks);
    let p = pack::read(&bytes, &Limits::default()).unwrap();
    for date in ["2026-11-12", "2026-11-14"] {
        let rules = Rules {
            date: date.into(),
            connector_modes: vec!["tram".into()],
            ..Rules::default()
        };
        let from_feed = build_network(
            &i.feed,
            &ServiceCalendar::new(&i.feed),
            &i.clustering,
            &i.targets,
            &[1..=1],
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
            &[1..=1],
            &rules,
            &p.walks,
        )
        .unwrap()
        .0;
        assert_eq!(
            postcard::to_allocvec(&from_feed).unwrap(),
            postcard::to_allocvec(&from_pack).unwrap(),
            "{date}"
        );
        assert!(!from_pack.footpaths.is_empty());
    }
    // The walk to a station outside the pack is dropped; the other is kept.
    assert_eq!(p.walks.walk.len(), 1);
}

#[test]
fn a_different_format_version_is_rejected_clearly() {
    let mut bytes = build();
    let at = pack::MAGIC.len();
    bytes[at..at + 4].copy_from_slice(&(FORMAT_VERSION + 1).to_le_bytes());
    for err in [
        pack::read(&bytes, &Limits::default())
            .unwrap_err()
            .to_string(),
        pack::read_header(&bytes).unwrap_err().to_string(),
    ] {
        assert!(
            err.contains(&format!("format version {}", FORMAT_VERSION + 1))
                && err.contains(&format!("version {FORMAT_VERSION}"))
                && err.contains("allstops pack"),
            "{err}"
        );
    }
}

#[test]
fn damage_and_other_files_are_errors_not_panics() {
    let good = build();
    let mut damaged = good.clone();
    let mid = damaged.len() / 2;
    damaged[mid] ^= 0x55;
    let err = pack::read(&damaged, &Limits::default())
        .unwrap_err()
        .to_string();
    assert!(err.contains("checksum"), "{err}");

    let err = pack::read(&zip(), &Limits::default())
        .unwrap_err()
        .to_string();
    assert!(err.contains("not an allstops pack"), "{err}");

    for cut in [
        0,
        5,
        pack::MAGIC.len() + 2,
        pack::MAGIC.len() + 4,
        100,
        good.len() - 1,
    ] {
        assert!(
            pack::read(&good[..cut], &Limits::default()).is_err(),
            "cut at {cut}"
        );
    }
    assert!(pack::is_pack(&good));
    assert!(!pack::is_pack(&zip()));
}

#[test]
fn the_header_is_readable_on_its_own() {
    let bytes = build();
    let h = pack::read_header(&bytes).unwrap();
    assert_eq!(h.feed_id, "test");
    assert_eq!(h.connector_modes, vec!["tram".to_string()]);
    assert_eq!(h.counts.targets, 2);
    assert!(h.counts.stop_times > 0);
}

#[test]
fn packs_respect_the_size_limit() {
    let bytes = build();
    let limits = Limits {
        max_compressed_bytes: (bytes.len() - 1) as u64,
        ..Limits::default()
    };
    assert!(pack::read(&bytes, &limits).is_err());
}
