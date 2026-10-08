//! Load everything a plan or a check needs, from a GTFS zip with its rules
//! files or from a network pack: feed, stations, targets, walks, rules and
//! the routing network for the plan date.

use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::time::Instant;

use allstops_core::itinerary::FeedRef;
use allstops_core::network::Network;
use allstops_core::rules::Rules;
use allstops_gtfs::calendar::ServiceCalendar;
use allstops_gtfs::cluster::{
    ClusterConfig, Clustering, StationOverrides, apply_overrides, cluster,
};
use allstops_gtfs::network::{BuildReport, build_network};
use allstops_gtfs::pack::{self, PackHeader};
use allstops_gtfs::select::{Selection, select, visit_route_types};
use allstops_gtfs::walks::WalkOverrides;
use allstops_gtfs::{Feed, Limits};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::style;

#[derive(clap::Args, Clone)]
pub struct PlanArgs {
    /// GTFS zip, or a network pack written by `allstops pack`.
    #[arg(value_name = "ZIP_OR_PACK")]
    pub input: PathBuf,
    /// Rules file (TOML). Its `selection`, `station_overrides` and `walks`
    /// paths are relative to the rules file. Required with a zip; with a
    /// pack it defaults to the rules the pack was built with, and must
    /// match the pack's selection, overrides and connector modes.
    #[arg(long)]
    pub rules: Option<PathBuf>,
    /// Override the plan date (YYYY-MM-DD).
    #[arg(long)]
    pub date: Option<String>,
}

/// The feed and what plans and checks derive from it.
pub struct Basis {
    pub feed: Feed,
    pub clustering: Clustering,
    /// Target stations, as indices into `clustering`.
    pub targets: Vec<u32>,
    /// Route types whose trips count as visits.
    pub visit_types: Vec<RangeInclusive<u16>>,
    pub walks: WalkOverrides,
    pub feed_ref: FeedRef,
    pub timezone: String,
    /// Decoding the zip or the pack, in milliseconds.
    pub load_ms: f64,
    /// The pack's header, when the input was a pack.
    pub pack: Option<PackHeader>,
}

impl Basis {
    pub fn target_ids(&self) -> Vec<String> {
        self.targets
            .iter()
            .map(|&t| self.clustering.stations[t as usize].id.clone())
            .collect()
    }
}

pub struct PlanInput {
    pub basis: Basis,
    pub rules: Rules,
    pub network: Network,
    pub report: BuildReport,
    pub build_ms: f64,
}

/// A rules file and the files it names.
pub struct RuleFiles {
    pub rules: Rules,
    pub selection: Selection,
    pub stations: StationOverrides,
    pub walks: WalkOverrides,
}

pub fn read_toml<T: DeserializeOwned>(path: &Path) -> Result<T> {
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

/// SHA-256 of a value's canonical JSON (fields in declaration order).
pub fn sha256_json<T: Serialize>(value: &T) -> Result<String> {
    Ok(crate::cmd_fetch::sha256_hex(&serde_json::to_vec(value)?))
}

/// Hashes of the station overrides and walks, or `None` when they are empty,
/// as recorded in and checked against a pack's header.
pub fn override_hashes(files: &RuleFiles) -> Result<(Option<String>, Option<String>)> {
    let stations = if files.stations == StationOverrides::default() {
        None
    } else {
        Some(sha256_json(&files.stations)?)
    };
    let walks = if files.walks.walk.is_empty() {
        None
    } else {
        Some(sha256_json(&files.walks)?)
    };
    Ok((stations, walks))
}

pub fn read_file(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("reading {}", path.display()))
}

/// A GTFS zip, clustered with the station overrides, with the selection's
/// targets.
pub fn basis_from_zip(
    bytes: &[u8],
    selection: &Selection,
    overrides: &StationOverrides,
    walks: WalkOverrides,
) -> Result<Basis> {
    let t0 = Instant::now();
    let feed = Feed::from_zip_bytes(bytes, &Limits::default())?;
    let load_ms = t0.elapsed().as_secs_f64() * 1e3;
    let clustering = stations(&feed, overrides)?;
    let targets = select(&feed, &clustering, selection)?;
    Ok(Basis {
        visit_types: visit_types(&feed, selection)?,
        feed_ref: feed_ref(bytes, &feed),
        timezone: feed.timezone()?.name().to_string(),
        feed,
        clustering,
        targets,
        walks,
        load_ms,
        pack: None,
    })
}

