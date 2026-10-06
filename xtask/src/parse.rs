//! GTFS loader comparison: the project's own loader against the
//! gtfs-structures crate, both reading the same zip bytes from memory.
//!
//! Every measurement runs in a fresh child process (this binary re-invoked
//! with the hidden `--child` flag) so that the peak resident memory a child
//! reports belongs to one loader alone. A baseline child that only reads the
//! zip into memory separates process and input overhead from the loader.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use allstops_gtfs::{Feed, Limits};
use anyhow::{Context, Result, bail};
use gtfs_structures::{Gtfs, GtfsReader, RawGtfs};
use serde::{Deserialize, Serialize};

#[derive(clap::Args)]
pub struct Args {
    #[arg(long, default_value = "data/cache/mvv.gtfs.zip")]
    zip: PathBuf,
    /// Child processes per loader; the table reports the median.
    #[arg(long, default_value_t = 3)]
    runs: usize,
    #[arg(long, default_value = "eval/out/parse.json")]
    out: PathBuf,
    /// Internal: load the feed once with this loader and print one JSON line.
    #[arg(long, value_enum, hide = true)]
    child: Option<Loader>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
enum Loader {
    Baseline,
    Allstops,
    GtfsStructuresRaw,
    GtfsStructures,
}

impl Loader {
    const ALL: [Loader; 4] = [
        Loader::Baseline,
        Loader::Allstops,
        Loader::GtfsStructuresRaw,
        Loader::GtfsStructures,
    ];

