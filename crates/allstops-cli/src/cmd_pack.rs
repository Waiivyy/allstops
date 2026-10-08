use std::path::{Path, PathBuf};
use std::time::Instant;

use allstops_core::rules::route_types_for_mode;
use allstops_gtfs::Limits;
use allstops_gtfs::calendar::{ServiceCalendar, validity};
use allstops_gtfs::network::build_network;
use allstops_gtfs::pack::{self, PackHeader, PackSource};
use allstops_gtfs::select::visit_route_types;
use anyhow::{Context, Result};

use crate::cmd_fetch::sha256_hex;
use crate::plan_input::{
    basis_from_pack, basis_from_zip, load_rules, override_hashes, pack_rules, read_file,
    sha256_json,
};
use crate::{Outcome, style};

#[derive(clap::Args)]
pub struct Args {
    /// GTFS zip to pack.
    zip: PathBuf,
    /// Rules file. Its selection, station overrides, walks and connector
    /// modes decide what the pack holds; the rest are stored as defaults.
    #[arg(long)]
    rules: PathBuf,
    /// Where to write the pack. Default: next to the zip, named after the
    /// rules file, with the extension `.pack`.
    #[arg(long)]
    out: Option<PathBuf>,
}

fn default_out(zip: &Path, rules: &Path) -> PathBuf {
    let stem = rules
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "network".into());
    zip.parent()
        .unwrap_or(Path::new("."))
        .join(format!("{stem}.pack"))
}

pub fn run(args: Args, json: bool) -> Result<Outcome> {
    let files = load_rules(&args.rules, None)?;
    let zip = read_file(&args.zip)?;
    if pack::is_pack(&zip) {
        anyhow::bail!(
            "{} is already a pack; give the GTFS zip",
            args.zip.display()
        );
    }
    let basis = basis_from_zip(
        &args.zip,
        &zip,
        &files.selection,
        &files.stations,
        files.walks.clone(),
    )?;
    if basis.feed_ref.id == "unregistered" {
        eprintln!(
            "{}",
            style::dim(
                "this feed is not pinned in a data/feeds.toml next to it or the current directory; the attribution comes from feed_info.txt"
            )
        );
    }
    let feed = &basis.feed;
    let mut connector_types = Vec::new();
    for m in &files.rules.connector_modes {
        connector_types
            .extend(route_types_for_mode(m).with_context(|| format!("unknown mode {m:?}"))?);
    }
    let (station_overrides_sha256, walks_sha256) = override_hashes(&files)?;
    let header = PackHeader {
        feed_id: basis.feed_ref.id.clone(),
        feed_sha256: basis.feed_ref.sha256.clone(),
        feed_version: basis.feed_ref.feed_version.clone(),
        attribution: basis.feed_ref.attribution.clone(),
        timezone: basis.timezone.clone(),
        validity: validity(feed, &ServiceCalendar::new(feed))
            .map(|(a, b)| (a.to_string(), b.to_string())),
        selection_name: files.selection.name.clone(),
        selection_sha256: sha256_json(&files.selection)?,
        rules_sha256: sha256_json(&files.rules)?,
        station_overrides_sha256,
        walks_sha256,
        connector_modes: files.rules.connector_modes.clone(),
        rules_toml: toml::to_string(&files.rules).context("writing the rules into the pack")?,
        generator: format!("allstops {}", env!("CARGO_PKG_VERSION")),
        counts: Default::default(),
    };
    let t0 = Instant::now();
    let bytes = pack::build(
        &PackSource {
            zip: &zip,
            feed,
            clustering: &basis.clustering,
            targets: &basis.targets,
            visit_types: &visit_route_types(feed, &files.selection)?,
            connector_types: &connector_types,
            walks: &basis.walks,
            header,
        },
        &Limits::default(),
    )?;
    let build_ms = t0.elapsed().as_secs_f64() * 1e3;
    let out = args
        .out
        .clone()
        .unwrap_or_else(|| default_out(&args.zip, &args.rules));
    std::fs::write(&out, &bytes).with_context(|| format!("writing {}", out.display()))?;

    // Load it back as `solve` does and build the network for the rules' date.
    let loaded = basis_from_pack(&bytes, None)?;
    let h = loaded.pack.clone().expect("read from a pack");
    let rules = pack_rules(&h, None)?;
    let t1 = Instant::now();
    let (network, _) = build_network(
        &loaded.feed,
        &ServiceCalendar::new(&loaded.feed),
        &loaded.clustering,
        &loaded.targets,
        &loaded.visit_types,
        &rules,
        &loaded.walks,
    )?;
    let network_ms = t1.elapsed().as_secs_f64() * 1e3;
    let sha = sha256_hex(&bytes);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "path": out,
                "bytes": bytes.len(),
                "sha256": sha,
                "format_version": pack::FORMAT_VERSION,
                "header": h,
                "zip_bytes": zip.len(),
                "zip_load_ms": basis.load_ms.round(),
                "build_ms": build_ms.round(),
                "pack_load_ms": loaded.load_ms.round(),
                "network_date": rules.date,
                "network_build_ms": network_ms.round(),
                "network_connections": network.connections.len(),
            }))?
        );
        return Ok(Outcome::Ok);
    }
    let c = &h.counts;
    println!("{}", style::bold("Network pack"));
    let row = |k: &str, v: String| println!("  {k:<22} {v}");
    row("file", out.display().to_string());
    row(
        "size",
        format!(
            "{} bytes ({:.1}% of the {} byte feed)",
            bytes.len(),
            100.0 * bytes.len() as f64 / zip.len() as f64,
            zip.len()
        ),
    );
    row("sha256", sha);
    row("format version", pack::FORMAT_VERSION.to_string());
    row(
        "selection",
        format!("{} ({} targets)", h.selection_name, c.targets),
    );
    row("connector modes", h.connector_modes.join(", "));
    row(
        "holds",
        format!(
            "{} stations, {} stops, {} routes, {} trips, {} stop times",
            c.stations, c.stops, c.routes, c.trips, c.stop_times
        ),
    );
    row("built in", format!("{build_ms:.0} ms"));
    row(
        "load",
        format!(
            "{:.0} ms for the pack (the full zip: {:.0} ms); network for {} in {network_ms:.0} ms",
            loaded.load_ms, basis.load_ms, rules.date
        ),
    );
    println!("{}", h.attribution);
    Ok(Outcome::Ok)
}
