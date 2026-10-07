//! Assemble a [`Network`] from trips given as stop sequences.

use crate::network::{
    ConnIdx, Connection, Footpath, Network, Station, StationIdx, Stop, Time, Trip, flag,
};

/// One scheduled call of a trip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Call {
    pub stop: u32,
    pub station: StationIdx,
    pub arr: Time,
    pub dep: Time,
    pub pickup: bool,
    pub drop_off: bool,
    /// A scheduled stop that can count as a visit (not a pass-through,
    /// unless pass-throughs count under the rules).
    pub counts: bool,
}

pub struct NetworkBuilder {
    stations: Vec<Station>,
    stops: Vec<Stop>,
    trips: Vec<Trip>,
    hops: Vec<Connection>,
    footpaths: Vec<(StationIdx, Footpath)>,
    change_time: Vec<Time>,
    default_change: Time,
    targets: Vec<StationIdx>,
    window: (Time, Time),
}

impl NetworkBuilder {
    pub fn new(window_start: Time, window_end: Time, default_change: Time) -> Self {
        NetworkBuilder {
            stations: Vec::new(),
            stops: Vec::new(),
            trips: Vec::new(),
            hops: Vec::new(),
            footpaths: Vec::new(),
            change_time: Vec::new(),
            default_change,
            targets: Vec::new(),
            window: (window_start, window_end),
        }
    }

    pub fn add_station(&mut self, s: Station) -> StationIdx {
        self.stations.push(s);
        self.change_time.push(self.default_change);
        (self.stations.len() - 1) as StationIdx
    }

    pub fn add_stop(&mut self, s: Stop) -> u32 {
        self.stops.push(s);
        (self.stops.len() - 1) as u32
    }

    pub fn set_change_time(&mut self, s: StationIdx, t: Time) {
        self.change_time[s as usize] = t;
    }

    pub fn add_target(&mut self, s: StationIdx) {
        self.targets.push(s);
    }

    pub fn add_footpath(&mut self, from: StationIdx, to: StationIdx, duration: Time, metres: f32) {
        self.footpaths.push((
            from,
            Footpath {
                to,
                duration,
                metres,
            },
        ));
    }

    /// Add a trip. `trip.visits` decides whether riding it can count as a
    /// visit; the hop ranges are filled in by [`Self::build`]. Calls must be
    /// in order with non-decreasing times; trips with fewer than two calls
    /// are ignored.
    pub fn add_trip(&mut self, trip: Trip, calls: &[Call]) -> Option<u32> {
        // Hop positions are u16: at most 65,536 hops, so 65,537 calls.
        if calls.len() < 2 || calls.len() - 1 > u16::MAX as usize + 1 {
            return None;
        }
        let ti = self.trips.len() as u32;
        for (k, w) in calls.windows(2).enumerate() {
            let (a, b) = (w[0], w[1]);
            let mut flags = 0;
            if a.pickup {
                flags |= flag::PICKUP;
            }
            if b.drop_off {
                flags |= flag::DROP_OFF;
            }
            if trip.visits && a.counts {
                flags |= flag::VISIT_DEP;
            }
            if trip.visits && b.counts {
                flags |= flag::VISIT_ARR;
            }
            self.hops.push(Connection {
                dep_station: a.station,
                arr_station: b.station,
                dep_stop: a.stop,
                arr_stop: b.stop,
                dep: a.dep,
                arr: b.arr.max(a.dep),
                trip: ti,
                pos: k as u16,
                flags,
            });
        }
        self.trips.push(trip);
        Some(ti)
    }

    pub fn build(self) -> Network {
        let NetworkBuilder {
            stations,
            stops,
            mut trips,
            mut hops,
            footpaths,
            change_time,
            targets,
            window,
            ..
        } = self;
        hops.sort_by_key(|c| (c.dep, c.arr, c.trip, c.pos));
        let mut per_trip: Vec<Vec<ConnIdx>> = vec![Vec::new(); trips.len()];
        for (i, c) in hops.iter().enumerate() {
            per_trip[c.trip as usize].push(i as ConnIdx);
        }
        let mut trip_conns = Vec::with_capacity(hops.len());
        for (t, list) in per_trip.into_iter().enumerate() {
            let mut list = list;
            list.sort_by_key(|&ci| hops[ci as usize].pos);
            trips[t].conns_start = trip_conns.len() as u32;
            trip_conns.extend(list);
            trips[t].conns_end = trip_conns.len() as u32;
        }

        let mut fps = footpaths;
        fps.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.to.cmp(&b.1.to)));
        fps.dedup_by(|b, a| a.0 == b.0 && a.1.to == b.1.to);
        let mut fp_start = vec![0u32; stations.len() + 1];
        for (from, _) in &fps {
            fp_start[*from as usize + 1] += 1;
        }
        for i in 0..stations.len() {
            fp_start[i + 1] += fp_start[i];
        }
        let mut targets = targets;
        targets.sort_unstable();
        targets.dedup();

        Network {
            stations,
            stops,
            trips,
            connections: hops,
            trip_conns,
            fp_start,
            footpaths: fps.into_iter().map(|(_, f)| f).collect(),
            change_time,
            targets,
            window_start: window.0,
            window_end: window.1,
        }
    }
}

