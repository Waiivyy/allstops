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
        if calls.len() < 2 {
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
