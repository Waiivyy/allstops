//! Earliest-arrival routing with RAPTOR (Delling, Pajor, Werneck:
//! "Round-Based Public Transit Routing", ALENEX 2012; Transportation
//! Science 49(3), 2015).
//!
//! Computes exactly the `board` label of the Connection Scan engine in
//! [`crate::csa`] for an [`Origin::At`](crate::csa::Origin::At) query: for
//! every station, the earliest time a runner can be ready to board there.
//!
//! - The origin station is ready at the origin time.
//! - Arriving by train at a station where drop-off is allowed makes it ready
//!   at the arrival plus the station's change time.
//! - One walk may follow directly after alighting (starting at the arrival
//!   time, not after the change time) or leave from the origin; the walk's
//!   end is a ready time. Walks never chain.
//! - Boarding needs pickup allowed; departures after the network's
//!   `window_end` are ignored, as in the scan.
//!
//! Trips are grouped into route patterns: trips with the same stop sequence
//! and the same pickup and drop-off flags at every call. Each group is
//! split into FIFO patterns (no trip overtakes another), so the earliest
//! trip catchable at a stop is never beaten by a later one further on.
//!
//! Round `k` scans the patterns through stations improved in round `k - 1`,
//! from the earliest such stop, then relaxes walks from stations whose
//! train arrival improved. Boarding uses the best label found so far rather
//! than the previous round's, which only finds the same journeys sooner.
//! Rounds run until one improves nothing. That always happens within
//! `trips + 1` rounds (an optimal journey never boards the same trip twice,
//! since staying aboard is as fast), so the cap on rounds is only a guard.

use std::collections::HashMap;

use crate::network::{Connection, INF, Network, StationIdx, Time, TripIdx, flag};

const NO_POS: u32 = u32::MAX;

/// Pattern stop flags.
const BOARD: u8 = 1;
const ALIGHT: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StopTime {
    arr: Time,
    dep: Time,
}

#[derive(Debug, Clone, Copy)]
struct Pattern {
    /// Range start into [`Patterns::stations`] and [`Patterns::flags`].
    stops_start: u32,
    stops: u32,
    /// Range start into [`Patterns::times`]: trip row `r`, stop `i` is at
    /// `times_start + r * stops + i`. Rows are in FIFO order.
    times_start: u32,
    trips: u32,
}

/// Route patterns of a network, with a station-to-pattern index.
pub struct Patterns {
    patterns: Vec<Pattern>,
    stations: Vec<StationIdx>,
    flags: Vec<u8>,
    times: Vec<StopTime>,
    /// `(pattern, stop position)` of each station:
    /// `by_station[by_station_start[s]..by_station_start[s + 1]]`.
    by_station_start: Vec<u32>,
    by_station: Vec<(u32, u32)>,
    groups: usize,
}

/// The hops of `t` are weakly earlier than the hops of `u` at every call.
fn no_later(net: &Network, t: TripIdx, u: TripIdx) -> bool {
    let (a, b) = (net.trip_connections(t), net.trip_connections(u));
    a.iter().zip(b).all(|(&x, &y)| {
        let (x, y) = (&net.connections[x as usize], &net.connections[y as usize]);
        x.dep <= y.dep && x.arr <= y.arr
    })
}

impl Patterns {
    pub fn build(net: &Network) -> Patterns {
        // Group trips by stop sequence and flags, in order of first trip.
        let mut index: HashMap<Vec<u32>, usize> = HashMap::new();
        let mut groups: Vec<Vec<TripIdx>> = Vec::new();
        for t in 0..net.trips.len() as TripIdx {
            let conns = net.trip_connections(t);
            let Some(&last) = conns.last() else {
                continue;
            };
            let mut key = Vec::with_capacity(3 * conns.len() + 2);
            for &ci in conns {
                let c = &net.connections[ci as usize];
                key.extend([
                    c.dep_stop,
                    c.dep_station,
                    u32::from(c.flags & (flag::PICKUP | flag::DROP_OFF)),
                ]);
            }
            let l = &net.connections[last as usize];
            key.extend([l.arr_stop, l.arr_station]);
            let g = *index.entry(key).or_insert_with(|| {
                groups.push(Vec::new());
                groups.len() - 1
            });
            groups[g].push(t);
        }

        let mut out = Patterns {
            patterns: Vec::new(),
            stations: Vec::new(),
            flags: Vec::new(),
            times: Vec::new(),
            by_station_start: Vec::new(),
            by_station: Vec::new(),
            groups: groups.len(),
        };
        let hop_times = |t: TripIdx| {
            net.trip_connections(t)
                .iter()
                .flat_map(|&ci| {
                    let c = &net.connections[ci as usize];
                    [c.dep, c.arr]
                })
                .collect::<Vec<Time>>()
        };
        for mut trips in groups {
            trips.sort_by_cached_key(|&t| (hop_times(t), t));
            // Greedy split into FIFO chains: each trip joins the first chain
            // whose last trip is nowhere later than it.
            let mut chains: Vec<Vec<TripIdx>> = Vec::new();
            for t in trips {
                match chains
                    .iter_mut()
                    .find(|c| no_later(net, *c.last().expect("chains are non-empty"), t))
                {
                    Some(c) => c.push(t),
                    None => chains.push(vec![t]),
                }
            }
            for chain in chains {
                out.push_pattern(net, &chain);
            }
        }

        let ns = net.stations.len();
        let mut start = vec![0u32; ns + 1];
        for p in &out.patterns {
            let s0 = p.stops_start as usize;
            for &s in &out.stations[s0..s0 + p.stops as usize] {
                start[s as usize + 1] += 1;
            }
        }
        for i in 0..ns {
            start[i + 1] += start[i];
        }
        let mut fill = start.clone();
        let mut by_station = vec![(0, 0); start[ns] as usize];
        for (pi, p) in out.patterns.iter().enumerate() {
            let s0 = p.stops_start as usize;
            for (pos, &s) in out.stations[s0..s0 + p.stops as usize].iter().enumerate() {
                by_station[fill[s as usize] as usize] = (pi as u32, pos as u32);
                fill[s as usize] += 1;
            }
        }
        out.by_station_start = start;
        out.by_station = by_station;
        out
    }

