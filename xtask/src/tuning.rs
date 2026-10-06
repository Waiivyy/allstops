//! Held-Karp step-rule comparison on a real network.

use std::path::PathBuf;
use std::time::Instant;

use allstops_core::bound::{HkConfig, held_karp_path_bound_with, target_distances};
use allstops_core::network::INF;
use allstops_core::profile::min_visit_gaps;
use anyhow::Result;

#[derive(clap::Args)]
pub struct Args {
    #[arg(long, default_value = "data/cache/mvv.gtfs.zip")]
    zip: PathBuf,
    #[arg(long, default_value = "data/rules/mvv-ubahn.toml")]
    rules: PathBuf,
    #[arg(long)]
    date: Option<String>,
    /// A known route length in seconds, used as the upper target.
    #[arg(long, default_value_t = 16000)]
    upper: i64,
}

pub fn run(a: Args) -> Result<()> {
    let real = crate::real::load(&a.zip, &a.rules, a.date.as_deref())?;
    let net = &real.network;
    let t = Instant::now();
    let stat = target_distances(net);
    let stat_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let mut prof = min_visit_gaps(net);
    let prof_ms = t.elapsed().as_secs_f64() * 1e3;
    let big = 10 * i64::from(net.window_end - net.window_start);
    for x in prof.iter_mut().flatten() {
        if *x >= i64::from(INF) {
            *x = big;
        }
    }
    println!(
        "network {} ({} targets); static matrix {stat_ms:.0} ms, profile matrix {prof_ms:.0} ms",
        real.report.date,
        net.targets.len()
    );
    println!("| matrix | max_iter | lambda | patience | target | bound | iterations | ms |");
    println!("|---|---|---|---|---|---|---|---|");
    let configs = [
        (2000, 2.0, 20, None),
        (20000, 2.0, 20, None),
        (20000, 2.0, 200, None),
        (20000, 1.0, 100, Some(1.05)),
        (20000, 2.0, 100, Some(1.10)),
        (50000, 2.0, 300, Some(1.05)),
    ];
    for (name, m) in [("static", &stat), ("profile", &prof)] {
        for &(max_iter, lambda, patience, target_ratio) in &configs {
            let cfg = HkConfig {
                max_iter,
                lambda,
                patience,
                target_ratio,
            };
            let t = Instant::now();
            let b = held_karp_path_bound_with(m, a.upper, &cfg);
            let ms = t.elapsed().as_secs_f64() * 1e3;
            let (s, it) = b.map(|b| (b.seconds, b.iterations)).unwrap_or((0, 0));
            println!(
                "| {name} | {max_iter} | {lambda} | {patience} | {} | {}:{:02}:{:02} | {it} | {ms:.0} |",
                target_ratio
                    .map(|r| format!("{r}x best"))
                    .unwrap_or_else(|| "upper".into()),
                s / 3600,
                (s / 60) % 60,
                s % 60
            );
        }
    }
    Ok(())
}