    fn name(self) -> &'static str {
        match self {
            Loader::Baseline => "baseline",
            Loader::Allstops => "allstops",
            Loader::GtfsStructuresRaw => "gtfs-structures-raw",
            Loader::GtfsStructures => "gtfs-structures",
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Loader::Baseline => "zip bytes read into memory, nothing parsed",
            Loader::Allstops => "allstops_gtfs::Feed::from_zip_bytes, default limits",
            Loader::GtfsStructuresRaw => {
                "GtfsReader::read_shapes(false).raw().read_from_reader(Cursor)"
            }
            Loader::GtfsStructures => "the raw read above, then Gtfs::try_from",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
struct Counts {
    stops: u64,
    routes: u64,
    trips: u64,
    stop_times: u64,
}

/// One child process: time of the load call alone (none for the baseline)
/// and the process's peak resident set size.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct Sample {
    load_ms: Option<f64>,
    peak_rss_bytes: u64,
    counts: Option<Counts>,
}

#[derive(Serialize)]
struct Summary {
    loader: &'static str,
    description: &'static str,
    samples: Vec<Sample>,
    median_load_ms: Option<f64>,
    median_peak_rss_bytes: Option<u64>,
    /// Median peak minus the baseline child's median peak.
    rss_over_baseline_bytes: Option<i64>,
    counts: Option<Counts>,
    error: Option<String>,
}

#[derive(Serialize)]
struct Report {
    feed: String,
    feed_bytes: u64,
    runs: usize,
    os: &'static str,
    arch: &'static str,
    method: &'static str,
    counts_match: Option<bool>,
    loaders: Vec<Summary>,
}

pub fn run(a: Args) -> Result<()> {
    if let Some(loader) = a.child {
        return child(&a.zip, loader);
    }
    if a.runs == 0 {
        bail!("--runs must be at least 1");
    }
    let exe = std::env::current_exe().context("locating the xtask binary")?;
    let feed_bytes = std::fs::metadata(&a.zip)
        .with_context(|| format!("reading {}", a.zip.display()))?
        .len();

    let mut samples: Vec<Vec<Sample>> = vec![Vec::new(); Loader::ALL.len()];
    let mut errors: Vec<Option<String>> = vec![None; Loader::ALL.len()];
    // Loaders take turns within each run so slow drift in machine state
    // spreads over all of them instead of landing on one.
    for run in 0..a.runs {
        for (i, &loader) in Loader::ALL.iter().enumerate() {
            match spawn(&exe, &a.zip, loader) {
                Ok(s) => samples[i].push(s),
                Err(e) => errors[i] = Some(format!("{e:#}")),
            }
        }
        eprintln!("run {}/{} done", run + 1, a.runs);
    }

    let baseline_rss = median_u64(samples[0].iter().map(|s| s.peak_rss_bytes));
    let mut loaders = Vec::new();
    for (i, &loader) in Loader::ALL.iter().enumerate() {
        let s = std::mem::take(&mut samples[i]);
        let counts = s.first().and_then(|x| x.counts);
        if s.iter().any(|x| x.counts != counts) {
            bail!("{} reported different counts across runs", loader.name());
        }
        let rss = median_u64(s.iter().map(|x| x.peak_rss_bytes));
        loaders.push(Summary {
            loader: loader.name(),
            description: loader.describe(),
            median_load_ms: median_f64(s.iter().filter_map(|x| x.load_ms)),
            median_peak_rss_bytes: rss,
            rss_over_baseline_bytes: rss.zip(baseline_rss).map(|(r, b)| r as i64 - b as i64),
            counts,
            error: errors[i].take(),
            samples: s,
        });
    }
    let counts_of = |l: Loader| {
        loaders
            .iter()
            .find(|s| s.loader == l.name())
            .and_then(|s| s.counts)
    };
    let counts_match = match (
        counts_of(Loader::Allstops),
        counts_of(Loader::GtfsStructuresRaw),
        counts_of(Loader::GtfsStructures),
    ) {
        (Some(a), Some(r), Some(g)) => Some(a == r && a == g),
        (Some(a), Some(r), None) => Some(a == r),
        _ => None,
    };

    let report = Report {
        feed: a
            .zip
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        feed_bytes,
        runs: a.runs,
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        method: "one fresh child process per sample; load_ms times only the load call \
                 with the zip already in memory; peak RSS from getrusage(RUSAGE_SELF) \
                 in the child",
        counts_match,
        loaders,
    };
    print_table(&report);
    if let Some(dir) = a.out.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&a.out, serde_json::to_string_pretty(&report)? + "\n")
        .with_context(|| format!("writing {}", a.out.display()))?;
    eprintln!("wrote {}", a.out.display());
    Ok(())
}

fn spawn(exe: &Path, zip: &Path, loader: Loader) -> Result<Sample> {
    let out = Command::new(exe)
        .arg("parse")
        .arg("--zip")
        .arg(zip)
        .arg("--child")
        .arg(loader.name())
        .output()
        .with_context(|| format!("starting the {} child", loader.name()))?;
    if !out.status.success() {
        bail!(
            "{} child failed ({}): {}",
            loader.name(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let line = String::from_utf8(out.stdout).context("child output is not UTF-8")?;
    serde_json::from_str(line.trim())
        .with_context(|| format!("parsing {} child output {line:?}", loader.name()))
}

fn child(zip: &Path, loader: Loader) -> Result<()> {
    let bytes = std::fs::read(zip).with_context(|| format!("reading {}", zip.display()))?;
    // Each arm stops the clock before the loaded data is dropped, so freeing
    // it is not counted as loading.
    let (load_ms, counts) = match loader {
        Loader::Baseline => (None, None),
        Loader::Allstops => {
            let t = Instant::now();
            let feed = Feed::from_zip_bytes(&bytes, &Limits::default())?;
            let ms = ms_since(t);
            let c = Counts {
                stops: feed.stops.len() as u64,
                routes: feed.routes.len() as u64,
                trips: feed.trips.len() as u64,
                stop_times: feed.stop_times.len() as u64,
            };
            (Some(ms), Some(c))
        }
        Loader::GtfsStructuresRaw => {
            let t = Instant::now();
            let raw = read_raw(&bytes)?;
            let ms = ms_since(t);
            (Some(ms), Some(raw_counts(&raw)?))
        }
        Loader::GtfsStructures => {
            let t = Instant::now();
            let gtfs = Gtfs::try_from(read_raw(&bytes)?)?;
            let ms = ms_since(t);
            let c = Counts {
                stops: gtfs.stops.len() as u64,
                routes: gtfs.routes.len() as u64,
                trips: gtfs.trips.len() as u64,
                stop_times: gtfs.trips.values().map(|t| t.stop_times.len() as u64).sum(),
            };
            (Some(ms), Some(c))
        }
    };
    let sample = Sample {
        load_ms,
        peak_rss_bytes: peak_rss_bytes()?,
        counts,
    };
    println!("{}", serde_json::to_string(&sample)?);
    Ok(())
}

/// gtfs-structures' in-memory entry point. Shapes are skipped since the
/// reader makes them optional; everything else it knows is read.
fn read_raw(bytes: &[u8]) -> Result<RawGtfs> {
    Ok(GtfsReader::default()
        .read_shapes(false)
        .raw()
        .read_from_reader(Cursor::new(bytes))?)
}

fn raw_counts(raw: &RawGtfs) -> Result<Counts> {
    fn len<T>(r: &Result<Vec<T>, gtfs_structures::Error>, what: &str) -> Result<u64> {
        match r {
            Ok(v) => Ok(v.len() as u64),
            Err(e) => bail!("{what}: {e}"),
        }
    }
    Ok(Counts {
        stops: len(&raw.stops, "stops.txt")?,
        routes: len(&raw.routes, "routes.txt")?,
        trips: len(&raw.trips, "trips.txt")?,
        stop_times: len(&raw.stop_times, "stop_times.txt")?,
    })
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

/// Peak resident set size of this process so far.
#[cfg(unix)]
fn peak_rss_bytes() -> Result<u64> {
    let mut ru = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: the pointer is valid for one `rusage`, which getrusage fills
    // in on success.
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, ru.as_mut_ptr()) };
    if rc != 0 {
        bail!("getrusage failed: {}", std::io::Error::last_os_error());
    }
    // SAFETY: zero-initialised above and filled in by a successful call.
    let max = unsafe { ru.assume_init() }.ru_maxrss as u64;
    // ru_maxrss is in bytes on macOS and in kilobytes on Linux and the BSDs.
    Ok(if cfg!(target_vendor = "apple") {
        max
    } else {
        max * 1024
    })
}

#[cfg(not(unix))]
fn peak_rss_bytes() -> Result<u64> {
    bail!("peak memory needs getrusage, which this platform does not have")
}

fn median_f64(xs: impl Iterator<Item = f64>) -> Option<f64> {
    let mut v: Vec<f64> = xs.collect();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => Some(v[n / 2]),
        _ => Some((v[n / 2 - 1] + v[n / 2]) / 2.0),
    }
}

fn median_u64(xs: impl Iterator<Item = u64>) -> Option<u64> {
    let mut v: Vec<u64> = xs.collect();
    v.sort_unstable();
    let n = v.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => Some(v[n / 2]),
        _ => Some((v[n / 2 - 1] + v[n / 2]) / 2),
    }
}

fn mib(b: f64) -> String {
    format!("{:.0}", b / f64::from(1 << 20))
}

fn print_table(r: &Report) {
    println!(
        "feed {} ({} bytes compressed), {} runs per loader, {}/{}",
        r.feed, r.feed_bytes, r.runs, r.os, r.arch
    );
    println!(
        "| loader | median load ms | load ms per run | median peak RSS MiB | over baseline MiB \
         | stops | routes | trips | stop_times |"
    );
    println!("|---|---|---|---|---|---|---|---|---|");
    for l in &r.loaders {
        let dash = || "-".to_string();
        let per_run = l
            .samples
            .iter()
            .filter_map(|s| s.load_ms)
            .map(|ms| format!("{ms:.0}"))
            .collect::<Vec<_>>()
            .join(", ");
        let count = |f: fn(&Counts) -> u64| {
            l.counts
                .as_ref()
                .map(f)
                .map_or_else(dash, |c| c.to_string())
        };
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            l.loader,
            l.median_load_ms.map_or_else(dash, |m| format!("{m:.0}")),
            if per_run.is_empty() { dash() } else { per_run },
            l.median_peak_rss_bytes.map_or_else(dash, |b| mib(b as f64)),
            l.rss_over_baseline_bytes
                .map_or_else(dash, |b| mib(b as f64)),
            count(|c| c.stops),
            count(|c| c.routes),
            count(|c| c.trips),
            count(|c| c.stop_times),
        );
    }
    for l in r.loaders.iter().filter(|l| l.error.is_some()) {
        println!("{} failed: {}", l.loader, l.error.as_deref().unwrap_or(""));
    }
    match r.counts_match {
        Some(true) => println!("stops, routes, trips and stop_times match across loaders"),
        Some(false) => println!("COUNTS DIFFER between loaders"),
        None => println!("counts could not be compared"),
    }
}

