//! Pack format comparison on a real network: postcard (serde) against rkyv
//! (zero-copy archive with bytecheck validation), with serde_json as a
//! reference. Measures encoded and deflated size, encode and decode time,
//! and whether the bytes are reproducible.

mod mirror;

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::hint::black_box;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use allstops_core::network::Network;
use allstops_gtfs::calendar::ServiceCalendar;
use allstops_gtfs::cluster::{ClusterConfig, cluster};
use allstops_gtfs::network::build_network;
use allstops_gtfs::select::select;
use anyhow::{Context, Result, ensure};
use flate2::Compression;
use flate2::write::DeflateEncoder;
use rkyv::rancor;
use serde::Serialize;

use crate::real::{Real, visit_types};

#[derive(clap::Args)]
pub struct Args {
    #[arg(long, default_value = "data/cache/mvv.gtfs.zip")]
    zip: PathBuf,
    #[arg(long, default_value = "data/rules/mvv-ubahn.toml")]
    rules: PathBuf,
    #[arg(long)]
    date: Option<String>,
    /// Timed runs per measurement, after one warm-up run; the median is reported.
    #[arg(long, default_value_t = 5)]
    runs: usize,
    /// Where to write the JSON results.
    #[arg(long, default_value = "eval/out/pack.json")]
    out: PathBuf,
}

#[derive(Serialize)]
struct NetStats {
    stations: usize,
    stops: usize,
    trips: usize,
    connections: usize,
    footpaths: usize,
    targets: usize,
}

#[derive(Serialize)]
struct Measured {
    format: &'static str,
    /// Version of the format crate in Cargo.lock.
    version: Option<String>,
    bytes: usize,
    deflate6_bytes: usize,
    deflate9_bytes: usize,
    /// From a core `Network` to bytes.
    encode_ms: f64,
    /// Validated zero-copy access to the archived root; formats without
    /// zero-copy access have none.
    access_ms: Option<f64>,
    /// From bytes to a core `Network`.
    decode_ms: f64,
    /// Encoding the same network twice gives identical bytes.
    deterministic: bool,
    /// A second network built from the same feed encodes to identical bytes.
    rebuild_identical: bool,
    /// The decoded network has the same postcard encoding as the original.
    round_trip: bool,
    /// Partial steps of `encode_ms` and `decode_ms`, in milliseconds.
    steps_ms: BTreeMap<&'static str, f64>,
}

#[derive(Serialize)]
struct Report {
    date: String,
    network: NetStats,
    build_ms: f64,
    runs: usize,
    formats: Vec<Measured>,
}

pub fn run(a: Args) -> Result<()> {
    ensure!(a.runs >= 1, "--runs must be at least 1");
    let real = crate::real::load(&a.zip, &a.rules, a.date.as_deref())?;
    let rebuilt = rebuild(&real)?;
    let net = &real.network;
    let stats = NetStats {
        stations: net.stations.len(),
        stops: net.stops.len(),
        trips: net.trips.len(),
        connections: net.connections.len(),
        footpaths: net.footpaths.len(),
        targets: net.targets.len(),
    };
    println!(
        "network {}: {} stations, {} stops, {} trips, {} connections, {} footpaths, {} targets; built in {:.0} ms",
        real.report.date,
        stats.stations,
        stats.stops,
        stats.trips,
        stats.connections,
        stats.footpaths,
        stats.targets,
        real.build_ms
    );
    let formats = vec![
        postcard_pack(net, &rebuilt, a.runs)?,
        rkyv_pack(net, &rebuilt, a.runs)?,
        json_pack(net, &rebuilt, a.runs)?,
    ];
    print_table(&formats, a.runs);
    let report = Report {
        date: real.report.date.to_string(),
        network: stats,
        build_ms: real.build_ms,
        runs: a.runs,
        formats,
    };
    write_json(&a.out, &report)?;
    println!("\nwrote {}", a.out.display());
    Ok(())
}

/// Build the network a second time from the loaded feed, with fresh
/// clustering and hash map state, to see whether a pack is reproducible.
fn rebuild(real: &Real) -> Result<Network> {
    let clustering = cluster(&real.feed, &ClusterConfig::default());
    let targets = select(&real.feed, &clustering, &real.selection)?;
    let cal = ServiceCalendar::new(&real.feed);
    let (network, _) = build_network(
        &real.feed,
        &cal,
        &clustering,
        &targets,
        &visit_types(&real.selection),
        &real.rules,
    )?;
    Ok(network)
}

