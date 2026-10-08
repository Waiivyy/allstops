//! `cargo xtask bench`: the same metrics for every stage, on seeded
//! synthetic networks and on the real networks listed in eval/bench.toml.
//!
//! Every instance goes through the production pipeline: load the GTFS zip,
//! cluster stops into stations, select the targets, build the plan-date
//! network, run the greedy from every target at several start times, write
//! the canonical itinerary and check it with the independent verifier the
//! way `allstops solve` does. Every instance with a route gets both lower
//! bounds; tiny synthetic instances also get the brute-force optimum. All
//! runs are single-threaded.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use allstops_core::bound::{lower_bound, profile_lower_bound};
use allstops_core::csa::{Csa, JLeg};
use allstops_core::itinerary::{FeedRef, Itinerary, to_itinerary};
use allstops_core::network::{Network, StationIdx, Time};
use allstops_core::oracle;
use allstops_core::plan::{Best, best_greedy, greedy_from, greedy_jobs, transfer_slacks};
use allstops_core::rules::{Rules, parse_clock};
use allstops_gtfs::calendar::ServiceCalendar;
use allstops_gtfs::cluster::Clustering;
use allstops_gtfs::network::build_network;
use allstops_gtfs::select::{Rule, Selection, select};
use allstops_gtfs::walks::WalkOverrides;
use allstops_gtfs::{Feed, Limits};
use allstops_verify::{Report, parse, verify};
use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::real::{RuleFiles, load_rules, visit_types};
use crate::synth::{self, Shape, Size};

/// Start times tried per start station and the step between them: the
/// defaults of `allstops solve`.
const START_COUNT: i32 = 12;
const START_STEP_S: Time = 600;
/// The real network `--mvv-zip` applies to.
const MVV_ID: &str = "mvv-ubahn";

#[derive(clap::Args)]
pub struct Args {
    /// Number of tiny synthetic seeds; small ones are a quarter as many,
    /// rounded up [default: 200 tiny and 50 small]
    #[arg(long)]
    seeds: Option<u64>,
    /// Run only the synthetic or only the real networks.
    #[arg(long, value_enum)]
    only: Option<Only>,
    /// Fewer seeds (20 tiny, 5 small), for CI. Quick and partial runs write
    /// their summary to eval/out/RESULTS.md instead of eval/RESULTS.md.
    #[arg(long)]
    quick: bool,
    /// GTFS zip of the Munich feed, instead of ALLSTOPS_MVV_ZIP or the path
    /// in eval/bench.toml.
    #[arg(long)]
    mvv_zip: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Only {
    Synthetic,
    Real,
}

/// eval/bench.toml.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default)]
    network: Vec<RealNetwork>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RealNetwork {
    id: String,
    name: String,
    /// Relative to the repository root.
    zip: PathBuf,
    /// Environment variable that overrides `zip`.
    #[serde(default)]
    zip_env: Option<String>,
    rules: PathBuf,
    dates: Vec<String>,
}

/// One instance: a synthetic seed or a real network on one date.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Row {
    /// `tiny`, `small` or the real network's id.
    pub group: String,
    pub name: String,
    pub seed: Option<u64>,
    pub date: String,
    pub shape: Option<Shape>,
    pub targets: usize,
    pub stations: usize,
    pub connections: usize,
    pub footpaths: usize,
    pub unserved_targets: usize,
    pub greedy_runs: usize,
    pub feasible_runs: usize,
    /// Duration of the best greedy route, which the verifier recomputed.
    pub best_s: Option<Time>,
    pub static_bound_s: Option<Time>,
    pub profile_bound_s: Option<Time>,
    /// The larger of the two bounds.
    pub lower_bound_s: Option<Time>,
    /// (best - lower bound) / lower bound.
    pub gap: Option<f64>,
    pub optimum_s: Option<Time>,
    pub greedy_is_optimal: Option<bool>,
    /// (best - optimum) / optimum.
    pub gap_to_optimum: Option<f64>,
    pub rides: Option<usize>,
    pub transfers: Option<usize>,
    /// Changes with less slack than the rules' tight_transfer_s.
    pub tight_transfers: Option<usize>,
    pub min_slack_s: Option<Time>,
    pub walk_m: Option<f64>,
    /// Both the first and the best route passed the verifier.
    pub verified: Option<bool>,
    /// Parsing the feed (once per real network, shared by its dates).
    pub load_ms: f64,
    /// Clustering, selection and the plan-date network.
    pub build_ms: f64,
    /// Every greedy run.
    pub solve_ms: f64,
    /// The first greedy run that visits every target, its itinerary and the
    /// verifier.
    pub first_ms: Option<f64>,
    /// Every greedy run, then itinerary and verifier for the best.
    pub best_ms: Option<f64>,
    pub bound_ms: f64,
    pub optimum_ms: Option<f64>,
    /// Broken invariants. The command fails when any row has one.
    pub problems: Vec<String>,
}