    /// Append one FIFO pattern; `trips` share stops and flags.
    fn push_pattern(&mut self, net: &Network, trips: &[TripIdx]) {
        let first: Vec<&Connection> = net
            .trip_connections(trips[0])
            .iter()
            .map(|&ci| &net.connections[ci as usize])
            .collect();
        let hops = first.len();
        self.patterns.push(Pattern {
            stops_start: self.stations.len() as u32,
            stops: (hops + 1) as u32,
            times_start: self.times.len() as u32,
            trips: trips.len() as u32,
        });
        for i in 0..=hops {
            let mut f = 0;
            if i < hops && first[i].has(flag::PICKUP) {
                f |= BOARD;
            }
            if i > 0 && first[i - 1].has(flag::DROP_OFF) {
                f |= ALIGHT;
            }
            let station = if i < hops {
                first[i].dep_station
            } else {
                first[i - 1].arr_station
            };
            self.stations.push(station);
            self.flags.push(f);
        }
        for &t in trips {
            let conns = net.trip_connections(t);
            for i in 0..=hops {
                // The first stop's arrival and the last stop's departure are
                // never read for riding; fill them with the other time.
                let dep = conns.get(i).map(|&ci| net.connections[ci as usize].dep);
                let arr = i
                    .checked_sub(1)
                    .map(|k| net.connections[conns[k] as usize].arr);
                let (arr, dep) = match (arr, dep) {
                    (Some(a), Some(d)) => (a, d),
                    (None, Some(d)) => (d, d),
                    (Some(a), None) => (a, a),
                    (None, None) => unreachable!("a trip has at least one hop"),
                };
                self.times.push(StopTime { arr, dep });
            }
        }
    }

    /// Number of FIFO route patterns.
    pub fn len(&self) -> usize {
        self.patterns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Number of stop-sequence groups before the FIFO split; `len() -
    /// groups()` patterns exist only because trips overtake each other.
    pub fn groups(&self) -> usize {
        self.groups
    }

    fn at_station(&self, s: StationIdx) -> &[(u32, u32)] {
        let (a, b) = (
            self.by_station_start[s as usize],
            self.by_station_start[s as usize + 1],
        );
        &self.by_station[a as usize..b as usize]
    }
}

/// Per-query buffers, reused between queries.
struct State {
    /// Earliest ready-to-board time per station: the result.
    board: Vec<Time>,
    /// Earliest train arrival with drop-off allowed, where walks start.
    arrival: Vec<Time>,
    /// Stations whose `board` improved this round.
    marked: Vec<bool>,
    marked_list: Vec<StationIdx>,
    /// Stations improved in the previous round, consumed by this round.
    prev_list: Vec<StationIdx>,
    /// Stations whose `arrival` improved this round.
    walk_marked: Vec<bool>,
    walk_list: Vec<StationIdx>,
    /// Earliest stop position to scan each queued pattern from.
    queue_from: Vec<u32>,
    queue: Vec<u32>,
    rounds: usize,
}

impl State {
    fn new(stations: usize, patterns: usize) -> Self {
        State {
            board: vec![INF; stations],
            arrival: vec![INF; stations],
            marked: vec![false; stations],
            marked_list: Vec::new(),
            prev_list: Vec::new(),
            walk_marked: vec![false; stations],
            walk_list: Vec::new(),
            queue_from: vec![NO_POS; patterns],
            queue: Vec::new(),
            rounds: 0,
        }
    }

    fn reset(&mut self) {
        self.board.fill(INF);
        self.arrival.fill(INF);
        // Marks are cleared as rounds consume them; a run stopped by the
        // round cap may leave some behind.
        for &s in &self.marked_list {
            self.marked[s as usize] = false;
        }
        self.marked_list.clear();
        for &s in &self.walk_list {
            self.walk_marked[s as usize] = false;
        }
        self.walk_list.clear();
        self.rounds = 0;
    }

