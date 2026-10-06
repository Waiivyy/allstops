use std::io::Write;
use std::path::{Path, PathBuf};

use allstops_gtfs::{Feed, Limits};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::registry::Registry;
use crate::{Outcome, style};

#[derive(clap::Args)]
pub struct Args {
    /// Feed id from the registry, for example `mvv`.
    id: String,
    /// Accept a download whose hash differs from the pin and update the pin.
    #[arg(long)]
    accept_new_hash: bool,
    #[arg(long, default_value_os_t = crate::default_registry())]
    registry: PathBuf,
    /// Folder the downloaded zip is stored in (not committed).
    #[arg(long, default_value_os_t = crate::default_cache())]
    cache: PathBuf,
}

pub fn cache_path(cache: &Path, id: &str) -> PathBuf {
    cache.join(format!("{id}.gtfs.zip"))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn validity_line(bytes: &[u8]) -> String {
    match Feed::from_zip_bytes(bytes, &Limits::default()) {
        Ok(feed) => match feed
            .feed_info
            .and_then(|f| Some((f.start_date?, f.end_date?, f.version)))
        {
            Some((s, e, v)) => format!("{s} to {e} (feed_version {v:?})"),
            None => "no feed_info validity dates".into(),
        },
        Err(e) => format!("unreadable: {e}"),
    }
}

pub fn run(args: Args, json: bool) -> Result<Outcome> {
    let registry = Registry::load(&args.registry)?;
    let mut entry = registry.get(&args.id)?.clone();
    let limit = Limits::default().max_compressed_bytes;

    eprintln!(
        "{} {} ({}, {})",
        style::dim("downloading"),
        entry.name,
        entry.licence,
        entry.url
    );
    let mut response = ureq::get(&entry.url)
        .header(
            "User-Agent",
            concat!("allstops/", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .with_context(|| format!("downloading {}", entry.url))?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .with_context(|| format!("reading {} (limit {limit} bytes)", entry.url))?;
    let hash = sha256_hex(&bytes);
    let dest = cache_path(&args.cache, &entry.id);
    let today = chrono::Local::now().date_naive().to_string();

    let matches = hash == entry.sha256;
    if !matches {
        let old = std::fs::read(&dest).ok();
        eprintln!(
            "{} the download does not match the pinned hash for {}",
            style::warn("hash changed:"),
            entry.id
        );
        eprintln!("  pinned  {} (retrieved {})", entry.sha256, entry.retrieved);
        eprintln!("  new     {hash}");
        match &old {
            Some(o) if sha256_hex(o) == entry.sha256 => {
                eprintln!("  pinned validity: {}", validity_line(o))
            }
            _ => eprintln!(
                "  pinned validity: unknown (pinned file not in {})",
                args.cache.display()
            ),
        }
        eprintln!("  new validity:    {}", validity_line(&bytes));
        if !args.accept_new_hash {
            bail!(
                "refusing to use a feed with a changed hash; rerun with --accept-new-hash to update the pin"
            );
        }
    }

    std::fs::create_dir_all(&args.cache)?;
    let tmp = dest.with_extension("zip.part");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &dest)?;
    if !matches {
        registry.update_pin(&entry.id, &hash, &today)?;
        entry.retrieved = today.clone();
        eprintln!(
            "{} pin updated in {}",
            style::good("ok:"),
            registry.path.display()
        );
    }

    if json {
        println!(
            "{}",
            serde_json::json!({
                "id": entry.id,
                "path": dest,
                "bytes": bytes.len(),
                "sha256": hash,
                "pin_updated": !matches,
            })
        );
    } else {
        println!(
            "{} {} ({} bytes, sha256 {})",
            style::good("fetched"),
            dest.display(),
            bytes.len(),
            &hash[..16]
        );
        let version = Feed::from_zip_bytes(&bytes, &Limits::default())
            .ok()
            .and_then(|f| f.feed_info.map(|i| i.version))
            .unwrap_or_default();
        println!("{}", style::dim(&entry.render_attribution(&version)));
    }
    Ok(Outcome::Ok)
}
