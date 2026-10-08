//! Section 10 test 11, fuzz style: mutated feeds go through the whole data
//! pipeline (load, profile, cluster, select, network) and must produce an
//! error or a result, never a panic. Runs in debug builds too, so integer
//! overflow panics would show. More cases: ALLSTOPS_FUZZ_CASES=5000.

use std::panic::{AssertUnwindSafe, catch_unwind};

use allstops_core::rules::Rules;
use allstops_gtfs::calendar::ServiceCalendar;
use allstops_gtfs::cluster::{ClusterConfig, cluster};
use allstops_gtfs::fixture::{minimal_files, zip_files};
use allstops_gtfs::inspect::profile;
use allstops_gtfs::network::build_network;
use allstops_gtfs::select::{Rule, Selection, select};
use allstops_gtfs::{Feed, Limits};
use proptest::prelude::*;

/// Values that have broken parsers before: empty, non-numeric, huge,
/// negative, out-of-range, special floats, odd times, every location type, a
/// bare byte-order mark and IDs that point back at existing rows.
const NASTY: &[&str] = &[
    "",
    "NaN",
    "inf",
    "-inf",
    "-1",
    "0",
    "1",
    "2",
    "3",
    "4",
    "1e308",
    "-1e308",
    "4294967295",
    "4294967296",
    "99999999999999999999",
    "24:00:00",
    "167:59:59",
    "168:00:01",
    "999:99:99",
    "00:00:-1",
    "\u{feff}",
    "S1",
    "S1a",
    "S2a",
    "T1",
    "R",
    "WD",
    "de:09162:1",
    "de:09162:1:1:1",
    "\"",
    "99991231",
    "00010101",
    "20261301",
    "Pacific/Apia",
    "Mars/Olympus",
];

#[derive(Debug, Clone)]
enum Op {
    /// Set field `col` of row `row` (1-based, so 0 is the header) to a value.
    Set {
        row: usize,
        col: usize,
        value: usize,
    },
    DuplicateRow(usize),
    DeleteRow(usize),
    /// Keep only the first `n` bytes of the file.
    Truncate(usize),
    /// Make one stop the parent of another, possibly forming a cycle.
    Parent {
        child: usize,
        parent: usize,
    },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (0usize..6, 0usize..10, 0..NASTY.len()).prop_map(|(row, col, value)| Op::Set { row, col, value }),
        1 => (0usize..6).prop_map(Op::DuplicateRow),
        1 => (1usize..6).prop_map(Op::DeleteRow),
        1 => (0usize..400).prop_map(Op::Truncate),
        1 => (1usize..5, 1usize..5).prop_map(|(child, parent)| Op::Parent { child, parent }),
    ]
}

fn apply(body: &str, op: &Op) -> String {
    let mut rows: Vec<Vec<String>> = body
        .lines()
        .map(|l| l.split(',').map(str::to_string).collect())
        .collect();
    match *op {
        Op::Set { row, col, value } => {
            if let Some(r) = rows.get_mut(row)
                && let Some(f) = r.get_mut(col)
            {
                *f = NASTY[value].to_string();
            }
        }
        Op::DuplicateRow(i) => {
            if let Some(r) = rows.get(i).cloned() {
                rows.push(r);
            }
        }
        Op::DeleteRow(i) => {
            if i < rows.len() {
                rows.remove(i);
            }
        }
        Op::Truncate(n) => {
            let joined = body.to_string();
            let mut cut = n.min(joined.len());
            while !joined.is_char_boundary(cut) {
                cut -= 1;
            }
            return joined[..cut].to_string();
        }
        Op::Parent { child, parent } => {
            // stops.txt columns: stop_id,...,parent_station (last).
            let parent_id = rows.get(parent).and_then(|r| r.first().cloned());
            if let (Some(pid), Some(r)) = (parent_id, rows.get_mut(child))
                && let Some(last) = r.last_mut()
            {
                *last = pid;
            }
        }
    }
    let mut out: String = rows
        .iter()
        .map(|r| r.join(","))
        .collect::<Vec<_>>()
        .join("\n");
    out.push('\n');
    out
}

