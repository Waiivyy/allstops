//! frequencies.txt expansion (GTFS reference, frequencies.txt): a trip with
//! frequency rows is a template; it runs at `start_time`, then every
//! `headway_secs` while the departure is before `end_time`, with the
//! template's travel and dwell times.

use allstops_gtfs::fixture::minimal_with;
use allstops_gtfs::time::parse_time;
use allstops_gtfs::{Error, Feed, LimitKind, Limits};

fn t(s: &str) -> i32 {
    parse_time(s).unwrap()
}

/// T1 runs S1a 08:00 to S2a 08:05 (with a 1-minute dwell at S1a), and is
/// the template for the given frequencies.txt rows.
fn feed(frequencies: &str) -> Result<Feed, Error> {
    Feed::from_zip_bytes(
        &minimal_with(&[
            (
                "stop_times.txt",
                "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
                 T1,07:59:00,08:00:00,S1a,1\nT1,08:05:00,08:05:00,S2a,2\n",
            ),
            ("frequencies.txt", frequencies),
        ]),
        &Limits::default(),
    )
}

/// Expanded trips of T1 as (id, first arrival, first departure, last arrival).
fn runs(feed: &Feed) -> Vec<(String, i32, i32, i32)> {
    feed.trips
        .iter()
        .enumerate()
        .filter(|(_, tr)| !tr.frequency_template)
        .map(|(i, tr)| {
            let st = feed.trip_stop_times(i as u32);
            (tr.id.clone(), st[0].arrival, st[0].departure, st[1].arrival)
        })
        .collect()
}

const HEADER: &str = "trip_id,start_time,end_time,headway_secs,exact_times\n";

#[test]
fn schedule_based_rows_expand_to_exact_departures() {
    let feed = feed(&format!("{HEADER}T1,06:00:00,07:00:00,1200,1\n")).unwrap();
    let r = runs(&feed);
    let starts: Vec<i32> = r.iter().map(|x| x.2).collect();
    // 07:00 is the end, not a departure.
    assert_eq!(starts, vec![t("06:00:00"), t("06:20:00"), t("06:40:00")]);
    // Travel and dwell times are the template's.
    assert_eq!(r[0].1, t("05:59:00"));
    assert_eq!(r[0].3, t("06:05:00"));
    assert_eq!(r[0].0, "T1@06:00:00");
    // Every expanded run is schedule-based and knows its template.
    let template = feed.trip_index["T1"];
    assert_eq!(feed.trips[template as usize].template, None);
    for (i, tr) in feed.trips.iter().enumerate() {
        if !tr.frequency_template {
            assert_eq!(tr.frequency, Some(true), "trip {i}");
            assert_eq!(tr.template, Some(template), "trip {i}");
        }
    }
}

#[test]
fn the_template_itself_never_runs() {
    let feed = feed(&format!("{HEADER}T1,06:00:00,06:30:00,600,1\n")).unwrap();
    let template = feed.trip_index["T1"] as usize;
    assert!(feed.trips[template].frequency_template);
    assert!(runs(&feed).iter().all(|r| r.0 != "T1"));
    assert_eq!(runs(&feed).len(), 3);
}

#[test]
fn frequency_based_rows_expand_and_are_marked_approximate() {
    let feed = feed(&format!("{HEADER}T1,06:00:00,06:30:00,600,0\n")).unwrap();
    assert_eq!(runs(&feed).len(), 3);
    for tr in feed.trips.iter().filter(|t| !t.frequency_template) {
        assert_eq!(tr.frequency, Some(false), "exact_times = 0 is approximate");
    }
    // An empty exact_times means frequency-based too.
    let feed =
        self::feed("trip_id,start_time,end_time,headway_secs\nT1,06:00:00,06:30:00,600\n").unwrap();
    assert!(
        feed.trips
            .iter()
            .filter(|t| !t.frequency_template)
            .all(|t| t.frequency == Some(false))
    );
}

#[test]
fn several_rows_for_one_trip_follow_each_other() {
    let feed = feed(&format!(
        "{HEADER}T1,07:00:00,08:00:00,1800,1\nT1,06:00:00,07:00:00,3600,1\n"
    ))
    .unwrap();
    let starts: Vec<i32> = runs(&feed).iter().map(|x| x.2).collect();
    assert_eq!(starts, vec![t("06:00:00"), t("07:00:00"), t("07:30:00")]);
}

#[test]
fn trips_without_frequencies_are_untouched() {
    let feed = feed(HEADER).unwrap();
    assert_eq!(
        runs(&feed),
        vec![(
            "T1".to_string(),
            t("07:59:00"),
            t("08:00:00"),
            t("08:05:00")
        )]
    );
    assert_eq!(feed.trips[0].frequency, None);
}

#[test]
fn expansion_is_bounded_by_the_row_limit() {
    // One departure per second for a day would be 86,400 runs.
    let limits = Limits {
        max_rows_per_file: 10_000,
        ..Limits::default()
    };
    let bytes = minimal_with(&[(
        "frequencies.txt",
        "trip_id,start_time,end_time,headway_secs,exact_times\nT1,00:00:00,24:00:00,1,1\n",
    )]);
    let err = Feed::from_zip_bytes(&bytes, &limits).unwrap_err();
    assert!(
        matches!(err, Error::Limit(LimitKind::RowCount { .. })),
        "{err}"
    );
}

#[test]
fn bad_rows_are_errors() {
    for body in [
        "T1,06:00:00,07:00:00,0,1\n",
        "T1,06:00:00,07:00:00,-60,1\n",
        "T1,07:00:00,06:00:00,600,1\n",
        "T1,06:00:00,07:00:00,600,2\n",
    ] {
        assert!(
            feed(&format!("{HEADER}{body}")).is_err(),
            "{body:?} should be rejected"
        );
    }
    // Overlapping rows for one trip are not allowed by the reference.
    assert!(
        feed(&format!(
            "{HEADER}T1,06:00:00,07:00:00,600,1\nT1,06:30:00,08:00:00,600,1\n"
        ))
        .is_err()
    );
}
