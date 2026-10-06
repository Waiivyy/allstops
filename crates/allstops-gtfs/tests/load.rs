//! Section 10 tests 1 (service days and time) and 11 (untrusted input).

use std::io::{Cursor, Write};

use allstops_gtfs::calendar::{ServiceCalendar, check_plan_date, day_offset, service_day_origin};
use allstops_gtfs::fixture::{minimal_with, zip_files};
use allstops_gtfs::{Error, Feed, LimitKind, Limits};
use chrono::{NaiveDate, TimeZone, Timelike};
use chrono_tz::Europe::Berlin;

fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

fn load(bytes: &[u8]) -> Result<Feed, Error> {
    Feed::from_zip_bytes(bytes, &Limits::default())
}

// ---- 1. Service days and time ------------------------------------------

#[test]
fn loads_minimal_feed() {
    let feed = load(&minimal_with(&[])).unwrap();
    assert_eq!(feed.stops.len(), 4);
    assert_eq!(feed.trips.len(), 1);
    assert_eq!(feed.trip_stop_times(0).len(), 2);
    assert_eq!(
        feed.root_stop(feed.stop_index["S1a"]),
        feed.stop_index["S1"]
    );
}

#[test]
fn trips_past_midnight_keep_their_service_day() {
    let feed = load(&minimal_with(&[(
        "stop_times.txt",
        "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
         T1,23:55:00,23:55:00,S1a,1\nT1,24:05:30,24:05:30,S2a,2\n",
    )]))
    .unwrap();
    let st = feed.trip_stop_times(0);
    assert_eq!(st[1].arrival, 24 * 3600 + 5 * 60 + 30);
    // Friday 2026-10-09 service, arriving after midnight: the absolute time
    // is early Saturday morning, counted from Friday's origin.
    let origin = service_day_origin(&Berlin, date("2026-10-09"));
    let abs = Berlin
        .timestamp_opt(origin + st[1].arrival as i64, 0)
        .unwrap();
    assert_eq!(abs.date_naive(), date("2026-10-10"));
    assert_eq!((abs.hour(), abs.minute(), abs.second()), (0, 5, 30));
}

#[test]
fn calendar_dates_add_and_remove_service() {
    let feed = load(&minimal_with(&[(
        "calendar_dates.txt",
        "service_id,date,exception_type\nWD,20261012,2\nWD,20261017,1\nEXTRA,20261018,1\n",
    )]))
    .unwrap();
    let cal = ServiceCalendar::new(&feed);
    let wd = feed.trips[0].service;
    assert!(cal.is_active(wd, date("2026-10-13")), "ordinary Tuesday");
    assert!(!cal.is_active(wd, date("2026-10-12")), "removed Monday");
    assert!(cal.is_active(wd, date("2026-10-17")), "added Saturday");
    assert!(!cal.is_active(wd, date("2026-10-18")), "ordinary Sunday");
    assert!(!cal.is_active(wd, date("2026-12-14")), "after end_date");
}

#[test]
fn calendar_dates_only_feed_is_accepted() {
    let bytes = {
        let mut files = allstops_gtfs::fixture::minimal_files();
        files.retain(|(n, _)| *n != "calendar.txt");
        files.push((
            "calendar_dates.txt",
            "service_id,date,exception_type\nWD,20261014,1\n".into(),
        ));
        let refs: Vec<(&str, &str)> = files.iter().map(|(n, b)| (*n, b.as_str())).collect();
        zip_files(&refs)
    };
    let feed = load(&bytes).unwrap();
    let cal = ServiceCalendar::new(&feed);
    assert_eq!(
        cal.service_range(),
        Some((date("2026-10-14"), date("2026-10-14")))
    );
}