/// The whole data pipeline; any error is fine, a panic is not.
fn pipeline(bytes: &[u8]) {
    let Ok(feed) = Feed::from_zip_bytes(bytes, &Limits::default()) else {
        return;
    };
    let _ = profile(&feed);
    let c = cluster(&feed, &ClusterConfig::default());
    assert_eq!(c.station_of_stop.len(), feed.stops.len());
    assert!(
        c.station_of_stop
            .iter()
            .all(|&s| (s as usize) < c.stations.len())
    );
    let sel = Selection {
        name: "fuzz".into(),
        include: vec![Rule {
            route_types: vec![1],
            ..Rule::default()
        }],
        exclude_stations: vec![],
    };
    let Ok(targets) = select(&feed, &c, &sel) else {
        return;
    };
    let cal = ServiceCalendar::new(&feed);
    let _ = build_network(
        &feed,
        &cal,
        &c,
        &targets,
        &[1..=1],
        &Rules::default(),
        &Default::default(),
    );
}

fn cases() -> u32 {
    std::env::var("ALLSTOPS_FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), .. ProptestConfig::default() })]

    #[test]
    fn mutated_feeds_never_panic(file in 0usize..6, ops in proptest::collection::vec(op(), 1..6)) {
        let mut files = minimal_files();
        let mut body = files[file].1.clone();
        for o in &ops {
            body = apply(&body, o);
        }
        files[file].1 = body;
        let refs: Vec<(&str, &str)> = files.iter().map(|(n, b)| (*n, b.as_str())).collect();
        let bytes = zip_files(&refs);
        let r = catch_unwind(AssertUnwindSafe(|| pipeline(&bytes)));
        prop_assert!(r.is_ok(), "panic on file {} after {:?}", files[file].0, ops);
    }
}

#[test]
fn hand_picked_hostile_feeds_never_panic() {
    let cases: &[(&str, &str)] = &[
        // Coordinates that are missing, infinite or out of range.
        (
            "stops.txt",
            "stop_id,stop_name,stop_lat,stop_lon\nS1a,One,,\nS2a,Two,,\n",
        ),
        // A parent cycle.
        (
            "stops.txt",
            "stop_id,stop_name,stop_lat,stop_lon,location_type,parent_station\n\
             S1a,One,48.1,11.5,0,S2a\nS2a,Two,48.1,11.5,0,S1a\n",
        ),
        // A stop_times row at a generic node without a parent.
        (
            "stops.txt",
            "stop_id,stop_name,stop_lat,stop_lon,location_type,parent_station\n\
             S1a,One,48.1,11.5,3,\nS2a,Two,48.11,11.51,0,\n",
        ),
        // Every stop at one point.
        (
            "stops.txt",
            "stop_id,stop_name,stop_lat,stop_lon\nS1a,A,48.1,11.5\nS2a,B,48.1,11.5\n",
        ),
        // A calendar over 10,000 years with no weekday set.
        (
            "calendar.txt",
            "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\n\
             WD,0,0,0,0,0,0,0,00010101,99991231\n",
        ),
        // A file that is only a byte-order mark.
        ("routes.txt", "\u{feff}"),
        // Times at the seven-day cap.
        (
            "stop_times.txt",
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
             T1,167:59:59,168:00:00,S1a,4294967295\nT1,168:00:00,168:00:00,S2a,0\n",
        ),
    ];
    for (file, body) in cases {
        let bytes = allstops_gtfs::fixture::minimal_with(&[(file, body)]);
        let r = catch_unwind(AssertUnwindSafe(|| pipeline(&bytes)));
        assert!(r.is_ok(), "panic on {file}: {body:?}");
    }
}