/// A loaded feed and what itineraries built from it carry.
struct Source {
    feed: Feed,
    sha256: String,
    feed_ref: FeedRef,
    timezone: String,
    load_ms: f64,
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn load_source(bytes: &[u8], id: &str) -> Result<Source> {
    let t = Instant::now();
    let feed = Feed::from_zip_bytes(bytes, &Limits::default())?;
    let load_ms = ms(t);
    let sha256 = sha256_hex(bytes);
    let info = feed.feed_info.as_ref();
    let feed_ref = FeedRef {
        id: id.to_string(),
        sha256: sha256.clone(),
        feed_version: info.map(|f| f.version.clone()).unwrap_or_default(),
        attribution: format!(
            "Timetable data: {}",
            info.map(|f| f.publisher_name.as_str())
                .unwrap_or("see the feed's publisher")
        ),
    };
    let timezone = feed.timezone()?.name().to_string();
    Ok(Source {
        feed,
        sha256,
        feed_ref,
        timezone,
        load_ms,
    })
}

/// The verifier call of `allstops solve` (crates/allstops-cli/src/check.rs):
/// the targets are recomputed from the selection, never read from the
/// itinerary.
fn check_json(
    feed: &Feed,
    clustering: &Clustering,
    selection: &Selection,
    walks: &WalkOverrides,
    rules: &Rules,
    feed_sha256: Option<&str>,
    json: &str,
) -> Result<Report> {
    let expected: allstops_verify::RulesIn = serde_json::from_value(serde_json::to_value(rules)?)?;
    let cal = ServiceCalendar::new(feed);
    let targets: Vec<String> = select(feed, clustering, selection)?
        .into_iter()
        .map(|i| clustering.stations[i as usize].id.clone())
        .collect();
    let types = visit_types(feed, selection)?;
    let ctx = allstops_verify::Context {
        feed,
        calendar: &cal,
        clustering,
        targets: &targets,
        visit_types: &types,
        feed_sha256,
        expected_rules: Some(&expected),
        walks,
    };
    let it = parse(json)?;
    Ok(verify(&ctx, &it))
}

/// Write the itinerary of `best` and run the verifier on it.
fn verify_route(
    src: &Source,
    clustering: &Clustering,
    files: &RuleFiles,
    net: &Network,
    best: &Best,
) -> Result<(Itinerary, Report)> {
    let rules = &files.rules;
    let it = to_itinerary(net, &best.plan, rules, src.feed_ref.clone(), &src.timezone)
        .context("internal error: a greedy plan does not visit every target")?;
    let json = serde_json::to_string_pretty(&it)?;
    let report = check_json(
        &src.feed,
        clustering,
        &files.selection,
        &files.walks,
        rules,
        Some(&src.sha256),
        &json,
    )?;
    Ok((it, report))
}

/// What is wrong with a route according to the verifier.
fn verifier_problems(what: &str, best: &Best, report: &Report) -> Vec<String> {
    let mut out: Vec<String> = report
        .violations
        .iter()
        .map(|v| {
            format!(
                "verifier rejected the {what} route: {} {}",
                v.code, v.message
            )
        })
        .collect();
    let planned = i64::from(best.last - best.first);
    if report.passed && report.duration_s != Some(planned) {
        out.push(format!(
            "verifier recomputed {:?} s for the {what} route, the plan says {planned} s",
            report.duration_s
        ));
    }
    out
}

fn start_stations(net: &Network, rules: &Rules) -> Result<Vec<StationIdx>> {
    if rules.start == "any" {
        return Ok(net.targets.clone());
    }
    match net.stations.iter().position(|s| s.id == rules.start) {
        Some(i) => Ok(vec![i as StationIdx]),
        None => bail!("start station {:?} is not in the network", rules.start),
    }
}

/// Build the network for `rules.date` and measure everything on it.
fn run_instance(src: &Source, files: &RuleFiles, with_optimum: bool, row: &mut Row) -> Result<()> {
    let (selection, rules) = (&files.selection, &files.rules);
    let t = Instant::now();
    let clustering = crate::real::stations(&src.feed, &files.stations)?;
    let targets = select(&src.feed, &clustering, selection)?;
    let cal = ServiceCalendar::new(&src.feed);
    let (net, report) = build_network(
        &src.feed,
        &cal,
        &clustering,
        &targets,
        &visit_types(&src.feed, selection)?,
        rules,
        &files.walks,
    )?;
    row.build_ms = ms(t);
    row.date = rules.date.clone();
    row.load_ms = src.load_ms;
    row.targets = net.targets.len();
    row.stations = net.stations.len();
    row.connections = net.connections.len();
    row.footpaths = net.footpaths.len();
    row.unserved_targets = report.unserved_targets.len();
    if row.unserved_targets > 0 {
        return Ok(());
    }

    let starts = start_stations(&net, rules)?;
    let ws = parse_clock(&rules.earliest_start).context("unreadable earliest_start")?;
    let times: Vec<Time> = (0..START_COUNT).map(|k| ws + k * START_STEP_S).collect();
    row.greedy_runs = starts.len() * times.len();
    let check = |b: &Best| verify_route(src, &clustering, files, &net, b);

    // Time to the first verified route.
    let t = Instant::now();
    let mut csa = Csa::new(&net);
    let first = greedy_jobs(&starts, &times)
        .into_iter()
        .find_map(|(s, t0)| greedy_from(&mut csa, s, t0));
    let first_checked = first.as_ref().map(&check).transpose()?;
    let first_ms = ms(t);

    // Time to the best route.
    let t = Instant::now();
    let (best, feasible) = best_greedy(&net, &starts, &times);
    row.solve_ms = ms(t);
    let best_checked = best.as_ref().map(&check).transpose()?;
    let best_ms = ms(t);
    row.feasible_runs = feasible;

    if let (Some(b), Some((it, rep))) = (&best, &best_checked) {
        row.best_s = Some(b.last - b.first);
        row.first_ms = Some(first_ms);
        row.best_ms = Some(best_ms);
        row.rides = Some(
            b.plan
                .legs
                .iter()
                .filter(|l| matches!(l, JLeg::Ride { .. }))
                .count(),
        );
        row.transfers = Some(it.summary.transfers);
        row.walk_m = Some(it.summary.walk_m);
        let slacks = transfer_slacks(&net, &b.plan);
        row.tight_transfers = Some(
            slacks
                .iter()
                .filter(|c| c.slack < rules.tight_transfer_s)
                .count(),
        );
        row.min_slack_s = slacks.iter().map(|c| c.slack).min();
        let mut problems = verifier_problems("best", b, rep);
        if let (Some(f), Some((_, frep))) = (&first, &first_checked) {
            problems.extend(verifier_problems("first", f, frep));
        }
        row.verified = Some(problems.is_empty());
        row.problems.extend(problems);
    }

    if with_optimum {
        let t = Instant::now();
        row.optimum_s = oracle::optimum(&net);
        row.optimum_ms = Some(ms(t));
    }
    if let Some(upper) = row.best_s.or(row.optimum_s) {
        let t = Instant::now();
        row.static_bound_s = lower_bound(&net, upper).map(|b| b.seconds);
        row.profile_bound_s = profile_lower_bound(&net, upper).map(|b| b.seconds);
        row.bound_ms = ms(t);
        row.lower_bound_s = row.static_bound_s.max(row.profile_bound_s);
    }
    check_invariants(row, with_optimum);
    Ok(())
}

/// Fill in the gaps and record every broken invariant:
/// lower bound <= optimum <= greedy, and lower bound <= greedy.
fn check_invariants(row: &mut Row, with_optimum: bool) {
    if let (Some(b), Some(l)) = (row.best_s, row.lower_bound_s) {
        if l > 0 {
            row.gap = Some(f64::from(b - l) / f64::from(l));
        }
        if l > b {
            row.problems.push(format!(
                "lower bound {l} s exceeds the verified route of {b} s"
            ));
        }
    }
    if let (Some(o), Some(l)) = (row.optimum_s, row.lower_bound_s)
        && l > o
    {
        row.problems
            .push(format!("lower bound {l} s exceeds the optimum {o} s"));
    }
    if let (Some(b), Some(o)) = (row.best_s, row.optimum_s) {
        row.greedy_is_optimal = Some(b == o);
        if o > 0 {
            row.gap_to_optimum = Some(f64::from(b - o) / f64::from(o));
        }
        if b < o {
            row.problems.push(format!(
                "the greedy route of {b} s beats the optimum of {o} s"
            ));
        }
    }
    if with_optimum && row.best_s.is_some() && row.optimum_s.is_none() {
        row.problems
            .push("the brute-force optimum finds no route, but the greedy does".into());
    }
}

pub fn synthetic_selection() -> Selection {
    Selection {
        name: "synthetic metro".into(),
        include: vec![Rule {
            route_types: vec![1],
            ..Rule::default()
        }],
        exclude_stations: Vec::new(),
    }
}

/// The default rules on the synthetic plan date.
pub fn synthetic_rules() -> Rules {
    Rules {
        date: synth::PLAN_DATE.into(),
        ..Rules::default()
    }
}

/// Generate one synthetic network and measure it.
pub fn run_synthetic(seed: u64, size: Size) -> Result<Row> {
    let s = synth::generate(seed, size);
    let id = format!("synthetic-{}-{seed}", size.name());
    let src = load_source(&s.zip, &id)?;
    let mut row = Row {
        group: size.name().into(),
        name: format!("{} seed {seed}", size.name()),
        seed: Some(seed),
        ..Row::default()
    };
    let files = RuleFiles {
        rules: synthetic_rules(),
        selection: synthetic_selection(),
        stations: Default::default(),
        walks: Default::default(),
    };
    run_instance(&src, &files, size == Size::Tiny, &mut row).with_context(|| row.name.clone())?;
    if row.targets != s.shape.metro_stations {
        row.problems.push(format!(
            "the generator made {} metro stations, the selection found {} targets",
            s.shape.metro_stations, row.targets
        ));
    }
    row.shape = Some(s.shape);
    Ok(row)
}

fn command_output(cmd: &str, args: &[&str], dir: &Path) -> Option<String> {
    let out = Command::new(cmd)
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask sits in the repository root")
        .to_path_buf()
}

/// The main checkout when `root` is a linked git worktree.
fn main_checkout(root: &Path) -> Option<PathBuf> {
    let common = command_output(
        "git",
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        root,
    )?;
    let main = Path::new(&common).parent()?.canonicalize().ok()?;
    (main != root.canonicalize().ok()?).then_some(main)
}

/// Where a real network's zip is: `--mvv-zip` (Munich only), else the
/// environment variable named in bench.toml, else the configured path in
/// this checkout, else the same path in the main checkout when this is a
/// git worktree (feeds are never committed, so a new worktree has none).
/// Returns the file, or every place tried.
fn find_zip(net: &RealNetwork, root: &Path, flag: Option<&Path>) -> Result<PathBuf, Vec<PathBuf>> {
    let explicit = flag
        .filter(|_| net.id == MVV_ID)
        .map(Path::to_path_buf)
        .or_else(|| {
            net.zip_env
                .as_deref()
                .and_then(std::env::var_os)
                .map(PathBuf::from)
        });
    let tried = match explicit {
        Some(p) => vec![p],
        None => {
            let mut c = vec![root.join(&net.zip)];
            if net.zip.is_relative()
                && let Some(main) = main_checkout(root)
            {
                c.push(main.join(&net.zip));
            }
            c
        }
    };
    tried.iter().find(|p| p.is_file()).cloned().ok_or(tried)
}

/// A path as shown in committed output: relative to the repository root
/// when inside it, otherwise only its file name, so no local directory
/// names end up in eval/RESULTS.md.
fn shown(p: &Path, root: &Path) -> String {
    if let Ok(rel) = p.strip_prefix(root) {
        return rel.display().to_string();
    }
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned());
    format!(".../{}", name.unwrap_or_default())
}

