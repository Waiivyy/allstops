//! Routing engine comparison on a full real network: Connection Scan
//! against RAPTOR for one-to-all earliest-board queries, the
//! early-terminating scan the greedy runs, point-to-point queries with the
//! journey, and profile queries (one backward scan per destination, then a
//! lookup per question). Single-threaded.

use std::hint::black_box;
use std::path::PathBuf;
use std::time::Instant;

use allstops_core::builder::random::Lcg;
use allstops_core::csa::{Csa, Origin};
use allstops_core::network::{INF, StationIdx, Time};
use allstops_core::profile::ProfileTo;
use allstops_core::raptor::Raptor;
use allstops_core::rules::parse_clock;
use allstops_gtfs::calendar::ServiceCalendar;
use allstops_gtfs::network::build_network;
use allstops_gtfs::select::select;
use anyhow::{Context, Result, bail};
use serde::Serialize;

#[derive(clap::Args)]
pub struct Args {
    #[arg(long, default_value = "data/cache/mvv.gtfs.zip")]
    zip: PathBuf,
    #[arg(long, default_value = "data/rules/mvv-ubahn.toml")]
    rules: PathBuf,
    #[arg(long, default_value = "2026-11-12")]
    date: String,
    /// Replace the rules file's connector modes, so every mode is routable.
    #[arg(long, value_delimiter = ',', default_value = "tram,bus,rail")]
    connector_modes: Vec<String>,
    #[arg(long, default_value_t = 300)]
    queries: usize,
    /// Query departures are drawn from earliest_start to this many hours
    /// after it.
    #[arg(long, default_value_t = 14)]
    span_h: i32,
    #[arg(long, default_value_t = 1)]
    seed: u64,
    /// Destinations (target stations) to build profiles for.
    #[arg(long, default_value_t = 20)]
    profile_dests: usize,
    #[arg(long, default_value = "eval/out/routing.json")]
    out: PathBuf,
}

#[derive(Serialize)]
struct NetworkSize {
    stations: usize,
    connections: usize,
    trips: usize,
    footpaths: usize,
    targets: usize,
    /// FIFO route patterns used by RAPTOR.
    patterns: usize,
    /// Stop-sequence groups before splitting overtaking trips.
    pattern_groups: usize,
}

#[derive(Serialize)]
struct Latency {
    mean_ms: f64,
    p50_ms: f64,
    p95_ms: f64,
    max_ms: f64,
}

#[derive(Serialize)]
struct Engine {
    name: &'static str,
    latency: Latency,
    /// Connections scanned (CSA) or rounds run (RAPTOR), mean per query.
    work_mean: f64,
    work_max: usize,
    work_unit: &'static str,
}

#[derive(Serialize)]
struct Agreement {
    queries: usize,
    /// Queries where every station's earliest-board label is identical.
    identical: usize,
    /// Station labels that differ, summed over all queries.
    differing_labels: usize,
}

#[derive(Serialize)]
struct Output {
    date: String,
    connector_modes: Vec<String>,
    seed: u64,
    queries: usize,
    span_h: i32,
    threads: usize,
    os: &'static str,
    arch: &'static str,
    network: NetworkSize,
    feed_load_ms: f64,
    network_build_ms: f64,
    raptor_preprocess_ms: f64,
    /// Stations with a finite label, mean over queries.
    reached_stations_mean: f64,
    agreement: Agreement,
    engines: Vec<Engine>,
    profiles: Profiles,
}

#[derive(Serialize)]
struct Profiles {
    destinations: usize,
    /// One backward scan per destination.
    scan: Latency,
    /// Lookups per destination: every query origin and time.
    lookups: usize,
    lookup_us_mean: f64,
    /// Lookups whose answer equals a forward scan from the same place and time.
    identical_to_forward: usize,
    /// (ready time, visit) pairs per station, mean over stations and
    /// destinations.
    pairs_per_station_mean: f64,
}

/// Nearest-rank percentile of sorted samples.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    let rank = (p * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn latency(ms: &[f64]) -> Latency {
    let mut s = ms.to_vec();
    s.sort_by(f64::total_cmp);
    Latency {
        mean_ms: s.iter().sum::<f64>() / s.len() as f64,
        p50_ms: percentile(&s, 0.50),
        p95_ms: percentile(&s, 0.95),
        max_ms: s[s.len() - 1],
    }
}

