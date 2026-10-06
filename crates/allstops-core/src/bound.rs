//! A first static lower bound.
//!
//! Relaxation: take the static graph whose edges are the fastest scheduled
//! hop between two stations (over every trip the rules allow) and every walk
//! link. Waiting, dwell and change times count as zero. Let `d(i, j)` be the
//! shortest static travel time between targets `i` and `j`.
//!
//! Claim: every feasible itinerary takes at least the length of the
//! shortest Hamiltonian path over the targets under `d`.
//! Sketch: order the targets by first visit time. Between two consecutive
//! first visits the runner moves from one target to the next along a walk
//! in the static graph, so the time between them is at least their `d`.
//! Summing gives a Hamiltonian path that is no longer than the itinerary.
//!
//! The path problem is NP-hard; this module bounds it from below with the
//! Held-Karp 1-tree relaxation (Held and Karp, 1970/1971), using a dummy
//! node joined at cost 0 to every target so both path ends are free, and
//! subgradient optimisation of the node penalties. Costs are symmetrised
//! with `min(d(i, j), d(j, i))`, which only lowers them. Every iterate is a
//! valid lower bound; the best one is returned.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use crate::network::{INF, Network, StationIdx, Time};

/// Static adjacency: fastest hop or walk between two stations.
pub fn static_graph(net: &Network) -> Vec<Vec<(StationIdx, Time)>> {
    let mut best: HashMap<(StationIdx, StationIdx), Time> = HashMap::new();
    for c in &net.connections {
        if c.dep_station == c.arr_station {
            continue;
        }
        let w = c.arr - c.dep;
        let e = best.entry((c.dep_station, c.arr_station)).or_insert(w);
        if w < *e {
            *e = w;
        }
    }
    for (s, w) in (0..net.stations.len()).flat_map(|s| {
        net.footpaths_from(s as StationIdx)
            .iter()
            .map(move |f| ((s as StationIdx, f.to), f.duration))
    }) {
        let e = best.entry(s).or_insert(w);
        if w < *e {
            *e = w;
        }
    }
    let mut adj = vec![Vec::new(); net.stations.len()];
    let mut edges: Vec<_> = best.into_iter().collect();
    edges.sort_unstable();
    for ((a, b), w) in edges {
        adj[a as usize].push((b, w));
    }
    adj
}

fn dijkstra(adj: &[Vec<(StationIdx, Time)>], src: StationIdx) -> Vec<i64> {
    let mut dist = vec![i64::MAX; adj.len()];
    let mut heap = BinaryHeap::new();
    dist[src as usize] = 0;
    heap.push(Reverse((0i64, src)));
    while let Some(Reverse((d, u))) = heap.pop() {
        if d > dist[u as usize] {
            continue;
        }
        for &(v, w) in &adj[u as usize] {
            let nd = d + i64::from(w);
            if nd < dist[v as usize] {
                dist[v as usize] = nd;
                heap.push(Reverse((nd, v)));
            }
        }
    }
    dist
}

