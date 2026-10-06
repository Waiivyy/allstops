use std::path::PathBuf;

use allstops_gtfs::cluster::{ClusterConfig, cluster};
use allstops_gtfs::select::Selection;
use allstops_gtfs::{Feed, Limits};
use anyhow::{Context as _, Result};

use crate::check::{check_json, print_report};
use crate::cmd_fetch::sha256_hex;
use crate::{Outcome, style};

#[derive(clap::Args)]
pub struct Args {
    /// GTFS zip the itinerary was planned from.
    zip: PathBuf,
    /// Itinerary JSON.
    itinerary: PathBuf,
    /// Selection that defines the target stations.
    #[arg(long)]
    selection: PathBuf,
}

pub fn run(args: Args, json: bool) -> Result<Outcome> {
    let bytes =
        std::fs::read(&args.zip).with_context(|| format!("reading {}", args.zip.display()))?;
    let feed = Feed::from_zip_bytes(&bytes, &Limits::default())?;
    let c = cluster(&feed, &ClusterConfig::default());
    let sel: Selection = toml::from_str(&std::fs::read_to_string(&args.selection)?)
        .with_context(|| format!("parsing {}", args.selection.display()))?;
    let text = std::fs::read_to_string(&args.itinerary)?;
    let report = check_json(&feed, &c, &sel, Some(&sha256_hex(&bytes)), &text)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if report.passed {
        println!(
            "{} {} stations visited, {} s from first to last target",
            style::good("verified:"),
            report.stations_visited,
            report.duration_s.unwrap_or(0)
        );
    } else {
        eprintln!(
            "{} {} violation(s)",
            style::bad("rejected:"),
            report.violations.len()
        );
        print_report(&report);
    }
    Ok(if report.passed {
        Outcome::Ok
    } else {
        Outcome::Rejected
    })
}
