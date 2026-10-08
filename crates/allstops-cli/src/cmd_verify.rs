use std::path::PathBuf;

use allstops_gtfs::cluster::StationOverrides;
use allstops_gtfs::pack;
use allstops_gtfs::select::Selection;
use allstops_gtfs::walks::WalkOverrides;
use anyhow::{Context as _, Result, bail};

use crate::check::{check_json, print_report, rules_for_verifier};
use crate::plan_input::{basis_from_pack, basis_from_zip, load_rules, read_file, read_toml};
use crate::{Outcome, style};

#[derive(clap::Args)]
pub struct Args {
    /// GTFS zip or network pack the itinerary was planned from.
    #[arg(value_name = "ZIP_OR_PACK")]
    input: PathBuf,
    /// Itinerary JSON.
    itinerary: PathBuf,
    /// Selection that defines the target stations, for a zip. Taken from
    /// the rules file when --rules is given and this is not; a pack holds
    /// its own.
    #[arg(long)]
    selection: Option<PathBuf>,
    /// Rules the itinerary must have been planned under. The itinerary's
    /// own rules must match them, and they are the rules checked, with the
    /// station and walk overrides they name (with a pack, they must match
    /// it). Without this option the rules embedded in the itinerary are
    /// checked.
    #[arg(long)]
    rules: Option<PathBuf>,
}

pub fn run(args: Args, json: bool) -> Result<Outcome> {
    let bytes = read_file(&args.input)?;
    let text = std::fs::read_to_string(&args.itinerary)
        .with_context(|| format!("reading {}", args.itinerary.display()))?;
    let files = args
        .rules
        .as_deref()
        .map(|p| load_rules(p, None))
        .transpose()?;
    let basis = if pack::is_pack(&bytes) {
        if args.selection.is_some() {
            bail!("a pack holds its own targets; leave out --selection");
        }
        basis_from_pack(&bytes, files.as_ref())?
    } else {
        match (&files, &args.selection) {
            (Some(f), sel) => {
                let selection: Selection = match sel {
                    Some(p) => read_toml(p)?,
                    None => f.selection.clone(),
                };
                basis_from_zip(&bytes, &selection, &f.stations, f.walks.clone())?
            }
            (None, Some(sel)) => {
                // The override files are named relative to a rules file, so
                // an itinerary planned with them needs --rules or a pack.
                let doc: serde_json::Value = serde_json::from_str(&text)
                    .with_context(|| format!("parsing {}", args.itinerary.display()))?;
                for key in ["station_overrides", "walks"] {
                    if !doc["rules"][key].is_null() {
                        bail!(
                            "the itinerary was planned with a {key} file; give the rules file with --rules, or the pack"
                        );
                    }
                }
                basis_from_zip(
                    &bytes,
                    &read_toml(sel)?,
                    &StationOverrides::default(),
                    WalkOverrides::default(),
                )?
            }
            (None, None) => bail!("with a GTFS zip, give --selection or --rules"),
        }
    };
    let expected = files
        .as_ref()
        .map(|f| rules_for_verifier(&f.rules))
        .transpose()?;
    let report = check_json(&basis, expected.as_ref(), &text)?;
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