fn run_real(
    net: &RealNetwork,
    root: &Path,
    flag: Option<&Path>,
    rows: &mut Vec<Row>,
    notes: &mut Vec<String>,
) -> Result<()> {
    let zip = match find_zip(net, root, flag) {
        Ok(z) => z,
        Err(tried) => {
            let tried: Vec<String> = tried.iter().map(|p| shown(p, root)).collect();
            let hint = match &net.zip_env {
                Some(v) if net.id == MVV_ID => format!("set {v} or pass --mvv-zip"),
                Some(v) => format!("set {v}"),
                None => "check eval/bench.toml".into(),
            };
            let msg = format!(
                "Skipped {}: GTFS zip not found (tried {}); {hint}.",
                net.name,
                tried.join(", ")
            );
            eprintln!("{msg}");
            notes.push(msg);
            return Ok(());
        }
    };
    let bytes = std::fs::read(&zip).with_context(|| format!("reading {}", zip.display()))?;
    let src = load_source(&bytes, &net.id)?;
    let files = load_rules(&root.join(&net.rules))?;
    notes.push(format!(
        "{}: feed sha256 `{}`, feed version {}, parsed in {:.0} ms; rules `{}`.",
        net.name,
        src.sha256,
        if src.feed_ref.feed_version.is_empty() {
            "not given"
        } else {
            &src.feed_ref.feed_version
        },
        src.load_ms,
        net.rules.display()
    ));
    for date in &net.dates {
        eprintln!("{}: {date}", net.name);
        let files = RuleFiles {
            rules: Rules {
                date: date.clone(),
                ..files.rules.clone()
            },
            ..files.clone()
        };
        let mut row = Row {
            group: net.id.clone(),
            name: net.name.clone(),
            ..Row::default()
        };
        run_instance(&src, &files, false, &mut row)
            .with_context(|| format!("{} on {date}", net.name))?;
        rows.push(row);
    }
    Ok(())
}