    fn improve_board(&mut self, s: usize, t: Time) {
        if t < self.board[s] {
            self.board[s] = t;
            if !self.marked[s] {
                self.marked[s] = true;
                self.marked_list.push(s as StationIdx);
            }
        }
    }

    fn improve_arrival(&mut self, s: usize, t: Time) {
        if t < self.arrival[s] {
            self.arrival[s] = t;
            if !self.walk_marked[s] {
                self.walk_marked[s] = true;
                self.walk_list.push(s as StationIdx);
            }
        }
    }
}

/// Reusable RAPTOR search over precomputed route patterns.
pub struct Raptor<'n> {
    pub net: &'n Network,
    patterns: Patterns,
    state: State,
}

impl<'n> Raptor<'n> {
    /// Build the route patterns of `net` and the query buffers.
    pub fn new(net: &'n Network) -> Self {
        let patterns = Patterns::build(net);
        let state = State::new(net.stations.len(), patterns.len());
        Raptor {
            net,
            patterns,
            state,
        }
    }

    pub fn patterns(&self) -> &Patterns {
        &self.patterns
    }

    /// Rounds run by the last query (each round boards one more trip).
    pub fn rounds(&self) -> usize {
        self.state.rounds
    }

    /// Earliest time a runner standing at `station` from `time` can be
    /// ready to board at every station (`INF` when unreachable). Equal to
    /// the Connection Scan `board` label for `Origin::At { station, time }`.
    pub fn earliest_board(&mut self, station: StationIdx, time: Time) -> &[Time] {
        let net = self.net;
        let pat = &self.patterns;
        let st = &mut self.state;
        st.reset();
        st.improve_board(station as usize, time);
        for f in net.footpaths_from(station) {
            st.improve_board(f.to as usize, time + f.duration);
        }
        let max_rounds = net.trips.len() + 1;
        while !st.marked_list.is_empty() && st.rounds < max_rounds {
            st.rounds += 1;
            // Queue every pattern through a station improved last round,
            // from its earliest such stop.
            std::mem::swap(&mut st.marked_list, &mut st.prev_list);
            for k in 0..st.prev_list.len() {
                let s = st.prev_list[k];
                st.marked[s as usize] = false;
                for &(p, pos) in pat.at_station(s) {
                    let q = &mut st.queue_from[p as usize];
                    if *q == NO_POS {
                        *q = pos;
                        st.queue.push(p);
                    } else if pos < *q {
                        *q = pos;
                    }
                }
            }
            st.prev_list.clear();
            for k in 0..st.queue.len() {
                let p = st.queue[k] as usize;
                let from = std::mem::replace(&mut st.queue_from[p], NO_POS);
                scan_pattern(net, pat, st, p, from as usize);
            }
            st.queue.clear();
            // One walk after alighting, from the arrival time.
            for k in 0..st.walk_list.len() {
                let s = st.walk_list[k];
                st.walk_marked[s as usize] = false;
                let a = st.arrival[s as usize];
                for f in net.footpaths_from(s) {
                    st.improve_board(f.to as usize, a + f.duration);
                }
            }
            st.walk_list.clear();
        }
        &self.state.board
    }
}

/// Ride pattern `p` from stop position `from` on: alight wherever allowed,
/// and switch to an earlier trip wherever the board label allows one.
fn scan_pattern(net: &Network, pat: &Patterns, st: &mut State, p: usize, from: usize) {
    let pt = pat.patterns[p];
    let n = pt.stops as usize;
    let rows = pt.trips as usize;
    let s0 = pt.stops_start as usize;
    let stations = &pat.stations[s0..s0 + n];
    let flags = &pat.flags[s0..s0 + n];
    let t0 = pt.times_start as usize;
    let times = &pat.times[t0..t0 + rows * n];
    // Current trip row; `rows` means none.
    let mut cur = rows;
    for i in from..n {
        let s = stations[i] as usize;
        if cur < rows && flags[i] & ALIGHT != 0 {
            let a = times[cur * n + i].arr;
            st.improve_board(s, a + net.change_time[s]);
            st.improve_arrival(s, a);
        }
        if flags[i] & BOARD != 0 {
            let ready = st.board[s];
            if ready < INF && (cur == rows || (cur > 0 && times[(cur - 1) * n + i].dep >= ready)) {
                // Earliest row before `cur` departing at or after `ready`;
                // rows are FIFO, so departures are sorted at every stop.
                let (mut lo, mut hi) = (0, cur);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    if times[mid * n + i].dep < ready {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                cur = lo;
            }
        }
        if cur < rows && times[cur * n + i].dep > net.window_end {
            // The scan ignores departures after the window: this trip
            // cannot be ridden on from here.
            cur = rows;
        }
    }
}