fn postcard_pack(net: &Network, rebuilt: &Network, runs: usize) -> Result<Measured> {
    let bytes = postcard::to_allocvec(net)?;
    let decoded: Network = postcard::from_bytes(&bytes)?;
    Ok(Measured {
        format: "postcard",
        version: locked_version("postcard"),
        bytes: bytes.len(),
        deflate6_bytes: deflate_len(&bytes, Compression::new(6))?,
        deflate9_bytes: deflate_len(&bytes, Compression::new(9))?,
        encode_ms: median_ms(runs, || postcard::to_allocvec(net))?,
        access_ms: None,
        decode_ms: median_ms(runs, || postcard::from_bytes::<Network>(&bytes))?,
        deterministic: postcard::to_allocvec(net)? == bytes,
        rebuild_identical: postcard::to_allocvec(rebuilt)? == bytes,
        round_trip: postcard::to_allocvec(&decoded)? == bytes,
        steps_ms: BTreeMap::new(),
    })
}

fn rkyv_bytes(net: &Network) -> Result<rkyv::util::AlignedVec, rancor::Error> {
    rkyv::to_bytes::<rancor::Error>(&mirror::Network::from(net))
}

fn rkyv_pack(net: &Network, rebuilt: &Network, runs: usize) -> Result<Measured> {
    let bytes = rkyv_bytes(net)?;
    let owned = mirror::Network::from(net);
    let archived = rkyv::access::<mirror::ArchivedNetwork, rancor::Error>(&bytes)?;
    let decoded = Network::from(rkyv::deserialize::<mirror::Network, rancor::Error>(
        archived,
    )?);
    let mut steps_ms = BTreeMap::new();
    steps_ms.insert(
        "copy_to_mirror",
        median_ms(runs, || Ok::<_, Infallible>(mirror::Network::from(net)))?,
    );
    steps_ms.insert(
        "serialize_mirror",
        median_ms(runs, || rkyv::to_bytes::<rancor::Error>(&owned))?,
    );
    steps_ms.insert(
        "validate_and_deserialize_mirror",
        median_ms(runs, || {
            rkyv::from_bytes::<mirror::Network, rancor::Error>(&bytes)
        })?,
    );
    steps_ms.insert(
        "deserialize_mirror_unvalidated",
        median_ms(runs, || {
            rkyv::deserialize::<mirror::Network, rancor::Error>(archived)
        })?,
    );
    Ok(Measured {
        format: "rkyv",
        version: locked_version("rkyv"),
        bytes: bytes.len(),
        deflate6_bytes: deflate_len(&bytes, Compression::new(6))?,
        deflate9_bytes: deflate_len(&bytes, Compression::new(9))?,
        encode_ms: median_ms(runs, || rkyv_bytes(net))?,
        access_ms: Some(median_ms(runs, || {
            rkyv::access::<mirror::ArchivedNetwork, rancor::Error>(&bytes)
        })?),
        decode_ms: median_ms(runs, || {
            rkyv::from_bytes::<mirror::Network, rancor::Error>(&bytes).map(Network::from)
        })?,
        deterministic: rkyv_bytes(net)?[..] == bytes[..],
        rebuild_identical: rkyv_bytes(rebuilt)?[..] == bytes[..],
        round_trip: postcard::to_allocvec(&decoded)? == postcard::to_allocvec(net)?,
        steps_ms,
    })
}

fn json_pack(net: &Network, rebuilt: &Network, runs: usize) -> Result<Measured> {
    let bytes = serde_json::to_vec(net)?;
    let decoded: Network = serde_json::from_slice(&bytes)?;
    Ok(Measured {
        format: "serde_json (reference)",
        version: locked_version("serde_json"),
        bytes: bytes.len(),
        deflate6_bytes: deflate_len(&bytes, Compression::new(6))?,
        deflate9_bytes: deflate_len(&bytes, Compression::new(9))?,
        encode_ms: median_ms(runs, || serde_json::to_vec(net))?,
        access_ms: None,
        decode_ms: median_ms(runs, || serde_json::from_slice::<Network>(&bytes))?,
        deterministic: serde_json::to_vec(net)? == bytes,
        rebuild_identical: serde_json::to_vec(rebuilt)? == bytes,
        round_trip: postcard::to_allocvec(&decoded)? == postcard::to_allocvec(net)?,
        steps_ms: BTreeMap::new(),
    })
}

/// Median wall time in milliseconds of `runs` calls of `f`, after one
/// warm-up call. Dropping the result is not timed.
fn median_ms<T, E>(runs: usize, mut f: impl FnMut() -> Result<T, E>) -> Result<f64, E> {
    drop(black_box(f()?));
    let mut ms = Vec::with_capacity(runs);
    for _ in 0..runs {
        let t = Instant::now();
        let out = black_box(f()?);
        ms.push(t.elapsed().as_secs_f64() * 1e3);
        drop(out);
    }
    ms.sort_by(f64::total_cmp);
    Ok(ms[ms.len() / 2])
}

fn deflate_len(bytes: &[u8], level: Compression) -> Result<usize> {
    let mut e = DeflateEncoder::new(Vec::new(), level);
    e.write_all(bytes)?;
    Ok(e.finish()?.len())
}

