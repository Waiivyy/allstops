//! Profile scans: for one destination, the earliest time it can be visited
//! from every state of the timetable, computed with one backward scan over
//! the connections (after the profile variant of the Connection Scan
//! Algorithm, Dibbelt et al., ACM JEA 23, 2018, section 4).
//!
//! [`ProfileTo`] answers profile queries: the earliest visit of one
//! destination as a function of the time a runner is ready at any station,
//! over the whole time window, so "what if I leave six minutes later" costs a
//! lookup instead of a scan.
//!
//! The timetable-aware lower bound uses the same scan: [`min_visit_gaps`]
//! gives, for every ordered pair of targets (i, j), the least time from any
//! moment at which `i` is visited (aboard a train stopping there, or boarding
//! one) to the earliest visit of `j` reachable from that moment. Waiting and
//! change times are included; only the order of visits is relaxed.

use crate::network::{ConnIdx, INF, Network, StationIdx, Time, flag};

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

/// Earliest visits of one destination from every station and every time:
/// the result of one backward scan.
pub struct ProfileTo {
    dest: StationIdx,
    /// Per station: boarding a train there at or after a time.
    ready: Vec<Staircase>,
    /// Per connection: aboard its trip at its arrival.
    aboard_arr: Vec<Time>,
}

impl ProfileTo {
    /// Scan the network backwards once towards `dest`.
    pub fn new(net: &Network, dest: StationIdx) -> Self {
        let mut ready = vec![Staircase::default(); net.stations.len()];
        let mut trip_best = vec![INF; net.trips.len()];
        let aboard_arr = scan_to(net, dest, &mut ready, &mut trip_best);
        ProfileTo {
            dest,
            ready,
            aboard_arr,
        }
    }

    pub fn dest(&self) -> StationIdx {
        self.dest
    }

    /// Earliest visit of the destination for a runner standing at
    /// `station` from time `t`, free to board there or to walk once and
    /// board where the walk ends: the same as a forward scan from
    /// [`crate::csa::Origin::At`]. `INF` when the destination cannot be
    /// visited inside the window.
    pub fn earliest_visit(&self, net: &Network, station: StationIdx, t: Time) -> Time {
        let mut best = self.ready[station as usize].query(t);
        for f in net.footpaths_from(station) {
            best = best.min(self.ready[f.to as usize].query(t + f.duration));
        }
        best
    }

    /// Earliest visit of the destination for a runner aboard the trip of
    /// connection `conn` as it arrives (who may stay aboard, or alight if
    /// drop-off is allowed).
    pub fn earliest_visit_aboard(&self, conn: ConnIdx) -> Time {
        self.aboard_arr[conn as usize]
    }

    /// The whole profile at `station`: the pairs (latest time to be ready
    /// at `station`, earliest visit of the destination), with both
    /// strictly increasing. Ready at `t`, the earliest visit is that of the
    /// first pair whose time is at least `t`.
    pub fn pairs(&self, net: &Network, station: StationIdx) -> Vec<(Time, Time)> {
        let mut all: Vec<(Time, Time)> = self.ready[station as usize].0.clone();
        for f in net.footpaths_from(station) {
            all.extend(
                self.ready[f.to as usize]
                    .0
                    .iter()
                    .map(|&(dep, v)| (dep - f.duration, v)),
            );
        }
        // Latest ready time first; keep a pair only if it visits earlier
        // than every pair with a later ready time.
        all.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let mut out: Vec<(Time, Time)> = Vec::with_capacity(all.len());
        for (r, v) in all {
            if out.last().is_none_or(|&(_, lv)| v < lv) {
                out.push((r, v));
            }
        }
        out.reverse();
        out
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
