//! Load everything a plan needs: feed, rules, selection, clustering and the
//! routing network for the plan date.

use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::time::Instant;

use allstops_core::network::Network;
use allstops_core::rules::Rules;
use allstops_gtfs::calendar::ServiceCalendar;
use allstops_gtfs::cluster::{ClusterConfig, Clustering, cluster};
use allstops_gtfs::network::{BuildReport, build_network};
use allstops_gtfs::select::{Selection, select};
use allstops_gtfs::{Feed, Limits};
use anyhow::{Context, Result};

use crate::style;

#[derive(clap::Args, Clone)]
pub struct PlanArgs {
    /// Path to a GTFS zip.
    pub zip: PathBuf,
    /// Rules file (TOML). Its `selection` path is relative to the rules file.
    #[arg(long)]
    pub rules: PathBuf,
    /// Override the plan date (YYYY-MM-DD).
    #[arg(long)]
    pub date: Option<String>,
}

pub struct PlanInput {
    #[allow(dead_code, reason = "used by verify and export")]
    pub feed: Feed,
    #[allow(dead_code, reason = "used by verify and export")]
    pub clustering: Clustering,
    pub rules: Rules,
    #[allow(dead_code, reason = "used by verify and export")]
    pub selection: Selection,
    pub network: Network,
    pub report: BuildReport,
    pub load_ms: f64,
    pub build_ms: f64,
}

pub fn load_rules(path: &Path, date: Option<&str>) -> Result<(Rules, Selection)> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut rules: Rules =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if let Some(d) = date {
        rules.date = d.to_string();
    }
    let sel_path = path
        .parent()
        .unwrap_or(Path::new("."))
        .join(&rules.selection);
    let sel_text = std::fs::read_to_string(&sel_path)
        .with_context(|| format!("reading {}", sel_path.display()))?;
    let selection: Selection =
        toml::from_str(&sel_text).with_context(|| format!("parsing {}", sel_path.display()))?;
    Ok((rules, selection))
}

/// Route types whose trips count as visits: those named by the selection's
/// rules, or every type when the selection names none.
pub fn visit_types(sel: &Selection) -> Vec<RangeInclusive<u16>> {
    let types: Vec<RangeInclusive<u16>> = sel
        .include
        .iter()
        .flat_map(|r| r.route_types.iter().map(|&t| t..=t))
        .collect();
    if types.is_empty() {
        vec![0..=u16::MAX]
    } else {
        types
    }
}

pub fn load(args: &PlanArgs) -> Result<PlanInput> {
    let (rules, selection) = load_rules(&args.rules, args.date.as_deref())?;
    let t0 = Instant::now();
    let bytes =
        std::fs::read(&args.zip).with_context(|| format!("reading {}", args.zip.display()))?;
    let feed = Feed::from_zip_bytes(&bytes, &Limits::default())?;
    let load_ms = t0.elapsed().as_secs_f64() * 1e3;
    let t1 = Instant::now();
    let clustering = cluster(&feed, &ClusterConfig::default());
    let targets = select(&feed, &clustering, &selection)?;
    let cal = ServiceCalendar::new(&feed);
    let (network, report) = build_network(
        &feed,
        &cal,
        &clustering,
        &targets,
        &visit_types(&selection),
        &rules,
    )?;
    let build_ms = t1.elapsed().as_secs_f64() * 1e3;
    eprintln!(
        "{}",
        style::dim(&format!(
            "feed loaded in {load_ms:.0} ms; network for {} built in {build_ms:.0} ms: {} trips, {} connections, {} stations, {} walk links, {} targets",
            report.date,
            report.trips,
            report.connections,
            report.stations,
            report.footpaths,
            report.targets
        ))
    );
    Ok(PlanInput {
        feed,
        clustering,
        rules,
        selection,
        network,
        report,
        load_ms,
        build_ms,
    })
}