#[derive(Serialize)]
struct Meta {
    command: String,
    date: String,
    commit: String,
    machine: String,
    build: &'static str,
    threads: usize,
    load_average_start: Option<f64>,
    load_average_end: Option<f64>,
}

fn machine() -> String {
    let cpu = command_output(
        "sysctl",
        &["-n", "machdep.cpu.brand_string"],
        Path::new("."),
    )
    .filter(|s| !s.is_empty())
    .or_else(|| {
        let text = std::fs::read_to_string("/proc/cpuinfo").ok()?;
        let line = text.lines().find(|l| l.starts_with("model name"))?;
        Some(line.split(':').nth(1)?.trim().to_string())
    })
    .unwrap_or_else(|| "unknown CPU".into());
    let cores = std::thread::available_parallelism().map_or(0, |n| n.get());
    format!("{cpu}, {cores} cores")
}

/// One-minute load average, to show how busy the machine was.
fn load_average() -> Option<f64> {
    let text = std::fs::read_to_string("/proc/loadavg")
        .ok()
        .or_else(|| command_output("sysctl", &["-n", "vm.loadavg"], Path::new(".")))?;
    text.split_whitespace().find_map(|w| w.parse().ok())
}

fn commit(root: &Path) -> String {
    let Some(hash) = command_output("git", &["rev-parse", "--short=12", "HEAD"], root) else {
        return "unknown".into();
    };
    let dirty = command_output(
        "git",
        &[
            "status",
            "--porcelain",
            "--untracked-files=no",
            "--",
            ".",
            ":(exclude)eval/RESULTS.md",
        ],
        root,
    )
    .is_some_and(|s| !s.is_empty());
    if dirty {
        format!("{hash} with uncommitted changes")
    } else {
        hash
    }
}

