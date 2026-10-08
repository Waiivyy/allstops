use std::collections::BTreeMap;
use std::path::PathBuf;

use allstops_gtfs::cluster::{MergeReason, StationOverrides};
use allstops_gtfs::select::{Selection, select};
use allstops_gtfs::{Feed, Limits};
use anyhow::{Context, Result};

use crate::{Outcome, style};

#[derive(clap::Args)]
pub struct Args {
    /// Path to a GTFS zip.
    zip: PathBuf,
    /// Selection file; prints the selected target stations.
    #[arg(long)]
    selection: Option<PathBuf>,
    /// Station overrides (merge, split, rename) applied after clustering.
    #[arg(long)]
    overrides: Option<PathBuf>,
    /// Write all stations with their member stops and merge reasons here.
    #[arg(long)]
    out: Option<PathBuf>,
}

pub fn run(args: Args, json: bool) -> Result<Outcome> {
    let bytes =
        std::fs::read(&args.zip).with_context(|| format!("reading {}", args.zip.display()))?;
    if allstops_gtfs::pack::is_pack(&bytes) {
        anyhow::bail!("stations needs a GTFS zip; `allstops inspect` shows what a pack holds");
    }
    let feed = Feed::from_zip_bytes(&bytes, &Limits::default())?;
    let attribution = crate::plan_input::feed_ref(&bytes, &feed, &args.zip).attribution;
    let overrides: StationOverrides = match &args.overrides {
        Some(p) => toml::from_str(
            &std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?,
        )
        .with_context(|| format!("parsing {}", p.display()))?,
        None => StationOverrides::default(),
    };
    let c = crate::plan_input::stations(&feed, &overrides)?;

    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    for s in &c.stations {
        for m in &s.members {
            *reasons
                .entry(format!("{:?}", m.reason).to_lowercase())
                .or_insert(0) += 1;
        }
    }
    let multi = c.stations.iter().filter(|s| s.members.len() > 1).count();
    let dhid_merges = c
        .stations
        .iter()
        .filter(|s| {
            s.members
                .iter()
                .any(|m| m.reason == MergeReason::DhidPrefix)
        })
        .count();

    if let Some(out) = &args.out {
        std::fs::write(out, serde_json::to_string_pretty(&c)?)?;
        eprintln!("{} {}", style::dim("wrote"), out.display());
    }

    let selected = match &args.selection {
        Some(p) => {
            let text =
                std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
            let sel: Selection =
                toml::from_str(&text).with_context(|| format!("parsing {}", p.display()))?;
            let ids = select(&feed, &c, &sel)?;
            Some((sel.name, ids))
        }
        None => None,
    };

    if json {
        let sel = selected.as_ref().map(|(name, ids)| {
            serde_json::json!({
                "name": name,
                "count": ids.len(),
                "stations": ids.iter().map(|&i| {
                    let s = &c.stations[i as usize];
                    serde_json::json!({ "id": s.id, "name": s.name, "lat": s.lat, "lon": s.lon })
                }).collect::<Vec<_>>(),
            })
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "stations": c.stations.len(),
                "stations_with_several_stops": multi,
                "stations_with_dhid_merges": dhid_merges,
                "members_by_reason": reasons,
                "ambiguity_counts": c.ambiguity_counts,
                "ambiguity_samples": c.ambiguities,
                "clustering_complete": c.complete,
                "attribution": attribution,
                "selection": sel,
            }))?
        );
        return Ok(Outcome::Ok);
    }

    println!("{}", style::bold("Stations"));
    println!("  stations                     {}", c.stations.len());
    println!("  with several stops           {multi}");
    println!("  with DHID-prefix merges      {dhid_merges}");
    for (r, n) in &reasons {
        println!("  members by {r:<18} {n}");
    }
    println!(
        "  same name, far apart         {}",
        c.ambiguity_counts.same_name_far_apart
    );
    println!(
        "  different names, very close  {}",
        c.ambiguity_counts.different_names_close
    );
    if !c.complete {
        println!(
            "  {}",
            style::warn("a work limit stopped clustering early; check for many stops at one place")
        );
    }
    if let Some((name, ids)) = selected {
        println!("{}", style::bold(&format!("Selection: {name}")));
        println!("  target stations              {}", ids.len());
        for &i in &ids {
            let s = &c.stations[i as usize];
            println!("  {:<24} {}", s.id, s.name);
        }
    }
    println!("{}", style::dim(&attribution));
    Ok(Outcome::Ok)
}