#[cfg(test)]
mod tests {
    use allstops_gtfs::fixture::minimal_with;

    use super::*;

    fn counts_both(bytes: &[u8]) -> (Counts, Counts) {
        let feed = Feed::from_zip_bytes(bytes, &Limits::default()).unwrap();
        let gtfs = Gtfs::try_from(read_raw(bytes).unwrap()).unwrap();
        let ours = Counts {
            stops: feed.stops.len() as u64,
            routes: feed.routes.len() as u64,
            trips: feed.trips.len() as u64,
            stop_times: feed.stop_times.len() as u64,
        };
        let theirs = Counts {
            stops: gtfs.stops.len() as u64,
            routes: gtfs.routes.len() as u64,
            trips: gtfs.trips.len() as u64,
            stop_times: gtfs.trips.values().map(|t| t.stop_times.len() as u64).sum(),
        };
        (ours, theirs)
    }

    #[test]
    fn medians_of_odd_even_and_empty_inputs() {
        assert_eq!(median_f64([3.0, 1.0, 2.0].into_iter()), Some(2.0));
        assert_eq!(median_f64([4.0, 1.0, 3.0, 2.0].into_iter()), Some(2.5));
        assert_eq!(median_f64(std::iter::empty()), None);
        assert_eq!(median_u64([30, 10, 20].into_iter()), Some(20));
        assert_eq!(median_u64([40, 10, 30, 20].into_iter()), Some(25));
        assert_eq!(median_u64(std::iter::empty()), None);
    }