/// Helpers for small hand-written and random test networks.
pub mod test_support {
    use super::*;

    pub fn station(name: &str) -> Station {
        Station {
            id: name.to_string(),
            name: name.to_string(),
            lat: 0.0,
            lon: 0.0,
        }
    }

    pub fn trip(name: &str, visits: bool) -> Trip {
        Trip {
            gtfs_id: name.to_string(),
            service_date: "2026-11-14".into(),
            offset: 0,
            route: name.to_string(),
            headsign: String::new(),
            route_type: if visits { 1 } else { 3 },
            visits,
            conns_start: 0,
            conns_end: 0,
        }
    }

    /// A call where everything is allowed and the stop counts.
    pub fn call(station: StationIdx, arr: Time, dep: Time) -> Call {
        Call {
            stop: station,
            station,
            arr,
            dep,
            pickup: true,
            drop_off: true,
            counts: true,
        }
    }

    /// A builder with one stop per station, named `S0`, `S1`, ...
    pub fn with_stations(n: usize, change: Time) -> NetworkBuilder {
        let mut b = NetworkBuilder::new(0, 48 * 3600, change);
        for i in 0..n {
            let s = b.add_station(station(&format!("S{i}")));
            b.add_stop(Stop {
                id: format!("S{i}"),
                station: s,
                platform: String::new(),
            });
        }
        b
    }
}

/// Seeded random networks for tests and benchmarks.
pub mod random {
    use super::test_support::{station, trip};
    use super::{Call, NetworkBuilder};
    use crate::network::{Network, Stop};

    /// A small deterministic generator (64-bit LCG), so tests need no
    /// external crate and results are identical on every platform.
    pub struct Lcg(u64);

    impl Lcg {
        pub fn new(seed: u64) -> Self {
            Lcg(seed ^ 0x9E37_79B9_7F4A_7C15)
        }
        pub fn next_u64(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }
        pub fn below(&mut self, n: u64) -> u64 {
            self.next_u64() % n.max(1)
        }
        pub fn chance(&mut self, percent: u64) -> bool {
            self.below(100) < percent
        }
    }

    /// A small network: a few metro lines (which count as visits) with
    /// several trips each in both directions, an occasional connector line
    /// and a few walk links. Every station on a metro line is a target.
    pub fn network(seed: u64, max_stations: usize) -> Network {
        let mut r = Lcg::new(seed);
        let n = 3 + r.below((max_stations.max(3) - 2) as u64) as usize;
        let change = 30 + r.below(120) as i32;
        let mut b = NetworkBuilder::new(0, 6 * 3600, change);
        for i in 0..n {
            let s = b.add_station(station(&format!("S{i}")));
            b.add_stop(Stop {
                id: format!("S{i}"),
                station: s,
                platform: String::new(),
            });
        }
        let mut is_target = vec![false; n];
        let lines = 1 + r.below(3);
        for li in 0..lines + 1 {
            let metro = li < lines || r.chance(30);
            let len = 2 + r.below(4) as usize;
            let mut seq: Vec<u32> = Vec::new();
            while seq.len() < len {
                let s = r.below(n as u64) as u32;
                if seq.last() != Some(&s) && !seq.contains(&s) {
                    seq.push(s);
                }
                if seq.len() >= n {
                    break;
                }
            }
            if seq.len() < 2 {
                continue;
            }
            let hops: Vec<i32> = (0..seq.len()).map(|_| 60 + r.below(300) as i32).collect();
            let headway = 300 + r.below(900) as i32;
            let first = r.below(1800) as i32;
            for dir in 0..2 {
                let order: Vec<u32> = if dir == 0 {
                    seq.clone()
                } else {
                    seq.iter().rev().copied().collect()
                };
                for k in 0..(3 + r.below(4)) as i32 {
                    let mut t = first + k * headway + dir * 120;
                    let mut calls = Vec::new();
                    for (i, &s) in order.iter().enumerate() {
                        let dwell = 20;
                        calls.push(Call {
                            stop: s,
                            station: s,
                            arr: t,
                            dep: t + dwell,
                            pickup: i + 1 < order.len() && !r.chance(5),
                            drop_off: i > 0 && !r.chance(5),
                            counts: !r.chance(5),
                        });
                        t += dwell + hops[i];
                    }
                    if metro {
                        for c in &calls {
                            if c.counts {
                                is_target[c.station as usize] = true;
                            }
                        }
                    }
                    b.add_trip(trip(&format!("L{li}D{dir}K{k}"), metro), &calls);
                }
            }
        }
        for _ in 0..r.below(4) {
            let (a, c) = (r.below(n as u64) as u32, r.below(n as u64) as u32);
            if a != c {
                let d = 120 + r.below(600) as i32;
                b.add_footpath(a, c, d, d as f32);
                b.add_footpath(c, a, d, d as f32);
            }
        }
        for (s, t) in is_target.iter().enumerate() {
            if *t {
                b.add_target(s as u32);
            }
        }
        b.build()
    }
}