#[test]
fn daylight_saving_days_use_noon_minus_twelve_hours() {
    // Autumn change in Europe/Berlin: 2026-10-25, clocks go back at 03:00.
    // The day is 25 hours long; noon minus 12 h is 01:00 local (CEST).
    let o = service_day_origin(&Berlin, date("2026-10-25"));
    let at = |secs: i64| Berlin.timestamp_opt(o + secs, 0).unwrap();
    assert_eq!(at(0).hour(), 1);
    // A trip at 05:00:00 on that service day departs at 05:00 wall time
    // (CET), because 05:00 is past the change.
    assert_eq!((at(5 * 3600).hour(), at(5 * 3600).minute()), (5, 0));
    // Spring change: 2026-03-29, clocks jump from 02:00 to 03:00. Noon minus
    // 12 h is 23:00 on the previous day.
    let o = service_day_origin(&Berlin, date("2026-03-29"));
    let at = |secs: i64| Berlin.timestamp_opt(o + secs, 0).unwrap();
    assert_eq!(at(0).date_naive(), date("2026-03-28"));
    assert_eq!(at(0).hour(), 23);
    assert_eq!(at(8 * 3600).hour(), 8, "08:00 lands at 08:00 CEST");
    // Day offsets between neighbouring service days.
    // Origins are noon minus 12 h, so the 25-hour wall-clock day sits
    // between the origins of 24 and 25 October.
    assert_eq!(
        day_offset(&Berlin, date("2026-10-25"), date("2026-10-24")),
        -25 * 3600
    );
    assert_eq!(
        day_offset(&Berlin, date("2026-10-26"), date("2026-10-25")),
        -24 * 3600
    );
    assert_eq!(
        day_offset(&Berlin, date("2026-03-29"), date("2026-03-28")),
        -23 * 3600
    );
    assert_eq!(
        day_offset(&Berlin, date("2026-11-14"), date("2026-11-13")),
        -24 * 3600
    );
}

#[test]
fn absolute_wall_clock_on_autumn_change() {
    // 05:00:00 on 2026-10-25 is 05:00 CET = 04:00 UTC.
    let o = service_day_origin(&Berlin, date("2026-10-25"));
    let utc = chrono::DateTime::from_timestamp(o + 5 * 3600, 0).unwrap();
    assert_eq!(utc.hour(), 4);
    let local = utc.with_timezone(&Berlin);
    assert_eq!(local.hour(), 5);
}

#[test]
fn plan_date_outside_validity_shows_range() {
    let feed = load(&minimal_with(&[])).unwrap();
    let cal = ServiceCalendar::new(&feed);
    assert!(check_plan_date(&feed, &cal, date("2026-11-14")).is_ok());
    let err = check_plan_date(&feed, &cal, date("2027-01-05")).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("2026-10-01") && msg.contains("2026-12-11"),
        "{msg}"
    );
}

#[test]
fn feed_info_range_wins_when_present() {
    let feed = load(&minimal_with(&[(
        "feed_info.txt",
        "feed_publisher_name,feed_publisher_url,feed_lang,feed_start_date,feed_end_date\n\
         P,https://example.org,de,20261005,20261130\n",
    )]))
    .unwrap();
    let cal = ServiceCalendar::new(&feed);
    let err = check_plan_date(&feed, &cal, date("2026-12-01")).unwrap_err();
    assert!(err.to_string().contains("2026-11-30"), "{err}");
}

#[test]
fn omitted_times_are_interpolated() {
    let feed = load(&minimal_with(&[
        (
            "stops.txt",
            "stop_id,stop_name,stop_lat,stop_lon\nA,A,48.1,11.5\nB,B,48.1,11.6\nC,C,48.1,11.7\nD,D,48.1,11.8\n",
        ),
        (
            "stop_times.txt",
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
             T1,08:00:00,08:00:00,A,1\nT1,,,B,2\nT1,,,C,3\nT1,08:09:00,08:10:00,D,4\n",
        ),
    ]))
    .unwrap();
    let st = feed.trip_stop_times(0);
    assert_eq!(st[1].arrival, 8 * 3600 + 3 * 60);
    assert_eq!(st[2].arrival, 8 * 3600 + 6 * 60);
    assert_eq!(feed.warnings.stop_times_interpolated, 2);
}