/// Shortest static travel times between every ordered pair of targets.
pub fn target_distances(net: &Network) -> Vec<Vec<i64>> {
    let adj = static_graph(net);
    net.targets
        .iter()
        .map(|&t| {
            let d = dijkstra(&adj, t);
            net.targets.iter().map(|&u| d[u as usize]).collect()
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bound {
    /// Seconds; no feasible itinerary is shorter.
    pub seconds: Time,
    pub iterations: usize,
}

/// Held-Karp 1-tree bound on the shortest Hamiltonian path with free ends.
/// `upper` is any known path length (a found itinerary's duration works)
/// and only steers the step size. Returns `None` when some pair of targets
/// is not connected at all.
pub fn held_karp_path_bound(d: &[Vec<i64>], upper: i64, max_iter: usize) -> Option<Bound> {
    held_karp_path_bound_with(
        d,
        upper,
        &HkConfig {
            max_iter,
            ..HkConfig::default()
        },
    )
}

/// Subgradient settings for [`held_karp_path_bound_with`].
#[derive(Debug, Clone)]
pub struct HkConfig {
    pub max_iter: usize,
    /// Initial step multiplier.
    pub lambda: f64,
    /// Halve the multiplier after this many iterations without improvement.
    pub patience: usize,
    /// Step towards `target_ratio * best bound so far` instead of `upper`
    /// when set (an adaptive target, useful when `upper` is far off).
    pub target_ratio: Option<f64>,
}

impl Default for HkConfig {
    fn default() -> Self {
        HkConfig {
            max_iter: 2000,
            lambda: 2.0,
            patience: 20,
            target_ratio: None,
        }
    }
}

pub fn held_karp_path_bound_with(d: &[Vec<i64>], upper: i64, cfg: &HkConfig) -> Option<Bound> {
    let max_iter = cfg.max_iter;
    let n = d.len();
    if n <= 1 {
        return Some(Bound {
            seconds: 0,
            iterations: 0,
        });
    }
    let big = i64::MAX / 4;
    let c = |i: usize, j: usize| -> i64 {
        let x = d[i][j].min(d[j][i]);
        if x >= big { big } else { x }
    };
    for i in 0..n {
        for j in 0..n {
            if i != j && c(i, j) >= big {
                return None;
            }
        }
    }
    let mut pi = vec![0.0f64; n];
    let mut best = f64::NEG_INFINITY;
    let mut lambda = cfg.lambda;
    let mut since_improve = 0;
    let mut iterations = 0;
    let mut in_tree = vec![false; n];
    let mut key = vec![f64::INFINITY; n];
    let mut parent = vec![usize::MAX; n];
    let mut deg = vec![0i32; n];
    for _ in 0..max_iter {
        iterations += 1;
        // Prim's MST over targets with penalised costs.
        in_tree.fill(false);
        key.fill(f64::INFINITY);
        parent.fill(usize::MAX);
        deg.fill(0);
        key[0] = 0.0;
        let mut mst = 0.0;
        for _ in 0..n {
            let mut u = usize::MAX;
            let mut ku = f64::INFINITY;
            for v in 0..n {
                if !in_tree[v] && key[v] < ku {
                    ku = key[v];
                    u = v;
                }
            }
            in_tree[u] = true;
            mst += ku;
            if parent[u] != usize::MAX {
                deg[u] += 1;
                deg[parent[u]] += 1;
            }
            for v in 0..n {
                if !in_tree[v] {
                    let w = c(u, v) as f64 + pi[u] + pi[v];
                    if w < key[v] {
                        key[v] = w;
                        parent[v] = u;
                    }
                }
            }
        }
        // The dummy joins the two targets with the smallest penalty (its
        // edges cost 0 plus the target's penalty).
        let (mut a, mut b) = (usize::MAX, usize::MAX);
        for v in 0..n {
            if a == usize::MAX || pi[v] < pi[a] {
                b = a;
                a = v;
            } else if b == usize::MAX || pi[v] < pi[b] {
                b = v;
            }
        }
        deg[a] += 1;
        deg[b] += 1;
        let tree = mst + pi[a] + pi[b];
        let lb = tree - 2.0 * pi.iter().sum::<f64>();
        if lb > best + 1e-9 {
            best = lb;
            since_improve = 0;
        } else {
            since_improve += 1;
            if since_improve >= cfg.patience {
                lambda /= 2.0;
                since_improve = 0;
            }
        }
        let norm: f64 = deg.iter().map(|&g| ((g - 2) as f64).powi(2)).sum();
        if norm == 0.0 {
            break; // The 1-tree is a Hamiltonian path: the bound is exact.
        }
        if lambda < 1e-6 {
            break;
        }
        let target = match cfg.target_ratio {
            Some(r) if best.is_finite() => (best * r).min(upper as f64),
            _ => upper as f64,
        };
        let step = lambda * (target - lb).max(1.0) / norm;
        for v in 0..n {
            pi[v] += step * (deg[v] - 2) as f64;
        }
    }
    // Path costs are integers, so any integer path length is at least
    // ceil(best); the small slack absorbs floating-point error.
    Some(Bound {
        seconds: (best - 1e-6).ceil().max(0.0) as Time,
        iterations,
    })
}

/// Timetable-aware bound: Held-Karp on the least time from any visit of
/// one target to the earliest reachable visit of another (see
/// [`crate::profile::min_visit_gaps`]). Pairs that can never follow each
/// other get a large finite cost, which keeps the relaxation valid because
/// no feasible itinerary uses them.
pub fn profile_lower_bound(net: &Network, upper: Time) -> Option<Bound> {
    let mut d = crate::profile::min_visit_gaps(net);
    let big = 10 * i64::from((net.window_end - net.window_start).max(1));
    for row in d.iter_mut() {
        for x in row.iter_mut() {
            if *x >= i64::from(INF) {
                *x = big;
            }
        }
    }
    held_karp_path_bound(&d, i64::from(upper.min(INF)), 2000)
}

/// The static bound for a network.
pub fn lower_bound(net: &Network, upper: Time) -> Option<Bound> {
    let d = target_distances(net);
    if d.iter().flatten().any(|&x| x == i64::MAX) {
        return None;
    }
    held_karp_path_bound(&d, i64::from(upper.min(INF)), 2000)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact shortest Hamiltonian path with free ends, by brute force.
    fn exact(d: &[Vec<i64>]) -> i64 {
        fn perm(
            d: &[Vec<i64>],
            used: &mut Vec<bool>,
            last: usize,
            acc: i64,
            best: &mut i64,
            k: usize,
        ) {
            if k == d.len() {
                *best = (*best).min(acc);
                return;
            }
            for v in 0..d.len() {
                if !used[v] {
                    used[v] = true;
                    perm(d, used, v, acc + d[last][v].min(d[v][last]), best, k + 1);
                    used[v] = false;
                }
            }
        }
        let mut best = i64::MAX;
        for s in 0..d.len() {
            let mut used = vec![false; d.len()];
            used[s] = true;
            perm(d, &mut used, s, 0, &mut best, 1);
        }
        best
    }

    #[test]
    fn bound_is_below_exact_path_on_random_metrics() {
        // Deterministic pseudo-random points on a line and in a plane.
        let mut seed = 12345u64;
        let mut rnd = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as i64 % 1000
        };
        for n in 2..8 {
            for _ in 0..30 {
                let pts: Vec<(i64, i64)> = (0..n).map(|_| (rnd(), rnd())).collect();
                let d: Vec<Vec<i64>> = pts
                    .iter()
                    .map(|a| {
                        pts.iter()
                            .map(|b| (a.0 - b.0).abs() + (a.1 - b.1).abs())
                            .collect()
                    })
                    .collect();
                let ex = exact(&d);
                let b = held_karp_path_bound(&d, ex, 500).unwrap();
                assert!(b.seconds as i64 <= ex, "bound {} > exact {ex}", b.seconds);
            }
        }
    }

    #[test]
    fn exact_on_a_line() {
        // Points on a line: the MST is the path, so the bound is exact.
        let xs = [0i64, 10, 25, 40, 70];
        let d: Vec<Vec<i64>> = xs
            .iter()
            .map(|a| xs.iter().map(|b| (a - b).abs()).collect())
            .collect();
        assert_eq!(held_karp_path_bound(&d, 70, 500).unwrap().seconds, 70);
    }

    #[test]
    fn disconnected_targets_have_no_bound() {
        let d = vec![vec![0, i64::MAX], vec![i64::MAX, 0]];
        assert!(held_karp_path_bound(&d, 0, 10).is_none());
    }
}
