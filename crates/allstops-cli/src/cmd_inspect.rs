use std::path::PathBuf;
use std::time::Instant;

use allstops_gtfs::inspect::{Profile, profile};
use allstops_gtfs::{Feed, Limits};
use anyhow::{Context, Result};
use chrono::NaiveDate;

use crate::{Outcome, style};

#[derive(clap::Args)]
pub struct Args {
    /// Path to a GTFS zip or a network pack.
    #[arg(value_name = "ZIP_OR_PACK")]
    zip: PathBuf,
    /// Also check that this plan date (YYYY-MM-DD) is inside the validity range.
    #[arg(long)]
    date: Option<NaiveDate>,
}

/// Plans need dates at least this far ahead to be useful.
const LOOKAHEAD_DAYS: i64 = 28;

pub fn run(args: Args, json: bool) -> Result<Outcome> {
    let bytes =
        std::fs::read(&args.zip).with_context(|| format!("reading {}", args.zip.display()))?;
    if allstops_gtfs::pack::is_pack(&bytes) {
        return inspect_pack(&bytes, json, args.date);
    }
    let t0 = Instant::now();
    let feed = Feed::from_zip_bytes(&bytes, &Limits::default())
        .with_context(|| format!("loading {}", args.zip.display()))?;
    let load_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let t1 = Instant::now();
    let p = profile(&feed);
    let profile_ms = t1.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "{}",
        style::dim(&format!(
            "loaded {} bytes in {load_ms:.0} ms, profiled in {profile_ms:.0} ms",
            bytes.len()
        ))
    );

    let today = chrono::Local::now().date_naive();
    let ahead = today + chrono::Duration::days(LOOKAHEAD_DAYS);
    let covers = |d: NaiveDate| p.validity.is_some_and(|(s, e)| d >= s && d <= e);

    if json {
        let mut v = serde_json::to_value(&p)?;
        v["load_ms"] = serde_json::json!(load_ms.round());
        v["covers_lookahead"] = serde_json::json!({ "date": ahead, "covered": covers(ahead) });
        if let Some(d) = args.date {
            v["covers_plan_date"] = serde_json::json!({ "date": d, "covered": covers(d) });
        }
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(Outcome::Ok);
    }
    print_human(&p);
    let line = |label: &str, d: NaiveDate| {
        let mark = if covers(d) {
            style::good("yes")
        } else {
            style::bad("NO")
        };
        println!("  {label:<26} {d}: {mark}");
    };
    println!("{}", style::bold("Coverage"));
    line(&format!("{LOOKAHEAD_DAYS} days ahead"), ahead);
    if let Some(d) = args.date {
        line("plan date", d);
    }
    Ok(Outcome::Ok)
}

fn range(r: Option<(NaiveDate, NaiveDate)>) -> String {
    r.map(|(s, e)| format!("{s} to {e}"))
        .unwrap_or_else(|| "none".into())
}

