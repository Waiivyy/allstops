//! Profile scans: for one destination, the earliest time it can be visited
//! from every state of the timetable, computed with one backward scan over
//! the connections (after the profile variant of the Connection Scan
//! Algorithm, Dibbelt et al., ACM JEA 23, 2018, section 4).
//!
//! Used for the timetable-aware lower bound: [`min_visit_gaps`] gives, for
//! every ordered pair of targets (i, j), the least time from any moment at
//! which `i` is visited (aboard a train stopping there, or boarding one) to
//! the earliest visit of `j` reachable from that moment. Waiting and change
//! times are included; only the order of visits is relaxed.

use crate::network::{INF, Network, StationIdx, Time, flag};

/// Earliest visit of a destination when ready to board at a station from a
/// given time on: a staircase of `(departure, visit)` pairs, both strictly
/// decreasing in insertion order.
#[derive(Clone, Default)]
struct Staircase(Vec<(Time, Time)>);

impl Staircase {
    /// Earliest visit when ready at time `t`.
    fn query(&self, t: Time) -> Time {
        // Entries are sorted by departure, decreasing. The last entry whose
        // departure is >= t has the smallest visit among those.
        let n = self.0.partition_point(|&(dep, _)| dep >= t);
        if n == 0 { INF } else { self.0[n - 1].1 }
    }

    fn insert(&mut self, dep: Time, visit: Time) {
        match self.0.last_mut() {
            Some(last) if visit >= last.1 => {}
            Some(last) if last.0 == dep => last.1 = visit,
            _ => self.0.push((dep, visit)),
        }
    }
}

/// One backward scan towards destination `dest`. Returns, per connection,
/// the earliest visit of `dest` for a runner aboard that connection's trip
/// at the connection's arrival (`aboard_arr`), indexed like
/// `net.connections`.
fn scan_to(
    net: &Network,
    dest: StationIdx,
    ready: &mut [Staircase],
    trip_best: &mut [Time],
) -> Vec<Time> {
    for s in ready.iter_mut() {
        s.0.clear();
    }
    trip_best.fill(INF);
    let mut aboard_arr = vec![INF; net.connections.len()];
    for ci in (0..net.connections.len()).rev() {
        let c = &net.connections[ci];
        if c.dep > net.window_end {
            continue;
        }
        let a = c.arr_station;
        // Options after arriving at `a` aboard this trip.
        let mut best = trip_best[c.trip as usize];
        if a == dest && c.has(flag::VISIT_ARR) && c.arr <= net.window_end {
            best = best.min(c.arr);
        }
        if c.has(flag::DROP_OFF) {
            best = best.min(ready[a as usize].query(c.arr + net.change_time[a as usize]));
            for f in net.footpaths_from(a) {
                best = best.min(ready[f.to as usize].query(c.arr + f.duration));
            }
        }
        aboard_arr[ci] = best;
        // Aboard before this hop departs: same as arriving aboard.
        trip_best[c.trip as usize] = best;
        if c.has(flag::PICKUP) {
            let mut v = best;
            if c.dep_station == dest && c.has(flag::VISIT_DEP) {
                v = c.dep;
            }
            ready[c.dep_station as usize].insert(c.dep, v);
        }
    }
    aboard_arr
}

/// Least time from a visit of target `i` to the earliest visit of target
/// `j` reachable from it, for all ordered pairs of targets (`INF` when no
/// visit of `i` leads to `j` inside the window). Diagonal entries are 0.
pub fn min_visit_gaps(net: &Network) -> Vec<Vec<i64>> {
    let n = net.targets.len();
    let mut idx = vec![usize::MAX; net.stations.len()];
    for (k, &t) in net.targets.iter().enumerate() {
        idx[t as usize] = k;
    }
    let mut out = vec![vec![i64::from(INF); n]; n];
    let mut ready = vec![Staircase::default(); net.stations.len()];
    let mut trip_best = vec![INF; net.trips.len()];
    for (jk, &j) in net.targets.iter().enumerate() {
        let aboard_arr = scan_to(net, j, &mut ready, &mut trip_best);
        for (ci, c) in net.connections.iter().enumerate() {
            if c.dep > net.window_end {
                continue;
            }
            // Visit of i by arriving aboard: continue from the arrival.
            let ia = idx[c.arr_station as usize];
            if ia != usize::MAX
                && ia != jk
                && c.has(flag::VISIT_ARR)
                && c.arr <= net.window_end
                && aboard_arr[ci] < INF
            {
                let gap = i64::from(aboard_arr[ci] - c.arr);
                out[ia][jk] = out[ia][jk].min(gap);
            }
            // Visit of i by boarding: aboard from the departure on, which
            // leads to the same options as arriving aboard at this hop's end.
            let id = idx[c.dep_station as usize];
            if id != usize::MAX
                && id != jk
                && c.has(flag::VISIT_DEP)
                && c.has(flag::PICKUP)
                && aboard_arr[ci] < INF
            {
                let gap = i64::from(aboard_arr[ci] - c.dep);
                out[id][jk] = out[id][jk].min(gap);
            }
        }
        out[jk][jk] = 0;
    }
    out
}
