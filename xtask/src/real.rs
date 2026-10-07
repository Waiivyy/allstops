//! Load a real network for benchmarks: feed, selection, rules, plan date.

use std::ops::RangeInclusive;
use std::path::Path;
use std::time::Instant;

use allstops_core::network::Network;
use allstops_core::rules::Rules;
use allstops_gtfs::calendar::ServiceCalendar;
use allstops_gtfs::cluster::{ClusterConfig, Clustering, cluster};
use allstops_gtfs::network::{BuildReport, build_network};
use allstops_gtfs::select::{Selection, select};
use allstops_gtfs::{Feed, Limits};
use anyhow::{Context, Result};

#[allow(dead_code, reason = "fields used by individual benchmarks")]
pub struct Real {
    pub feed: Feed,
    pub bytes: Vec<u8>,
    pub clustering: Clustering,
    pub selection: Selection,
    pub rules: Rules,
    pub network: Network,
    pub report: BuildReport,
    pub load_ms: f64,
    pub build_ms: f64,
}

/// Route types whose trips count as visits, from the routes the
/// selection's route filters match.
pub fn visit_types(feed: &Feed, sel: &Selection) -> Result<Vec<RangeInclusive<u16>>> {
    Ok(allstops_gtfs::select::visit_route_types(feed, sel)?
        .into_iter()
        .map(|t| t..=t)
        .collect())
}

pub fn load_feed(zip: &Path) -> Result<(Feed, Vec<u8>, f64)> {
    let bytes = std::fs::read(zip).with_context(|| format!("reading {}", zip.display()))?;
    let t0 = Instant::now();
    let feed = Feed::from_zip_bytes(&bytes, &Limits::default())?;
    Ok((feed, bytes, t0.elapsed().as_secs_f64() * 1e3))
}

/// A rules file and the selection it names (relative to the rules file).
pub fn load_rules(rules_path: &Path) -> Result<(Rules, Selection)> {
    let rules: Rules = toml::from_str(
        &std::fs::read_to_string(rules_path)
            .with_context(|| format!("reading {}", rules_path.display()))?,
    )?;
    rules
        .validate()
        .map_err(|m| anyhow::anyhow!("invalid rules in {}:\n{m}", rules_path.display()))?;
    let sel_path = rules_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(&rules.selection);
    let selection: Selection = toml::from_str(
        &std::fs::read_to_string(&sel_path)
            .with_context(|| format!("reading {}", sel_path.display()))?,
    )?;
    Ok((rules, selection))
}

pub fn load(zip: &Path, rules_path: &Path, date: Option<&str>) -> Result<Real> {
    let (feed, bytes, load_ms) = load_feed(zip)?;
    let (mut rules, selection) = load_rules(rules_path)?;
    if let Some(d) = date {
        rules.date = d.to_string();
    }
    let t1 = Instant::now();
    let clustering = cluster(&feed, &ClusterConfig::default());
    let targets = select(&feed, &clustering, &selection)?;
    let cal = ServiceCalendar::new(&feed);
    let (network, report) = build_network(
        &feed,
        &cal,
        &clustering,
        &targets,
        &visit_types(&feed, &selection)?,
        &rules,
    )?;
    let build_ms = t1.elapsed().as_secs_f64() * 1e3;
    Ok(Real {
        feed,
        bytes,
        clustering,
        selection,
        rules,
        network,
        report,
        load_ms,
        build_ms,
    })
}