/// The command line, with absolute paths cut down to their file names.
fn command_line(root: &Path) -> String {
    let mut out = String::from("cargo xtask");
    for a in std::env::args().skip(1) {
        let shown_arg = match a.split_once('=') {
            Some((k, v)) if Path::new(v).is_absolute() => {
                format!("{k}={}", shown(Path::new(v), root))
            }
            _ if Path::new(&a).is_absolute() => shown(Path::new(&a), root),
            _ => a,
        };
        out.push(' ');
        out.push_str(&shown_arg);
    }
    out
}

/// Aggregates over one group of synthetic rows.
#[derive(Debug, Default, Serialize)]
pub struct Summary {
    pub group: String,
    pub instances: usize,
    /// The greedy found a route.
    pub feasible: usize,
    pub verified: usize,
    /// The brute-force optimum found a route.
    pub with_optimum: usize,
    pub greedy_optimal: usize,
    pub gap_to_optimum_mean: Option<f64>,
    pub gap_to_optimum_max: Option<f64>,
    /// Instances with both a lower bound and an optimum, and how many of
    /// them had bound <= optimum (the run fails unless all do).
    pub bound_vs_optimum: usize,
    pub bound_le_optimum: usize,
    pub gap_mean: Option<f64>,
    pub gap_max: Option<f64>,
    pub targets_mean: Option<f64>,
    pub connections_mean: Option<f64>,
    pub metro_lines_mean: Option<f64>,
    /// Networks with each generated feature.
    pub with_branch: usize,
    pub with_ring: usize,
    pub with_bus: usize,
    pub with_near_miss: usize,
    pub first_ms_median: Option<f64>,
    pub first_ms_max: Option<f64>,
    pub best_ms_median: Option<f64>,
    pub best_ms_max: Option<f64>,
    pub optimum_ms_median: Option<f64>,
    pub optimum_ms_max: Option<f64>,
    pub transfers_mean: Option<f64>,
    pub tight_transfers: usize,
    pub min_slack_s: Option<Time>,
    pub walk_m_mean: Option<f64>,
}

fn mean(xs: &[f64]) -> Option<f64> {
    (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64)
}

fn max(xs: &[f64]) -> Option<f64> {
    xs.iter().copied().reduce(f64::max)
}

fn median(xs: &[f64]) -> Option<f64> {
    let mut v = xs.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => Some(v[n / 2]),
        _ => Some((v[n / 2 - 1] + v[n / 2]) / 2.0),
    }
}

pub fn summarise(group: &str, rows: &[&Row]) -> Summary {
    let col = |f: &dyn Fn(&Row) -> Option<f64>| -> Vec<f64> {
        rows.iter().filter_map(|r| f(r)).collect()
    };
    let has = |f: &dyn Fn(&Shape) -> bool| {
        rows.iter()
            .filter(|r| r.shape.as_ref().is_some_and(f))
            .count()
    };
    let gaps_opt = col(&|r| r.gap_to_optimum);
    let gaps = col(&|r| r.gap);
    let first = col(&|r| r.first_ms);
    let best = col(&|r| r.best_ms);
    let opt_ms = col(&|r| r.optimum_ms);
    let bounded: Vec<&&Row> = rows
        .iter()
        .filter(|r| r.lower_bound_s.is_some() && r.optimum_s.is_some())
        .collect();
    Summary {
        group: group.into(),
        instances: rows.len(),
        feasible: rows.iter().filter(|r| r.best_s.is_some()).count(),
        verified: rows.iter().filter(|r| r.verified == Some(true)).count(),
        with_optimum: rows.iter().filter(|r| r.optimum_s.is_some()).count(),
        greedy_optimal: rows
            .iter()
            .filter(|r| r.greedy_is_optimal == Some(true))
            .count(),
        gap_to_optimum_mean: mean(&gaps_opt),
        gap_to_optimum_max: max(&gaps_opt),
        bound_vs_optimum: bounded.len(),
        bound_le_optimum: bounded
            .iter()
            .filter(|r| r.lower_bound_s <= r.optimum_s)
            .count(),
        gap_mean: mean(&gaps),
        gap_max: max(&gaps),
        targets_mean: mean(&col(&|r| Some(r.targets as f64))),
        connections_mean: mean(&col(&|r| Some(r.connections as f64))),
        metro_lines_mean: mean(&col(&|r| r.shape.as_ref().map(|s| s.metro_lines as f64))),
        with_branch: has(&|s| s.branch),
        with_ring: has(&|s| s.ring),
        with_bus: has(&|s| s.bus),
        with_near_miss: has(&|s| s.near_misses > 0),
        first_ms_median: median(&first),
        first_ms_max: max(&first),
        best_ms_median: median(&best),
        best_ms_max: max(&best),
        optimum_ms_median: median(&opt_ms),
        optimum_ms_max: max(&opt_ms),
        transfers_mean: mean(&col(&|r| r.transfers.map(|t| t as f64))),
        tight_transfers: rows.iter().filter_map(|r| r.tight_transfers).sum(),
        min_slack_s: rows.iter().filter_map(|r| r.min_slack_s).min(),
        walk_m_mean: mean(&col(&|r| r.walk_m)),
    }
}

