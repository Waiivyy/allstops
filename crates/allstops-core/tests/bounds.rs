//! Section 10 tests 6 and 7 on tiny synthetic instances:
//! static bound <= profile bound guarantee is not claimed, but both bounds
//! must be <= the brute-force optimum, which must be <= every greedy result;
//! every greedy result visits every target according to its own legs.

use allstops_core::bound::{lower_bound, profile_lower_bound};
use allstops_core::builder::random;
use allstops_core::csa::Csa;
use allstops_core::oracle::optimum;
use allstops_core::plan::greedy;

fn cases() -> u64 {
    std::env::var("ALLSTOPS_SYNTH_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(400)
}

#[test]
fn bounds_and_greedy_bracket_the_optimum() {
    let mut feasible = 0;
    let mut greedy_found = 0;
    let mut greedy_optimal = 0;
    for seed in 0..cases() {
        let net = random::network(seed, 7);
        net.validate().unwrap();
        let Some(opt) = optimum(&net) else { continue };
        feasible += 1;
        if let Some(b) = lower_bound(&net, opt) {
            assert!(
                b.seconds <= opt,
                "seed {seed}: static bound {} > optimum {opt}",
                b.seconds
            );
        }
        if let Some(b) = profile_lower_bound(&net, opt) {
            assert!(
                b.seconds <= opt,
                "seed {seed}: profile bound {} > optimum {opt}",
                b.seconds
            );
        }
        let mut csa = Csa::new(&net);
        let mut best: Option<i32> = None;
        for &s in &net.targets {
            for k in 0..8 {
                if let Some(p) = greedy(&mut csa, s, net.window_start + k * 300) {
                    let (first, last) = p.duration(&net).expect("greedy plans visit every target");
                    let d = last - first;
                    assert!(d >= opt, "seed {seed}: greedy {d} beats the optimum {opt}");
                    best = Some(best.map_or(d, |b| b.min(d)));
                }
            }
        }
        if let Some(b) = best {
            greedy_found += 1;
            if b == opt {
                greedy_optimal += 1;
            }
        }
    }
    eprintln!(
        "{feasible} feasible instances; greedy found a route on {greedy_found}, optimal on {greedy_optimal}"
    );
    assert!(
        feasible > cases() / 4,
        "generator produces too few feasible instances"
    );
}

/// The same brackets on networks where some hops and dwells take no time,
/// so several connections happen at the same instant (positive change and
/// walk times keep their order unambiguous).
#[test]
fn bounds_hold_with_instant_hops() {
    let mut feasible = 0;
    for seed in 0..cases() {
        let net = random::network_with(seed + 1_000_000, 7, true);
        net.validate().unwrap();
        let Some(opt) = optimum(&net) else { continue };
        feasible += 1;
        if let Some(b) = lower_bound(&net, opt) {
            assert!(
                b.seconds <= opt,
                "seed {seed}: static bound {} > optimum {opt}",
                b.seconds
            );
        }
        if let Some(b) = profile_lower_bound(&net, opt) {
            assert!(
                b.seconds <= opt,
                "seed {seed}: profile bound {} > optimum {opt}",
                b.seconds
            );
        }
        let mut csa = Csa::new(&net);
        for &s in &net.targets {
            if let Some(p) = greedy(&mut csa, s, net.window_start) {
                let (first, last) = p.duration(&net).expect("greedy plans visit every target");
                assert!(last - first >= opt, "seed {seed}: greedy beats the optimum");
            }
        }
    }
    assert!(
        feasible > cases() / 4,
        "generator produces too few feasible instances"
    );
}
