use std::time::Instant;

use allstops_core::csa::{Csa, JLeg};
use allstops_core::network::{Network, StationIdx, Time};
use allstops_core::plan::{Plan, greedy, visits};
use allstops_core::rules::parse_clock;
use anyhow::{Result, bail};
use rayon::prelude::*;

use crate::plan_input::{PlanArgs, load};
use crate::{Outcome, style};

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    plan: PlanArgs,
    /// Minutes between tried start times, from earliest_start.
    #[arg(long, default_value_t = 10)]
    start_step_min: i32,
    /// Number of start times to try per start station.
    #[arg(long, default_value_t = 12)]
    start_count: i32,
}

pub struct Best {
    pub plan: Plan,
    pub first: Time,
    pub last: Time,
    pub start: StationIdx,
    pub t0: Time,
}

/// Run the greedy from every candidate start station and start time, in
/// parallel, and keep the shortest. Ties go to the earlier start time and
/// then the lower station index, so the result does not depend on thread
/// scheduling.
pub fn best_greedy(net: &Network, starts: &[StationIdx], times: &[Time]) -> (Option<Best>, usize) {
    let jobs: Vec<(StationIdx, Time)> = starts
        .iter()
        .flat_map(|&s| times.iter().map(move |&t| (s, t)))
        .collect();
    let results: Vec<Option<Best>> = jobs
        .par_iter()
        .map_init(
            || Csa::new(net),
            |csa, &(s, t0)| {
                let plan = greedy(csa, s, t0)?;
                let (first, last) = plan.duration(net)?;
                Some(Best {
                    plan,
                    first,
                    last,
                    start: s,
                    t0,
                })
            },
        )
        .collect();
    let feasible = results.iter().filter(|r| r.is_some()).count();
    let best = results
        .into_iter()
        .flatten()
        .min_by_key(|b| (b.last - b.first, b.t0, b.start));
    (best, feasible)
}

pub fn hm(t: Time) -> String {
    let t = t.max(0);
    format!("{:02}:{:02}", t / 3600, (t / 60) % 60)
}

pub fn dur(s: Time) -> String {
    format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

pub fn run(args: Args, json: bool) -> Result<Outcome> {
    let input = load(&args.plan)?;
    let net = &input.network;
    if !input.report.unserved_targets.is_empty() {
        eprintln!(
            "{} {} target station(s) have no service in the window on {}: {}",
            style::bad("infeasible:"),
            input.report.unserved_targets.len(),
            input.report.date,
            input.report.unserved_targets.join(", ")
        );
        return Ok(Outcome::Rejected);
    }
    let ws = parse_clock(&input.rules.earliest_start).unwrap_or(0);
    let starts: Vec<StationIdx> = if input.rules.start == "any" {
        net.targets.clone()
    } else {
        match net.stations.iter().position(|s| s.id == input.rules.start) {
            Some(i) => vec![i as StationIdx],
            None => bail!(
                "start station {:?} is not in the network",
                input.rules.start
            ),
        }
    };
    let times: Vec<Time> = (0..args.start_count)
        .map(|k| ws + k * args.start_step_min * 60)
        .collect();

    let t0 = Instant::now();
    let (best, feasible) = best_greedy(net, &starts, &times);
    let solve_ms = t0.elapsed().as_secs_f64() * 1e3;
    let runs = starts.len() * times.len();

    let Some(best) = best else {
        eprintln!(
            "{} no greedy run covered every target ({runs} runs)",
            style::bad("infeasible:")
        );
        return Ok(Outcome::Rejected);
    };
    let total = best.last - best.first;
    let v = visits(net, &best.plan.legs);
    let rides = best
        .plan
        .legs
        .iter()
        .filter(|l| matches!(l, JLeg::Ride { .. }))
        .count();
    let walks = best
        .plan
        .legs
        .iter()
        .filter(|l| matches!(l, JLeg::Walk { .. }))
        .count();
    let first_station = &net.stations[v.first().map(|x| x.0).unwrap_or(best.start) as usize].name;
    let is_target = net.target_mask();
    let last_station = v
        .iter()
        .filter(|(s, _)| is_target[*s as usize])
        .max_by_key(|(_, t)| *t)
        .map(|(s, _)| net.stations[*s as usize].name.clone())
        .unwrap_or_default();

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "date": input.report.date,
                "duration_s": total,
                "first_visit": hm(best.first),
                "last_visit": hm(best.last),
                "start": first_station,
                "end": last_station,
                "rides": rides,
                "walks": walks,
                "greedy_runs": runs,
                "feasible_runs": feasible,
                "solve_ms": solve_ms.round(),
                "build_ms": input.build_ms.round(),
                "load_ms": input.load_ms.round(),
            }))?
        );
    } else {
        println!(
            "{}",
            style::bold(&format!("Greedy route, {}", input.report.date))
        );
        println!("  total time       {}", dur(total));
        println!("  first visit      {} at {}", first_station, hm(best.first));
        println!("  last visit       {} at {}", last_station, hm(best.last));
        println!("  legs             {rides} rides, {walks} walks");
        println!("  greedy runs      {runs} ({feasible} covered every target) in {solve_ms:.0} ms");
        println!(
            "{}",
            style::dim("Times are service-day clock times (may exceed 24:00); not yet verified.")
        );
    }
    Ok(Outcome::Ok)
}