#[test]
fn stop_times_are_ordered_by_sequence_not_file_order() {
    let feed = load(&minimal_with(&[(
        "stop_times.txt",
        "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
         T1,08:05:00,08:05:00,S2a,20\nT1,08:00:00,08:00:00,S1a,10\n",
    )]))
    .unwrap();
    let st = feed.trip_stop_times(0);
    assert_eq!(st[0].sequence, 10);
    assert_eq!(st[1].sequence, 20);
}

// ---- 11. Untrusted input -------------------------------------------------

#[test]
fn truncated_zip_is_an_error() {
    let bytes = minimal_with(&[]);
    for cut in [0, 10, bytes.len() / 2, bytes.len() - 1] {
        assert!(load(&bytes[..cut]).is_err(), "cut at {cut}");
    }
}

#[test]
fn random_bytes_are_an_error() {
    let junk: Vec<u8> = (0..4096u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
        .collect();
    assert!(matches!(load(&junk), Err(Error::Zip(_))));
}

#[test]
fn zip_bomb_hits_uncompressed_limit() {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    for (name, body) in allstops_gtfs::fixture::minimal_files() {
        if name == "stop_times.txt" {
            continue;
        }
        w.start_file(name, opts).unwrap();
        w.write_all(body.as_bytes()).unwrap();
    }
    w.start_file("stop_times.txt", opts).unwrap();
    w.write_all(b"trip_id,arrival_time,departure_time,stop_id,stop_sequence\n")
        .unwrap();
    let line = b"T1,08:00:00,08:00:00,S1a,1\n";
    for _ in 0..(8 << 20) / line.len() {
        w.write_all(line).unwrap();
    }
    let bytes = w.finish().unwrap().into_inner();
    assert!(bytes.len() < 200_000, "compresses well: {}", bytes.len());

    let limits = Limits {
        max_uncompressed_bytes: 1 << 20,
        ..Limits::default()
    };
    let err = Feed::from_zip_bytes(&bytes, &limits).unwrap_err();
    assert!(
        matches!(err, Error::Limit(LimitKind::UncompressedSize { .. })),
        "{err}"
    );
}

#[test]
fn compressed_size_and_entry_limits_hold() {
    let bytes = minimal_with(&[]);
    let small = Limits {
        max_compressed_bytes: 100,
        ..Limits::default()
    };
    assert!(matches!(
        Feed::from_zip_bytes(&bytes, &small),
        Err(Error::Limit(LimitKind::CompressedSize { .. }))
    ));
    let few = Limits {
        max_entries: 3,
        ..Limits::default()
    };
    assert!(matches!(
        Feed::from_zip_bytes(&bytes, &few),
        Err(Error::Limit(LimitKind::EntryCount { .. }))
    ));
}

#[test]
fn oversized_line_hits_limit() {
    let long_name = "x".repeat(100_000);
    let stops = format!(
        "stop_id,stop_name,stop_lat,stop_lon\nS1a,{long_name},48.1,11.5\nS2a,B,48.1,11.6\n"
    );
    let err = load(&minimal_with(&[("stops.txt", &stops)])).unwrap_err();
    assert!(
        matches!(err, Error::Limit(LimitKind::LineLength { .. })),
        "{err}"
    );
}

#[test]
fn row_limit_holds() {
    let limits = Limits {
        max_rows_per_file: 1,
        ..Limits::default()
    };
    let err = Feed::from_zip_bytes(&minimal_with(&[]), &limits).unwrap_err();
    assert!(
        matches!(err, Error::Limit(LimitKind::RowCount { .. })),
        "{err}"
    );
}

#[test]
fn invalid_utf8_is_an_error() {
    let mut files = allstops_gtfs::fixture::minimal_files();
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    for (name, body) in files.drain(..) {
        w.start_file(name, opts).unwrap();
        if name == "stops.txt" {
            w.write_all(
                b"stop_id,stop_name,stop_lat,stop_lon\nS1a,\xff\xfe,48.1,11.5\nS2a,B,48.1,11.6\n",
            )
            .unwrap();
        } else {
            w.write_all(body.as_bytes()).unwrap();
        }
    }
    let bytes = w.finish().unwrap().into_inner();
    let err = load(&bytes).unwrap_err();
    assert!(err.to_string().contains("UTF-8"), "{err}");
}

#[test]
fn missing_required_column_is_named() {
    let err = load(&minimal_with(&[(
        "stop_times.txt",
        "trip_id,arrival_time,departure_time,stop_sequence\nT1,08:00:00,08:00:00,1\n",
    )]))
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "stop_times.txt: missing required column stop_id"
    );
}