fn hms(s: Time) -> String {
    format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

fn pct(x: f64) -> String {
    format!("{:.1}%", x * 100.0)
}

fn millis(x: f64) -> String {
    if x < 10.0 {
        format!("{x:.2}")
    } else if x < 100.0 {
        format!("{x:.1}")
    } else {
        format!("{x:.0}")
    }
}

fn or_na<T>(x: Option<T>, f: impl Fn(T) -> String) -> String {
    x.map(f).unwrap_or_else(|| "n/a".into())
}

fn share(k: usize, n: usize) -> String {
    if n == 0 {
        "n/a".into()
    } else {
        format!("{k} / {n} ({})", pct(k as f64 / n as f64))
    }
}

fn pair(a: Option<f64>, b: Option<f64>, f: impl Fn(f64) -> String) -> String {
    match (a, b) {
        (Some(a), Some(b)) => format!("{} / {}", f(a), f(b)),
        _ => "n/a".into(),
    }
}

fn markdown(meta: &Meta, sums: &[Summary], rows: &[Row], notes: &[String]) -> String {
    let mut s = String::new();
    let rules = synthetic_rules();
    let _ = writeln!(s, "# Benchmark results\n");
    let _ = writeln!(
        s,
        "Written by `cargo xtask bench`; rerun the command to refresh this file. \
         Per-instance rows go to `eval/out/bench.json`, which is not committed.\n"
    );
    let _ = writeln!(s, "| Run | |\n|---|---|");
    let _ = writeln!(s, "| Command | `{}` |", meta.command);
    let _ = writeln!(s, "| Date (UTC) | {} |", meta.date);
    let _ = writeln!(s, "| Commit | `{}` |", meta.commit);
    let _ = writeln!(
        s,
        "| Machine | {}; every run single-threaded, {} build |",
        meta.machine, meta.build
    );
    let _ = writeln!(
        s,
        "| Load average (1 min) at start / end | {} / {} |",
        or_na(meta.load_average_start, |x| format!("{x:.2}")),
        or_na(meta.load_average_end, |x| format!("{x:.2}"))
    );
    let _ = writeln!(
        s,
        "| Greedy starts | every target station at {START_COUNT} start times, {} min apart from earliest_start |",
        START_STEP_S / 60
    );
    let _ = writeln!(s, "\n## Caveats\n");
    let _ = writeln!(
        s,
        "- Walking speed ({} km/h), walk detour factor ({}) and transfer buffers ({} s to change at a station, at least {} s for a walk link) are assumptions from the rules, not measurements.",
        rules.walking_speed_kmh,
        rules.walk_detour_factor,
        rules.min_transfer_s.same_station,
        rules.min_transfer_s.walk_link
    );
    let _ = writeln!(
        s,
        "- Planned times are plans from the published timetable, not records. Nobody has ridden these routes, and real trains run late."
    );
    let _ = writeln!(
        s,
        "- Synthetic networks are generated. They exercise the pipeline and the bounds; they say nothing about any real network."
    );
    let _ = writeln!(
        s,
        "- Times are wall-clock milliseconds on a machine that may have been running other work (see the load average); treat them as rough."
    );
    let _ = writeln!(
        s,
        "- Time to first verified route: the first greedy run that visits every target, its itinerary and the verifier. Time to best: every greedy run, then itinerary and verifier for the shortest. Neither includes building the network."
    );

    if !sums.is_empty() {
        let _ = writeln!(s, "\n## Synthetic networks\n");
        let _ = writeln!(
            s,
            "Plan date {} (a Thursday), default rules. Tiny: 3 to 7 target stations, with the brute-force optimum. Small: 15 to 30 target stations, no optimum. Seeds count up from 0.\n",
            synth::PLAN_DATE
        );
        let mut head = String::from("| |");
        let mut rule = String::from("|---|");
        for x in sums {
            let _ = write!(head, " {} |", x.group);
            rule.push_str("---|");
        }
        let _ = writeln!(s, "{head}\n{rule}");
        let mut line = |label: &str, f: &dyn Fn(&Summary) -> String| {
            let mut l = format!("| {label} |");
            for x in sums {
                let _ = write!(l, " {} |", f(x));
            }
            let _ = writeln!(s, "{l}");
        };
        line("instances", &|x| x.instances.to_string());
        line("target stations, mean", &|x| {
            or_na(x.targets_mean, |v| format!("{v:.1}"))
        });
        line("connections, mean", &|x| {
            or_na(x.connections_mean, |v| format!("{v:.0}"))
        });
        line("metro lines, mean", &|x| {
            or_na(x.metro_lines_mean, |v| format!("{v:.2}"))
        });
        line("with a branch / ring / connector bus", &|x| {
            format!("{} / {} / {}", x.with_branch, x.with_ring, x.with_bus)
        });
        line("with a line reached only on foot", &|x| {
            x.with_near_miss.to_string()
        });
        line("greedy found a route", &|x| share(x.feasible, x.instances));
        line("verifier passed the first and best route", &|x| {
            share(x.verified, x.feasible)
        });
        line("greedy equals the optimum", &|x| {
            share(x.greedy_optimal, x.with_optimum)
        });
        line("gap greedy to optimum, mean / max", &|x| {
            pair(x.gap_to_optimum_mean, x.gap_to_optimum_max, pct)
        });
        line("lower bound <= optimum (asserted)", &|x| {
            share(x.bound_le_optimum, x.bound_vs_optimum)
        });
        line("gap greedy to lower bound, mean / max", &|x| {
            pair(x.gap_mean, x.gap_max, pct)
        });
        line("time to first verified route, ms, median / max", &|x| {
            pair(x.first_ms_median, x.first_ms_max, millis)
        });
        line("time to best route, ms, median / max", &|x| {
            pair(x.best_ms_median, x.best_ms_max, millis)
        });
        line("brute-force optimum, ms, median / max", &|x| {
            pair(x.optimum_ms_median, x.optimum_ms_max, millis)
        });
        line("transfers per route, mean", &|x| {
            or_na(x.transfers_mean, |v| format!("{v:.2}"))
        });
        let tight = format!(
            "tight transfers (slack under {} s), total",
            rules.tight_transfer_s
        );
        line(&tight, &|x| x.tight_transfers.to_string());
        line("smallest transfer slack, s", &|x| {
            or_na(x.min_slack_s, |v| v.to_string())
        });
        line("walking per route, m, mean", &|x| {
            or_na(x.walk_m_mean, |v| format!("{v:.0}"))
        });
    }

    let real: Vec<&Row> = rows.iter().filter(|r| r.seed.is_none()).collect();
    if !real.is_empty() || !notes.is_empty() {
        let _ = writeln!(s, "\n## Real networks\n");
        for n in notes {
            let _ = writeln!(s, "- {n}");
        }
    }
    if !real.is_empty() {
        let _ = writeln!(
            s,
            "\n| network | date | targets | connections | greedy runs with a route | best route | lower bound | gap | transfers | tight | min slack s | walk m | verified | build ms | solve ms | first route ms | best route ms | bounds ms |"
        );
        let _ = writeln!(
            s,
            "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"
        );
        for r in real {
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | {} / {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                r.name,
                r.date,
                r.targets,
                r.connections,
                r.feasible_runs,
                r.greedy_runs,
                or_na(r.best_s, hms),
                or_na(r.lower_bound_s, hms),
                or_na(r.gap, pct),
                or_na(r.transfers, |v| v.to_string()),
                or_na(r.tight_transfers, |v| v.to_string()),
                or_na(r.min_slack_s, |v| v.to_string()),
                or_na(r.walk_m, |v| format!("{v:.0}")),
                or_na(r.verified, |v| if v { "yes".into() } else { "NO".into() }),
                millis(r.build_ms),
                millis(r.solve_ms),
                or_na(r.first_ms, millis),
                or_na(r.best_ms, millis),
                millis(r.bound_ms),
            );
        }
        let unserved: Vec<&Row> = rows
            .iter()
            .filter(|r| r.seed.is_none() && r.unserved_targets > 0)
            .collect();
        for r in unserved {
            let _ = writeln!(
                s,
                "\n{} on {}: {} target stations have no service in the window, so there is no route.",
                r.name, r.date, r.unserved_targets
            );
        }
    }

    let groups = problem_groups(rows);
    if !groups.is_empty() {
        let _ = writeln!(s, "\n## Problems\n");
        let _ = writeln!(
            s,
            "Broken invariants and verifier rejections. The command exits with an error while any remain; every message is in `eval/out/bench.json`.\n"
        );
        let _ = writeln!(
            s,
            "| problem | instances | first instances |\n|---|---|---|"
        );
        for (kind, names) in groups {
            let shown: Vec<&str> = names.iter().take(8).map(String::as_str).collect();
            let more = if names.len() > shown.len() {
                ", ..."
            } else {
                ""
            };
            let _ = writeln!(
                s,
                "| {kind} | {} | {}{more} |",
                names.len(),
                shown.join(", ")
            );
        }
    }
    s
}

/// A problem message without its numbers and details, for grouping.
fn problem_kind(p: &str) -> String {
    let head = match p.split_once(": ") {
        Some((a, b)) => format!("{a}: {}", b.split_whitespace().next().unwrap_or("")),
        None => p.to_string(),
    };
    let mut out = String::new();
    for c in head.chars() {
        if c.is_ascii_digit() {
            if !out.ends_with('N') {
                out.push('N');
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Problems grouped by kind, each with the instances that have it, in the
/// order they first appear.
fn problem_groups(rows: &[Row]) -> Vec<(String, Vec<String>)> {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for r in rows {
        let label = if r.seed.is_some() {
            r.name.clone()
        } else {
            format!("{} {}", r.name, r.date)
        };
        for p in &r.problems {
            let kind = problem_kind(p);
            match groups.iter_mut().find(|(k, _)| *k == kind) {
                Some((_, names)) => {
                    if names.last() != Some(&label) {
                        names.push(label.clone());
                    }
                }
                None => groups.push((kind, vec![label.clone()])),
            }
        }
    }
    groups
}

#[derive(Serialize)]
struct Output<'a> {
    meta: &'a Meta,
    summaries: &'a [Summary],
    rows: &'a [Row],
    notes: &'a [String],
}

pub fn run(args: Args) -> Result<()> {
    let root = repo_root();
    let load_start = load_average();
    let (tiny, small) = match (args.seeds, args.quick) {
        (Some(n), _) => (n, n.div_ceil(4)),
        (None, true) => (20, 5),
        (None, false) => (200, 50),
    };
    let mut rows: Vec<Row> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    if args.only != Some(Only::Real) {
        for (size, count) in [(Size::Tiny, tiny), (Size::Small, small)] {
            let t = Instant::now();
            for seed in 0..count {
                rows.push(run_synthetic(seed, size)?);
                if (seed + 1) % 25 == 0 || seed + 1 == count {
                    eprintln!(
                        "{}: {}/{count} in {:.1} s",
                        size.name(),
                        seed + 1,
                        t.elapsed().as_secs_f64()
                    );
                }
            }
        }
    }
    if args.only != Some(Only::Synthetic) {
        let path = root.join("eval/bench.toml");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        for net in &cfg.network {
            run_real(net, &root, args.mvv_zip.as_deref(), &mut rows, &mut notes)?;
        }
    }

    let sums: Vec<Summary> = [Size::Tiny, Size::Small]
        .iter()
        .map(|size| {
            let group: Vec<&Row> = rows.iter().filter(|r| r.group == size.name()).collect();
            summarise(size.name(), &group)
        })
        .filter(|x| x.instances > 0)
        .collect();
    let meta = Meta {
        command: command_line(&root),
        date: chrono::Utc::now().format("%Y-%m-%d").to_string(),
        commit: commit(&root),
        machine: machine(),
        build: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        threads: 1,
        load_average_start: load_start,
        load_average_end: load_average(),
    };
    let md = markdown(&meta, &sums, &rows, &notes);
    println!("{md}");

    let out_dir = root.join("eval/out");
    std::fs::create_dir_all(&out_dir)?;
    let json = serde_json::to_string_pretty(&Output {
        meta: &meta,
        summaries: &sums,
        rows: &rows,
        notes: &notes,
    })?;
    std::fs::write(out_dir.join("bench.json"), json)?;
    let md_path = if args.quick || args.only.is_some() {
        out_dir.join("RESULTS.md")
    } else {
        root.join("eval/RESULTS.md")
    };
    std::fs::write(&md_path, &md)?;
    eprintln!(
        "wrote {} and {}",
        shown(&md_path, &root),
        shown(&out_dir.join("bench.json"), &root)
    );

    let problems: Vec<String> = rows
        .iter()
        .flat_map(|r| {
            r.problems
                .iter()
                .map(move |p| format!("{} {}: {p}", r.name, r.date))
        })
        .collect();
    if !problems.is_empty() {
        for p in &problems {
            eprintln!("FAILED {p}");
        }
        bail!(
            "{} problems (broken invariants or verifier rejections); see the Problems section of {}",
            problems.len(),
            shown(&md_path, &root)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handful of tiny seeds through the whole pipeline: every route
    /// passes the verifier and lower bound <= optimum <= greedy.
    #[test]
    fn tiny_seeds_verify_and_bracket_the_optimum() {
        let mut rows = Vec::new();
        for seed in 0..6 {
            let row = run_synthetic(seed, Size::Tiny).unwrap();
            assert!(row.problems.is_empty(), "seed {seed}: {:?}", row.problems);
            assert_eq!(row.verified, Some(true), "seed {seed}");
            let (Some(lb), Some(opt), Some(best)) = (row.lower_bound_s, row.optimum_s, row.best_s)
            else {
                panic!("seed {seed}: missing bound, optimum or route: {row:?}");
            };
            assert!(
                lb <= opt && opt <= best,
                "seed {seed}: {lb} <= {opt} <= {best}"
            );
            rows.push(row);
        }
        let refs: Vec<&Row> = rows.iter().collect();
        let sum = summarise("tiny", &refs);
        assert_eq!(sum.verified, rows.len());
        assert_eq!(sum.bound_le_optimum, sum.bound_vs_optimum);
    }

    #[test]
    fn invariant_breaks_are_reported() {
        let mut row = Row {
            best_s: Some(100),
            optimum_s: Some(120),
            lower_bound_s: Some(130),
            ..Row::default()
        };
        check_invariants(&mut row, true);
        assert_eq!(row.problems.len(), 3, "{:?}", row.problems);
        assert_eq!(row.greedy_is_optimal, Some(false));
    }

    #[test]
    fn problems_group_by_kind() {
        assert_eq!(
            problem_kind(
                "verifier rejected the best route: STATIONS_MISMATCH leg lists 3 stations, the trip visits 4"
            ),
            "verifier rejected the best route: STATIONS_MISMATCH"
        );
        assert_eq!(
            problem_kind("lower bound 130 s exceeds the optimum 120 s"),
            "lower bound N s exceeds the optimum N s"
        );
    }

    #[test]
    fn local_paths_stay_out_of_committed_output() {
        let root = Path::new("/home/someone/allstops");
        assert_eq!(
            shown(Path::new("/home/someone/allstops/data/cache/x.zip"), root),
            "data/cache/x.zip"
        );
        assert_eq!(
            shown(Path::new("/elsewhere/feeds/x.zip"), root),
            ".../x.zip"
        );
    }
}