/// A network pack. With rules files, they must describe the pack: the same
/// selection, station overrides and walks, and no connector mode the pack
/// does not hold.
pub fn basis_from_pack(bytes: &[u8], files: Option<&RuleFiles>) -> Result<Basis> {
    let t0 = Instant::now();
    let p = pack::read(bytes, &Limits::default())?;
    let load_ms = t0.elapsed().as_secs_f64() * 1e3;
    if let Some(files) = files {
        check_matches_pack(files, &p.header)?;
    }
    let h = &p.header;
    Ok(Basis {
        feed_ref: FeedRef {
            id: h.feed_id.clone(),
            sha256: h.feed_sha256.clone(),
            feed_version: h.feed_version.clone(),
            attribution: h.attribution.clone(),
        },
        timezone: p.feed.timezone()?.name().to_string(),
        visit_types: p.visit_types.iter().map(|&t| t..=t).collect(),
        feed: p.feed,
        clustering: p.clustering,
        targets: p.targets,
        walks: p.walks,
        load_ms,
        pack: Some(p.header),
    })
}

fn check_matches_pack(files: &RuleFiles, h: &PackHeader) -> Result<()> {
    let rebuild = "rebuild the pack with `allstops pack`, or leave out --rules";
    if sha256_json(&files.selection)? != h.selection_sha256 {
        bail!(
            "the pack was built for another selection ({:?}); {rebuild}",
            h.selection_name
        );
    }
    let (stations, walks) = override_hashes(files)?;
    if stations != h.station_overrides_sha256 {
        bail!("the pack was built with other station overrides; {rebuild}");
    }
    if walks != h.walks_sha256 {
        bail!("the pack was built with another walks file; {rebuild}");
    }
    if let Some(m) = files
        .rules
        .connector_modes
        .iter()
        .find(|m| !h.connector_modes.contains(m))
    {
        bail!(
            "the pack holds no {m} trips (its connector modes are {}); {rebuild}",
            h.connector_modes.join(", ")
        );
    }
    Ok(())
}

/// The rules a pack was built with.
pub fn pack_rules(h: &PackHeader, date: Option<&str>) -> Result<Rules> {
    let mut rules: Rules =
        toml::from_str(&h.rules_toml).context("reading the rules stored in the pack")?;
    if let Some(d) = date {
        rules.date = d.to_string();
    }
    rules
        .validate()
        .map_err(|m| anyhow::anyhow!("invalid rules in the pack:\n{m}"))?;
    Ok(rules)
}

/// A zip with its rules files, or a pack with optional rules.
pub fn load_basis(
    input: &Path,
    rules: Option<&Path>,
    date: Option<&str>,
) -> Result<(Basis, Rules)> {
    let bytes = read_file(input)?;
    if pack::is_pack(&bytes) {
        let files = rules.map(|p| load_rules(p, date)).transpose()?;
        let basis = basis_from_pack(&bytes, files.as_ref())?;
        let rules = match files {
            Some(f) => f.rules,
            None => pack_rules(basis.pack.as_ref().expect("read from a pack"), date)?,
        };
        Ok((basis, rules))
    } else {
        let Some(path) = rules else {
            bail!("--rules is required with a GTFS zip");
        };
        let files = load_rules(path, date)?;
        let basis = basis_from_zip(&bytes, &files.selection, &files.stations, files.walks)?;
        Ok((basis, files.rules))
    }
}

pub fn load(args: &PlanArgs) -> Result<PlanInput> {
    let (basis, rules) = load_basis(&args.input, args.rules.as_deref(), args.date.as_deref())?;
    let t1 = Instant::now();
    let cal = ServiceCalendar::new(&basis.feed);
    let (network, report) = build_network(
        &basis.feed,
        &cal,
        &basis.clustering,
        &basis.targets,
        &basis.visit_types,
        &rules,
        &basis.walks,
    )?;
    let build_ms = t1.elapsed().as_secs_f64() * 1e3;
    eprintln!(
        "{}",
        style::dim(&format!(
            "{} loaded in {:.0} ms; network for {} built in {build_ms:.0} ms: {} trips, {} connections, {} stations, {} walk links, {} targets",
            if basis.pack.is_some() { "pack" } else { "feed" },
            basis.load_ms,
            report.date,
            report.trips,
            report.connections,
            report.stations,
            report.footpaths,
            report.targets
        ))
    );
    if !basis.walks.walk.is_empty() {
        eprintln!(
            "{}",
            style::dim(&format!(
                "walks file: {} walk links set, {} entries matched no walk link within {} m",
                report.walk_overrides_applied, report.walk_overrides_unused, rules.max_walk_m
            ))
        );
    }
    Ok(PlanInput {
        basis,
        rules,
        network,
        report,
        build_ms,
    })
}
