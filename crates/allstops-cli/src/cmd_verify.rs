use std::path::PathBuf;

use allstops_gtfs::cluster::StationOverrides;
use allstops_gtfs::select::Selection;
use allstops_gtfs::walks::WalkOverrides;
use allstops_gtfs::{Feed, Limits};
use anyhow::{Context as _, Result, bail};

use crate::check::{check_json, print_report, rules_for_verifier};
use crate::cmd_fetch::sha256_hex;
use crate::plan_input::{RuleFiles, load_rules, stations};
use crate::{Outcome, style};

#[derive(clap::Args)]
pub struct Args {
    /// GTFS zip the itinerary was planned from.
    zip: PathBuf,
    /// Itinerary JSON.
    itinerary: PathBuf,
    /// Selection that defines the target stations. Taken from the rules
    /// file when --rules is given and this is not.
    #[arg(long, required_unless_present = "rules")]
    selection: Option<PathBuf>,
    /// Rules the itinerary must have been planned under. The itinerary's
    /// own rules must match them, and they are the rules checked, with the
    /// station and walk overrides they name. Without this option the rules
    /// embedded in the itinerary are checked.
    #[arg(long)]
    rules: Option<PathBuf>,
}

pub fn run(args: Args, json: bool) -> Result<Outcome> {
    let bytes =
        std::fs::read(&args.zip).with_context(|| format!("reading {}", args.zip.display()))?;
    let feed = Feed::from_zip_bytes(&bytes, &Limits::default())?;
    let text = std::fs::read_to_string(&args.itinerary)?;
    let (expected, rules_selection, station_overrides, walks) = match &args.rules {
        Some(path) => {
            let RuleFiles {
                rules,
                selection,
                stations,
                walks,
            } = load_rules(path, None)?;
            (
                Some(rules_for_verifier(&rules)?),
                Some(selection),
                stations,
                walks,
            )
        }
        None => {
            // The override files are named relative to a rules file, so an
            // itinerary planned with them can only be checked with --rules.
            let doc: serde_json::Value = serde_json::from_str(&text)
                .with_context(|| format!("parsing {}", args.itinerary.display()))?;
            for key in ["station_overrides", "walks"] {
                if !doc["rules"][key].is_null() {
                    bail!(
                        "the itinerary was planned with a {key} file; give the rules file with --rules"
                    );
                }
            }
            (
                None,
                None,
                StationOverrides::default(),
                WalkOverrides::default(),
            )
        }
    };
    let c = stations(&feed, &station_overrides)?;
    let sel: Selection = match (&args.selection, rules_selection) {
        (Some(path), _) => toml::from_str(&std::fs::read_to_string(path)?)
            .with_context(|| format!("parsing {}", path.display()))?,
        (None, Some(sel)) => sel,
        (None, None) => anyhow::bail!("give --selection or --rules"),
    };
    let report = check_json(
        &feed,
        &c,
        &sel,
        &walks,
        expected.as_ref(),
        Some(&sha256_hex(&bytes)),
        &text,
    )?;
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