    #[cfg(unix)]
    #[test]
    fn peak_rss_grows_with_touched_memory() {
        let before = peak_rss_bytes().unwrap();
        assert!(before > 0);
        // Touch 64 MiB more than the process has ever held, so the
        // high-water mark has to move if the unit conversion is right.
        let big = vec![1u8; before as usize + (64 << 20)];
        std::hint::black_box(&big);
        let after = peak_rss_bytes().unwrap();
        assert!(after >= before + (32 << 20), "{before} -> {after}");
    }

    #[test]
    fn loaders_agree_on_the_minimal_feed() {
        let (ours, theirs) = counts_both(&minimal_with(&[]));
        assert_eq!(ours, theirs);
        assert_eq!(ours.stop_times, 2);
    }

    #[test]
    fn loaders_agree_with_bom_quoted_headers_and_late_times() {
        // The MVV feed starts every file with a byte-order mark and quotes
        // every header and field.
        let bytes = minimal_with(&[(
            "stop_times.txt",
            "\u{feff}\"trip_id\",\"arrival_time\",\"departure_time\",\"stop_id\",\"stop_sequence\"\n\
             \"T1\",\"24:50:00\",\"24:50:00\",\"S1a\",\"1\"\n\
             \"T1\",\"25:05:00\",\"25:05:00\",\"S2a\",\"2\"\n",
        )]);
        let (ours, theirs) = counts_both(&bytes);
        assert_eq!(ours, theirs);
        let gtfs = Gtfs::try_from(read_raw(&bytes).unwrap()).unwrap();
        let st = &gtfs.trips["T1"].stop_times;
        assert_eq!(st[1].arrival_time, Some(25 * 3600 + 5 * 60));
    }
}