/// Time `f` on every query; `f` returns the work it did.
fn time_queries(
    name: &'static str,
    work_unit: &'static str,
    queries: &[(StationIdx, Time)],
    mut f: impl FnMut(StationIdx, Time) -> usize,
) -> Engine {
    let mut ms = Vec::with_capacity(queries.len());
    let mut work = Vec::with_capacity(queries.len());
    for &(s, t) in queries {
        let t0 = Instant::now();
        let w = black_box(f(black_box(s), black_box(t)));
        ms.push(t0.elapsed().as_secs_f64() * 1e3);
        work.push(w);
    }
    Engine {
        name,
        latency: latency(&ms),
        work_mean: work.iter().sum::<usize>() as f64 / work.len() as f64,
        work_max: work.iter().copied().max().unwrap_or(0),
        work_unit,
    }
}

pub fn run(a: Args) -> Result<()> {
    if a.queries == 0 || a.span_h < 0 {
        bail!("--queries must be at least 1 and --span-h not negative");
    }
    let (feed, _bytes, feed_load_ms) = crate::real::load_feed(&a.zip)?;
    let crate::real::RuleFiles {
        mut rules,
        selection,
        stations: station_overrides,
        walks,
    } = crate::real::load_rules(&a.rules)?;
    rules.date = a.date.clone();
    rules.connector_modes = a.connector_modes.clone();
    let t = Instant::now();
    let clustering = crate::real::stations(&feed, &station_overrides)?;
    walks.check_stations(&clustering)?;
    let targets = select(&feed, &clustering, &selection)?;
    let cal = ServiceCalendar::new(&feed);
    let (net, _report) = build_network(
        &feed,
        &cal,
        &clustering,
        &targets,
        &crate::real::visit_types(&feed, &selection)?,
        &rules,
        &walks,
    )?;
    let network_build_ms = t.elapsed().as_secs_f64() * 1e3;

    let t = Instant::now();
    let mut raptor = Raptor::new(&net);
    let raptor_preprocess_ms = t.elapsed().as_secs_f64() * 1e3;
    let mut csa = Csa::new(&net);

    let ws = parse_clock(&rules.earliest_start).context("bad earliest_start")?;
    let mut rng = Lcg::new(a.seed);
    let queries: Vec<(StationIdx, Time)> = (0..a.queries)
        .map(|_| {
            let s = rng.below(net.stations.len() as u64) as StationIdx;
            let t = ws + rng.below(a.span_h as u64 * 3600 + 1) as Time;
            (s, t)
        })
        .collect();

    // Agreement, which also warms both engines up.
    let mut identical = 0;
    let mut differing_labels = 0;
    let mut reached = 0usize;
    for &(station, time) in &queries {
        let want = csa.run(Origin::At { station, time }, None).board.clone();
        let got = raptor.earliest_board(station, time);
        reached += want.iter().filter(|&&b| b < INF).count();
        let diff = got.iter().zip(&want).filter(|(g, w)| g != w).count();
        if diff == 0 {
            identical += 1;
        }
        differing_labels += diff;
    }

    let stop_at = net.target_mask();
    // A target station per query, for point-to-point queries.
    let dests: Vec<StationIdx> = (0..queries.len())
        .map(|_| net.targets[rng.below(net.targets.len() as u64) as usize])
        .collect();
    let mut dest_iter = dests.iter().cycle();
    let mut only = vec![false; net.stations.len()];
    let engines = vec![
        time_queries(
            "CSA, one target, with the journey",
            "connections scanned",
            &queries,
            |station, time| {
                let d = *dest_iter.next().expect("cycle");
                only[d as usize] = true;
                let scanned = csa.run(Origin::At { station, time }, Some(&only)).scanned;
                black_box(csa.journey_to_visit(d));
                only[d as usize] = false;
                scanned
            },
        ),
        time_queries(
            "CSA, full scan",
            "connections scanned",
            &queries,
            |station, time| csa.run(Origin::At { station, time }, None).scanned,
        ),
        time_queries("RAPTOR", "rounds", &queries, |station, time| {
            raptor.earliest_board(station, time);
            raptor.rounds()
        }),
        time_queries(
            "CSA, stop at first target visit",
            "connections scanned",
            &queries,
            |station, time| {
                csa.run(Origin::At { station, time }, Some(&stop_at))
                    .scanned
            },
        ),
    ];

    // Profiles: build one per destination, then answer every query from it.
    let pdests: Vec<StationIdx> = (0..a.profile_dests.min(net.targets.len()))
        .map(|_| net.targets[rng.below(net.targets.len() as u64) as usize])
        .collect();
    let mut scan_ms = Vec::new();
    let mut lookup_s = 0.0f64;
    let mut lookups = 0usize;
    let mut identical_to_forward = 0usize;
    let mut pairs = 0usize;
    for &d in &pdests {
        let t0 = Instant::now();
        let p = black_box(ProfileTo::new(&net, d));
        scan_ms.push(t0.elapsed().as_secs_f64() * 1e3);
        let t0 = Instant::now();
        let answers: Vec<Time> = queries
            .iter()
            .map(|&(s, t)| black_box(p.earliest_visit(&net, s, t)))
            .collect();
        lookup_s += t0.elapsed().as_secs_f64();
        lookups += answers.len();
        for (&(station, time), &got) in queries.iter().zip(&answers) {
            if csa.run(Origin::At { station, time }, None).visit[d as usize] == got {
                identical_to_forward += 1;
            }
        }
        pairs += (0..net.stations.len() as u32)
            .map(|s| p.pairs(&net, s).len())
            .sum::<usize>();
    }
    let profiles = Profiles {
        destinations: pdests.len(),
        scan: latency(&scan_ms),
        lookups,
        lookup_us_mean: lookup_s * 1e6 / lookups.max(1) as f64,
        identical_to_forward,
        pairs_per_station_mean: pairs as f64 / (pdests.len().max(1) * net.stations.len()) as f64,
    };

    let out = Output {
        date: rules.date.clone(),
        connector_modes: rules.connector_modes.clone(),
        seed: a.seed,
        queries: queries.len(),
        span_h: a.span_h,
        threads: 1,
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        network: NetworkSize {
            stations: net.stations.len(),
            connections: net.connections.len(),
            trips: net.trips.len(),
            footpaths: net.footpaths.len(),
            targets: net.targets.len(),
            patterns: raptor.patterns().len(),
            pattern_groups: raptor.patterns().groups(),
        },
        feed_load_ms,
        network_build_ms,
        raptor_preprocess_ms,
        reached_stations_mean: reached as f64 / queries.len() as f64,
        agreement: Agreement {
            queries: queries.len(),
            identical,
            differing_labels,
        },
        engines,
        profiles,
    };

    let n = &out.network;
    println!(
        "network {} with connector modes {:?}: {} stations, {} connections, {} trips, {} walk links, {} targets",
        out.date, out.connector_modes, n.stations, n.connections, n.trips, n.footpaths, n.targets
    );
    println!(
        "RAPTOR patterns: {} ({} stop-sequence groups before the FIFO split), built in {:.1} ms",
        n.patterns, n.pattern_groups, out.raptor_preprocess_ms
    );
    println!(
        "{} queries (seed {}), departures in [{}, +{} h]; mean {:.0} stations reached",
        out.queries, out.seed, rules.earliest_start, out.span_h, out.reached_stations_mean
    );
    println!(
        "agreement: {}/{} queries with identical earliest-board labels at every station ({} differing labels)",
        out.agreement.identical, out.agreement.queries, out.agreement.differing_labels
    );
    println!();
    println!("| engine | mean ms | p50 ms | p95 ms | max ms | work per query (mean / max) |");
    println!("|---|---|---|---|---|---|");
    for e in &out.engines {
        let l = &e.latency;
        println!(
            "| {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.1} / {} {} |",
            e.name, l.mean_ms, l.p50_ms, l.p95_ms, l.max_ms, e.work_mean, e.work_max, e.work_unit
        );
    }

    let p = &out.profiles;
    println!();
    println!(
        "profiles: {} destinations, backward scan mean {:.1} ms (p50 {:.1}, max {:.1}); {} lookups at {:.2} µs each; {}/{} equal to a forward scan; {:.1} (ready, visit) pairs per station",
        p.destinations,
        p.scan.mean_ms,
        p.scan.p50_ms,
        p.scan.max_ms,
        p.lookups,
        p.lookup_us_mean,
        p.identical_to_forward,
        p.lookups,
        p.pairs_per_station_mean
    );

    if let Some(dir) = a.out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&a.out, serde_json::to_string_pretty(&out)?)?;
    eprintln!("wrote {}", a.out.display());
    Ok(())
}