/// The version of `name` recorded in the workspace's Cargo.lock.
fn locked_version(name: &str) -> Option<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.lock");
    let lock: toml::Table = toml::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    lock.get("package")?
        .as_array()?
        .iter()
        .find(|p| p.get("name").and_then(|n| n.as_str()) == Some(name))?
        .get("version")?
        .as_str()
        .map(String::from)
}

fn print_table(formats: &[Measured], runs: usize) {
    println!(
        "\nmedian of {runs} runs, single-threaded\n\n\
         | format | version | bytes | deflate -6 | deflate -9 | encode ms | validated access ms | decode ms | deterministic | same after rebuild | round trip |"
    );
    println!("|---|---|---:|---:|---:|---:|---:|---:|---|---|---|");
    let yes = |b: bool| if b { "yes" } else { "no" };
    for m in formats {
        println!(
            "| {} | {} | {} | {} | {} | {:.1} | {} | {:.1} | {} | {} | {} |",
            m.format,
            m.version.as_deref().unwrap_or("?"),
            m.bytes,
            m.deflate6_bytes,
            m.deflate9_bytes,
            m.encode_ms,
            m.access_ms
                .map(|x| format!("{x:.1}"))
                .unwrap_or_else(|| "n/a".into()),
            m.decode_ms,
            yes(m.deterministic),
            yes(m.rebuild_identical),
            yes(m.round_trip),
        );
    }
    for m in formats.iter().filter(|m| !m.steps_ms.is_empty()) {
        println!("\n{} steps (median ms):", m.format);
        for (k, v) in &m.steps_ms {
            println!("- {k}: {v:.1}");
        }
    }
}

fn write_json(path: &Path, report: &Report) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(report)?;
    std::fs::write(path, text + "\n").with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use allstops_core::builder::random;

    /// A random network whose fields all hold distinct values, so a
    /// conversion that swaps or drops a field changes the encoding.
    fn distinct(seed: u64) -> Network {
        let mut n = random::network(seed, 12);
        for (i, s) in n.stations.iter_mut().enumerate() {
            s.name = format!("Name {i}");
            s.lat = 48.0 + i as f64 / 7.0;
            s.lon = 11.0 - i as f64 / 9.0;
        }
        for (i, s) in n.stops.iter_mut().enumerate() {
            s.id = format!("stop:{i}");
            s.platform = format!("P{i}");
        }
        for (i, t) in n.trips.iter_mut().enumerate() {
            t.service_date = format!("2026-11-{:02}", 1 + i % 28);
            t.offset = 86_400 - i as i32;
            t.route = format!("U{i}");
            t.headsign = format!("Headsign {i}");
        }
        for (i, c) in n.connections.iter_mut().enumerate() {
            c.dep_stop = 1000 + i as u32;
            c.arr_stop = 2000 + i as u32;
        }
        for (i, f) in n.footpaths.iter_mut().enumerate() {
            f.metres = 0.5 + i as f32;
        }
        n.window_start = 17;
        n.window_end = 99_999;
        n
    }

    #[test]
    fn postcard_round_trip_reproduces_the_bytes() {
        for seed in 0..20 {
            let n = distinct(seed);
            let bytes = postcard::to_allocvec(&n).unwrap();
            let back: Network = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(postcard::to_allocvec(&back).unwrap(), bytes, "seed {seed}");
            assert_eq!(postcard::to_allocvec(&n).unwrap(), bytes, "seed {seed}");
        }
    }

    #[test]
    fn rkyv_mirror_round_trip_keeps_every_field() {
        for seed in 0..20 {
            let n = distinct(seed);
            let bytes = rkyv_bytes(&n).unwrap();
            let back =
                Network::from(rkyv::from_bytes::<mirror::Network, rancor::Error>(&bytes).unwrap());
            assert_eq!(
                postcard::to_allocvec(&back).unwrap(),
                postcard::to_allocvec(&n).unwrap(),
                "seed {seed}"
            );
            assert_eq!(rkyv_bytes(&n).unwrap()[..], bytes[..], "seed {seed}");
        }
    }

    #[test]
    fn rkyv_access_rejects_a_truncated_archive() {
        let bytes = rkyv_bytes(&distinct(3)).unwrap();
        let mut cut = rkyv::util::AlignedVec::<16>::new();
        cut.extend_from_slice(&bytes[..bytes.len() / 2]);
        assert!(rkyv::access::<mirror::ArchivedNetwork, rancor::Error>(&cut).is_err());
    }

    #[test]
    fn median_ignores_one_slow_run() {
        let mut calls = 0;
        let ms = median_ms(5, || {
            calls += 1;
            if calls == 2 {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Ok::<_, Infallible>(())
        })
        .unwrap();
        assert_eq!(calls, 6);
        assert!(ms < 50.0, "{ms}");
    }
}
