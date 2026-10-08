use std::time::Instant;

use allstops_core::csa::{Csa, JLeg};
use allstops_core::network::{Network, StationIdx, Time};
use allstops_core::plan::{
    Best, Change, greedy_from, greedy_jobs, shortest, transfer_slacks, visits,
};
use allstops_core::rules::parse_clock;
use anyhow::{Result, bail};
use rayon::prelude::*;

use allstops_core::bound::{lower_bound, profile_lower_bound};
use allstops_core::itinerary::to_itinerary;

use crate::check::{check_json, print_report, rules_for_verifier};
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
    /// Write the verified itinerary JSON here.
    #[arg(long)]
    out: Option<std::path::PathBuf>,
}

/// [`allstops_core::plan::best_greedy`] in parallel: the same runs and the
/// same tie rule, so the result does not depend on thread scheduling.
pub fn best_greedy(net: &Network, starts: &[StationIdx], times: &[Time]) -> (Option<Best>, usize) {
    let results: Vec<Option<Best>> = greedy_jobs(starts, times)
        .par_iter()
        .map_init(|| Csa::new(net), |csa, &(s, t0)| greedy_from(csa, s, t0))
        .collect();
    shortest(results)
}

pub const SAFETY_NOTE: &str = "Planned from the published timetable. Real trains run late. Check official sources and ride safely.";

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

    // Two valid lower bounds; the larger one is reported.
    let tb = Instant::now();
    let static_bound = lower_bound(net, total);
    let static_ms = tb.elapsed().as_secs_f64() * 1e3;
    let tp = Instant::now();
    let profile_bound = profile_lower_bound(net, total);
    let profile_ms = tp.elapsed().as_secs_f64() * 1e3;
    let bound_ms = static_ms + profile_ms;
    let lb = match (&static_bound, &profile_bound) {
        (Some(a), Some(b)) => Some(a.seconds.max(b.seconds)),
        (Some(a), None) => Some(a.seconds),
        (None, Some(b)) => Some(b.seconds),
        (None, None) => None,
    };
    let gap = lb
        .filter(|&l| l > 0)
        .map(|l| f64::from(total - l) / f64::from(l));
    if let Some(l) = lb
        && l > total
    {
        bail!("internal error: lower bound {l}s exceeds a found route of {total}s");
    }

    // Nothing is shown unless the independent verifier accepts it.
    let Some(mut itinerary) = to_itinerary(
        net,
        &best.plan,
        &input.rules,
        input.basis.feed_ref.clone(),
        &input.basis.timezone,
    ) else {
        bail!("internal error: the best plan does not visit every target");
    };
    itinerary.lower_bound_s = lb;
    itinerary.gap = gap.map(|g| (g * 1e4).round() / 1e4);
    let doc = serde_json::to_string_pretty(&itinerary)?;
    let report = check_json(&input.basis, Some(&rules_for_verifier(&input.rules)?), &doc)?;
    if !report.passed {
        eprintln!(
            "{} the verifier rejected the planned route ({} violations). This is a bug; nothing is shown.",
            style::bad("error:"),
            report.violations.len()
        );
        print_report(&report);
        return Ok(Outcome::Rejected);
    }
    if report.duration_s != Some(i64::from(total)) {
        bail!(
            "internal error: verifier duration {:?} differs from planned {total}",
            report.duration_s
        );
    }
    // Changes between rides and how much time each leaves to spare.
    let changes = transfer_slacks(net, &best.plan);
    let tight: Vec<&Change> = changes
        .iter()
        .filter(|c| c.slack < input.rules.tight_transfer_s)
        .collect();
    let least_spare = changes.iter().map(|c| c.slack).min();
    if let Some(out) = &args.out {
        std::fs::write(out, &doc)?;
        eprintln!("{} {}", style::dim("wrote"), out.display());
    }
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
                "lower_bound_s": lb,
                "gap": gap,
                "bound_ms": bound_ms.round(),
                "static_bound_s": static_bound.as_ref().map(|b| b.seconds),
                "static_bound_ms": static_ms.round(),
                "profile_bound_s": profile_bound.as_ref().map(|b| b.seconds),
                "profile_bound_ms": profile_ms.round(),
                "verified": true,
                "changes": changes.len(),
                "tight_transfer_s": input.rules.tight_transfer_s,
                "tight_changes": tight.iter().map(|c| serde_json::json!({
                    "station": net.stations[c.station as usize].name,
                    "leg": c.leg,
                    "after_walk": c.walked,
                    "spare_s": c.slack,
                })).collect::<Vec<_>>(),
                "least_spare_s": least_spare,
                "attribution": itinerary.feed.attribution,
                "note": SAFETY_NOTE,
                "build_ms": input.build_ms.round(),
                "load_ms": input.basis.load_ms.round(),
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
        println!(
            "  changes          {} ({} tight: under {} s to spare beyond the minimum change{})",
            changes.len(),
            tight.len(),
            input.rules.tight_transfer_s,
            least_spare
                .map(|s| format!("; least {s} s"))
                .unwrap_or_default()
        );
        for c in &tight {
            println!(
                "    {}",
                style::warn(&format!(
                    "tight: {} s spare at {}{}",
                    c.slack,
                    net.stations[c.station as usize].name,
                    if c.walked { " after a walk" } else { "" }
                ))
            );
        }
        match (lb, gap) {
            (Some(l), Some(g)) => println!("  lower bound      {} (gap {:.1}%)", dur(l), g * 100.0),
            _ => println!("  lower bound      none (targets not connected)"),
        }
        let show = |b: &Option<allstops_core::bound::Bound>| {
            b.as_ref()
                .map(|b| dur(b.seconds))
                .unwrap_or_else(|| "none".into())
        };
        println!(
            "    static         {} ({static_ms:.0} ms)",
            show(&static_bound)
        );
        println!(
            "    profile        {} ({profile_ms:.0} ms)",
            show(&profile_bound)
        );
        println!("  greedy runs      {runs} ({feasible} covered every target) in {solve_ms:.0} ms");
        let against = if input.basis.pack.is_some() {
            "verified against the pack's timetable"
        } else {
            "verified against the raw timetable"
        };
        println!("  {}", style::good(against));
        println!(
            "{}",
            style::dim("Times are service-day clock times (may exceed 24:00).")
        );
        println!("{}", style::dim(&itinerary.feed.attribution));
        println!("{}", style::dim(SAFETY_NOTE));
    }
    Ok(Outcome::Ok)
}