#[test]
fn missing_required_file_is_named() {
    let mut files = allstops_gtfs::fixture::minimal_files();
    files.retain(|(n, _)| *n != "trips.txt");
    let refs: Vec<(&str, &str)> = files.iter().map(|(n, b)| (*n, b.as_str())).collect();
    let err = load(&zip_files(&refs)).unwrap_err();
    assert_eq!(err.to_string(), "missing required file trips.txt");
}

#[test]
fn negative_and_malformed_times_are_errors() {
    for bad in ["-01:00:00", "8:00", "08:61:00", "xx:00:00"] {
        let st = format!(
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence\nT1,{bad},{bad},S1a,1\nT1,08:05:00,08:05:00,S2a,2\n"
        );
        let err = load(&minimal_with(&[("stop_times.txt", &st)])).unwrap_err();
        assert!(
            err.to_string().contains("stop_times.txt line 2"),
            "{bad}: {err}"
        );
    }
}

#[test]
fn malformed_values_are_errors_not_panics() {
    let cases: &[(&str, &str)] = &[
        (
            "stops.txt",
            "stop_id,stop_name,stop_lat,stop_lon\nS1a,A,abc,11.5\n",
        ),
        (
            "stops.txt",
            "stop_id,stop_name,stop_lat,stop_lon\nS1a,A,95.0,11.5\n",
        ),
        (
            "stops.txt",
            "stop_id,stop_name,stop_lat,stop_lon,location_type\nS1a,A,48,11,9\n",
        ),
        ("stops.txt", "stop_id,stop_name\nS1a,A\nS1a,B\n"),
        ("routes.txt", "route_id,agency_id,route_type\nR,A,metro\n"),
        (
            "calendar.txt",
            "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\nWD,1,1,1,1,1,0,0,2026-10-01,20261213\n",
        ),
        (
            "stop_times.txt",
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence\nT1,08:00:00,08:00:00,S1a,one\n",
        ),
        (
            "stop_times.txt",
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence\nT1,08:00:00,08:00:00,S1a,1\nT1,08:01:00,08:01:00,S2a,1\n",
        ),
        (
            "agency.txt",
            "agency_id,agency_name,agency_url,agency_timezone\nA,Agency,https://example.org,Mars/Olympus\n",
        ),
    ];
    for (file, body) in cases {
        let r = load(&minimal_with(&[(file, body)])).and_then(|f| f.timezone().map(|_| f));
        assert!(r.is_err(), "{file}: {body:?} should be rejected");
    }
}

#[test]
fn files_nested_in_one_folder_are_found() {
    let files = allstops_gtfs::fixture::minimal_files();
    let named: Vec<(String, &str)> = files
        .iter()
        .map(|(n, b)| (format!("gtfs/{n}"), b.as_str()))
        .collect();
    let refs: Vec<(&str, &str)> = named.iter().map(|(n, b)| (n.as_str(), *b)).collect();
    assert!(load(&zip_files(&refs)).is_ok());
}
