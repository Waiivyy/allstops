//! Load a real network for benchmarks: feed, selection, rules, plan date.

use std::ops::RangeInclusive;
use std::path::Path;
use std::time::Instant;

use allstops_core::network::Network;
use allstops_core::rules::Rules;
use allstops_gtfs::calendar::ServiceCalendar;
use allstops_gtfs::cluster::{
    ClusterConfig, Clustering, StationOverrides, apply_overrides, cluster,
};
use allstops_gtfs::network::{BuildReport, build_network};
use allstops_gtfs::select::{Selection, select};
use allstops_gtfs::walks::WalkOverrides;
use allstops_gtfs::{Feed, Limits};
use anyhow::{Context, Result};
use serde::de::DeserializeOwned;

#[allow(dead_code, reason = "fields used by individual benchmarks")]
pub struct Real {
    pub feed: Feed,
    pub bytes: Vec<u8>,
    pub clustering: Clustering,
    pub selection: Selection,
    pub rules: Rules,
    pub stations: StationOverrides,
    pub walks: WalkOverrides,
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

/// A rules file and the files it names.
#[derive(Clone)]
pub struct RuleFiles {
    pub rules: Rules,
    pub selection: Selection,
    pub stations: StationOverrides,
    pub walks: WalkOverrides,
}

fn read_toml<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// A rules file and the selection and override files it names (relative
/// to the rules file), checked as `allstops` checks them.
pub fn load_rules(rules_path: &Path) -> Result<RuleFiles> {
    let rules: Rules = read_toml(rules_path)?;
    rules
        .validate()
        .map_err(|m| anyhow::anyhow!("invalid rules in {}:\n{m}", rules_path.display()))?;
    let dir = rules_path.parent().unwrap_or(Path::new("."));
    let selection = read_toml(&dir.join(&rules.selection))?;
    let stations = match &rules.station_overrides {
        Some(rel) => read_toml(&dir.join(rel))?,
        None => StationOverrides::default(),
    };
    let walks: WalkOverrides = match &rules.walks {
        Some(rel) => read_toml(&dir.join(rel))?,
        None => WalkOverrides::default(),
    };
    walks
        .validate()
        .map_err(|m| anyhow::anyhow!("invalid walks file: {m}"))?;
    Ok(RuleFiles {
        rules,
        selection,
        stations,
        walks,
    })
}

/// Stations: automatic clustering, then the overrides.
pub fn stations(feed: &Feed, overrides: &StationOverrides) -> Result<Clustering> {
    apply_overrides(feed, cluster(feed, &ClusterConfig::default()), overrides)
        .context("applying the station overrides")
}

pub fn load(zip: &Path, rules_path: &Path, date: Option<&str>) -> Result<Real> {
    let (feed, bytes, load_ms) = load_feed(zip)?;
    let RuleFiles {
        mut rules,
        selection,
        stations: station_overrides,
        walks,
    } = load_rules(rules_path)?;
    if let Some(d) = date {
        rules.date = d.to_string();
    }
    let t1 = Instant::now();
    let clustering = stations(&feed, &station_overrides)?;
    walks.check_stations(&clustering)?;
    let targets = select(&feed, &clustering, &selection)?;
    let cal = ServiceCalendar::new(&feed);
    let (network, report) = build_network(
        &feed,
        &cal,
        &clustering,
        &targets,
        &visit_types(&feed, &selection)?,
        &rules,
        &walks,
    )?;
    let build_ms = t1.elapsed().as_secs_f64() * 1e3;
    Ok(Real {
        feed,
        bytes,
        clustering,
        selection,
        rules,
        stations: station_overrides,
        walks,
        network,
        report,
        load_ms,
        build_ms,
    })
}
