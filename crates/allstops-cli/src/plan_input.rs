//! Load everything a plan needs: feed, rules, selection, clustering and the
//! routing network for the plan date.

use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::time::Instant;

use allstops_core::network::Network;
use allstops_core::rules::Rules;
use allstops_gtfs::calendar::ServiceCalendar;
use allstops_gtfs::cluster::{
    ClusterConfig, Clustering, StationOverrides, apply_overrides, cluster,
};
use allstops_gtfs::network::{BuildReport, build_network};
use allstops_gtfs::select::{Selection, select, visit_route_types};
use allstops_gtfs::walks::WalkOverrides;
use allstops_gtfs::{Feed, Limits};
use anyhow::{Context, Result};
use serde::de::DeserializeOwned;

use crate::style;

#[derive(clap::Args, Clone)]
pub struct PlanArgs {
    /// Path to a GTFS zip.
    pub zip: PathBuf,
    /// Rules file (TOML). Its `selection`, `station_overrides` and `walks`
    /// paths are relative to the rules file.
    #[arg(long)]
    pub rules: PathBuf,
    /// Override the plan date (YYYY-MM-DD).
    #[arg(long)]
    pub date: Option<String>,
}

pub struct PlanInput {
    pub feed: Feed,
    pub clustering: Clustering,
    pub rules: Rules,
    pub selection: Selection,
    pub walks: WalkOverrides,
    pub network: Network,
    pub report: BuildReport,
    pub load_ms: f64,
    pub build_ms: f64,
    pub feed_sha256: String,
    pub feed_ref: allstops_core::itinerary::FeedRef,
    pub timezone: String,
}

/// A rules file and the files it names.
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

/// Read a rules file and the selection and override files it names
/// (relative to the rules file), and check them.
pub fn load_rules(path: &Path, date: Option<&str>) -> Result<RuleFiles> {
    let mut rules: Rules = read_toml(path)?;
    if let Some(d) = date {
        rules.date = d.to_string();
    }
    rules
        .validate()
        .map_err(|m| anyhow::anyhow!("invalid rules in {}:\n{m}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new("."));
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

/// Route types whose trips count as visits: those of the routes the
/// selection's route filters match (see `select::visit_route_types`).
pub fn visit_types(feed: &Feed, sel: &Selection) -> Result<Vec<RangeInclusive<u16>>> {
    Ok(visit_route_types(feed, sel)?
        .into_iter()
        .map(|t| t..=t)
        .collect())
}

/// Identity and attribution of a feed file: from the registry when this
/// exact file is pinned there, otherwise crediting the publisher named in
/// the feed.
pub fn feed_ref(bytes: &[u8], feed: &Feed) -> allstops_core::itinerary::FeedRef {
    let sha256 = crate::cmd_fetch::sha256_hex(bytes);
    let feed_version = feed
        .feed_info
        .as_ref()
        .map(|f| f.version.clone())
        .unwrap_or_default();
    let registered = crate::registry::Registry::load(&crate::default_registry())
        .ok()
        .and_then(|r| r.feeds.into_iter().find(|f| f.sha256 == sha256));
    match registered {
        Some(e) => allstops_core::itinerary::FeedRef {
            attribution: e.render_attribution(&feed_version),
            id: e.id,
            sha256,
            feed_version,
        },
        None => allstops_core::itinerary::FeedRef {
            id: "unregistered".into(),
            attribution: format!(
                "Timetable data: {}",
                feed.feed_info
                    .as_ref()
                    .map(|f| f.publisher_name.as_str())
                    .unwrap_or("see the feed's publisher")
            ),
            sha256,
            feed_version,
        },
    }
}

pub fn load(args: &PlanArgs) -> Result<PlanInput> {
    let RuleFiles {
        rules,
        selection,
        stations: station_overrides,
        walks,
    } = load_rules(&args.rules, args.date.as_deref())?;
    let t0 = Instant::now();
    let bytes =
        std::fs::read(&args.zip).with_context(|| format!("reading {}", args.zip.display()))?;
    let feed = Feed::from_zip_bytes(&bytes, &Limits::default())?;
    let load_ms = t0.elapsed().as_secs_f64() * 1e3;
    let feed_ref = feed_ref(&bytes, &feed);
    let feed_sha256 = feed_ref.sha256.clone();
    let timezone = feed.timezone()?.name().to_string();
    let t1 = Instant::now();
    let clustering = stations(&feed, &station_overrides)?;
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
    if !walks.walk.is_empty() {
        eprintln!(
            "{}",
            style::dim(&format!(
                "walks file: {} walk links set, {} entries matched no walk link within {} m",
                report.walk_overrides_applied, report.walk_overrides_unused, rules.max_walk_m
            ))
        );
    }
    Ok(PlanInput {
        feed,
        clustering,
        rules,
        selection,
        walks,
        network,
        report,
        load_ms,
        build_ms,
        feed_sha256,
        feed_ref,
        timezone,
    })
}