fn print_human(p: &Profile) {
    println!("{}", style::bold("Feed"));
    println!(
        "  publisher                  {}",
        p.publisher.as_deref().unwrap_or("(no feed_info)")
    );
    println!(
        "  feed_version               {}",
        p.feed_version.as_deref().unwrap_or("-")
    );
    println!("  time zone                  {}", p.timezone);
    println!(
        "  declared validity          {}",
        range(p.declared_validity)
    );
    println!("  service range (calendars)  {}", range(p.service_range));
    println!(
        "  files                      {}",
        p.files_present.join(", ")
    );
    println!(
        "  optional files missing     {}",
        p.optional_files_missing.join(", ")
    );
    if !p.files_empty.is_empty() {
        println!("  files with no rows         {}", p.files_empty.join(", "));
    }

    println!("{}", style::bold("Contents"));
    println!("  agencies                   {}", p.agencies);
    for (rt, c) in &p.routes_by_type {
        println!(
            "  route_type {rt:<4}            {} routes, {} trips",
            c.routes, c.trips
        );
    }
    for (lt, n) in &p.stops_by_location_type {
        println!("  {lt:<26} {n}");
    }
    println!("  stops with parent_station  {}", p.stops_with_parent);
    println!("  hierarchy depth            {}", p.hierarchy_depth);
    println!(
        "  DHID-style stop IDs        {:.1}% (e.g. {})",
        p.id_scheme.dhid_share * 100.0,
        p.id_scheme.examples.join(", ")
    );
    println!("  trips                      {}", p.trips);
    println!("  stop_times                 {}", p.stop_times);
    println!("  services                   {}", p.services);
    println!("  trips ending after 24:00   {}", p.trips_past_midnight);
    println!("  latest time                {}", p.latest_time);
    println!(
        "  frequencies rows           {} ({} exact_times)",
        p.frequencies_rows, p.frequencies_exact_times
    );
    println!("  transfers rows             {}", p.transfers_rows);
    for (k, n) in &p.pickup_drop_off {
        println!("  {k:<26} {n}");
    }

    println!("{}", style::bold("Warnings"));
    let w = &p.warnings;
    let l = &w.load;
    let rows: [(&str, u64); 16] = [
        ("orphan stops/stations", w.orphan_stops as u64),
        (
            "trips with no service days",
            w.trips_without_service_days as u64,
        ),
        (
            "trips with non-monotonic times",
            w.trips_non_monotonic as u64,
        ),
        ("duplicate trips", w.duplicate_trips as u64),
        (
            "stops without coordinates",
            w.stops_without_coordinates as u64,
        ),
        ("stop_times unknown trip", l.stop_times_unknown_trip),
        ("stop_times unknown stop", l.stop_times_unknown_stop),
        ("stop_times one time filled", l.stop_times_filled_time),
        ("stop_times interpolated", l.stop_times_interpolated),
        ("stop_times missing time", l.stop_times_missing_time),
        ("trips unknown route", l.trips_unknown_route),
        ("trips without stop_times", l.trips_without_stop_times),
        ("stops unknown parent", l.stops_unknown_parent),
        ("calendar_dates bad type", l.calendar_dates_bad_exception),
        ("frequencies unknown trip", l.frequencies_unknown_trip),
        ("transfers rows skipped", l.transfers_skipped),
    ];
    for (label, n) in rows {
        let v = n.to_string();
        let v = if n > 0 {
            style::warn(&v)
        } else {
            style::dim(&v)
        };
        println!("  {label:<32} {v}");
    }
}

/// Print a pack's header after checking the whole pack.
fn inspect_pack(bytes: &[u8], json: bool, date: Option<NaiveDate>) -> Result<Outcome> {
    let t0 = Instant::now();
    let p = allstops_gtfs::pack::read(bytes, &Limits::default())?;
    let load_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let h = &p.header;
    // The range plans from this pack accept, from its own calendars.
    let validity = allstops_gtfs::calendar::validity(
        &p.feed,
        &allstops_gtfs::calendar::ServiceCalendar::new(&p.feed),
    );
    let covers = |d: NaiveDate| validity.is_some_and(|(s, e)| d >= s && d <= e);
    if json {
        let mut v = serde_json::to_value(h)?;
        v["format_version"] = serde_json::json!(allstops_gtfs::pack::FORMAT_VERSION);
        v["bytes"] = serde_json::json!(bytes.len());
        v["load_ms"] = serde_json::json!(load_ms.round());
        if let Some(d) = date {
            v["covers_plan_date"] = serde_json::json!({ "date": d, "covered": covers(d) });
        }
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(Outcome::Ok);
    }
    let none = || "none".to_string();
    println!("{}", style::bold("Network pack"));
    let row = |k: &str, v: String| println!("  {k:<22} {v}");
    row(
        "format version",
        allstops_gtfs::pack::FORMAT_VERSION.to_string(),
    );
    row(
        "size",
        format!("{} bytes, checked in {load_ms:.0} ms", bytes.len()),
    );
    row("written by", h.generator.clone());
    row("feed", format!("{} ({})", h.feed_id, h.feed_version));
    row("feed sha256", h.feed_sha256.clone());
    row("time zone", h.timezone.clone());
    row(
        "validity",
        h.validity
            .as_ref()
            .map(|(a, b)| format!("{a} to {b}"))
            .unwrap_or_else(none),
    );
    row(
        "selection",
        format!("{} ({})", h.selection_name, h.selection_sha256),
    );
    row("rules sha256", h.rules_sha256.clone());
    row(
        "station overrides",
        h.station_overrides_sha256.clone().unwrap_or_else(none),
    );
    row("walks", h.walks_sha256.clone().unwrap_or_else(none));
    row("connector modes", h.connector_modes.join(", "));
    let c = &h.counts;
    row(
        "holds",
        format!(
            "{} stations ({} targets), {} stops, {} routes, {} trips, {} stop times",
            c.stations, c.targets, c.stops, c.routes, c.trips, c.stop_times
        ),
    );
    if let Some(d) = date {
        let mark = if covers(d) {
            style::good("yes")
        } else {
            style::bad("NO")
        };
        row("plan date", format!("{d}: {mark}"));
    }
    println!("{}", h.attribution);
    Ok(Outcome::Ok)
}
