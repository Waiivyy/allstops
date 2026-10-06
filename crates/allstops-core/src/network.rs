//! The routing network for one plan: stations, trips and elementary
//! connections on a single time line, plus walking links.
//!
//! All times are seconds relative to the origin ("noon minus 12 hours") of
//! the plan date's service day. Trips of the previous and next service days
//! are shifted onto this line when the network is built.

use serde::{Deserialize, Serialize};

pub type Time = i32;
pub type StationIdx = u32;
pub type TripIdx = u32;
pub type ConnIdx = u32;

pub const INF: Time = Time::MAX / 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Station {
    pub id: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stop {
    pub id: String,
    pub station: StationIdx,
    pub platform: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trip {
    pub gtfs_id: String,
    /// `YYYY-MM-DD` of the GTFS service day this trip instance belongs to.
    pub service_date: String,
    /// Plan time minus GTFS service-day time, for this trip instance.
    pub offset: Time,
    pub route: String,
    pub headsign: String,
    pub route_type: u16,
    /// Whether riding this trip can count as a visit.
    pub visits: bool,
    /// Range into [`Network::trip_conns`].
    pub conns_start: u32,
    pub conns_end: u32,
}

/// Flags on a connection.
pub mod flag {
    /// Boarding allowed at the departure stop.
    pub const PICKUP: u8 = 1;
    /// Alighting allowed at the arrival stop.
    pub const DROP_OFF: u8 = 2;
    /// The departure stop counts as a visit when boarding there.
    pub const VISIT_DEP: u8 = 4;
    /// The arrival stop counts as a visit for a runner aboard.
    pub const VISIT_ARR: u8 = 8;
}

/// One vehicle hop between two consecutive stops of a trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub dep_station: StationIdx,
    pub arr_station: StationIdx,
    pub dep_stop: u32,
    pub arr_stop: u32,
    pub dep: Time,
    pub arr: Time,
    pub trip: TripIdx,
    /// Index of this hop within its trip (0 for the first).
    pub pos: u16,
    pub flags: u8,
}

impl Connection {
    pub fn has(&self, f: u8) -> bool {
        self.flags & f != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Footpath {
    pub to: StationIdx,
    /// Seconds, already including the walk-link minimum.
    pub duration: Time,
    pub metres: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Network {
    pub stations: Vec<Station>,
    pub stops: Vec<Stop>,
    pub trips: Vec<Trip>,
    /// Sorted by departure time, then arrival, trip and position.
    pub connections: Vec<Connection>,
    /// Connection indices of each trip in order, grouped by trip.
    pub trip_conns: Vec<ConnIdx>,
    /// Walking links, grouped by origin station: `footpaths[fp_start[s]..fp_start[s + 1]]`.
    pub fp_start: Vec<u32>,
    pub footpaths: Vec<Footpath>,
    /// Minimum time to change between two trips at each station.
    pub change_time: Vec<Time>,
    /// Stations that must be visited.
    pub targets: Vec<StationIdx>,
    /// Plan time window.
    pub window_start: Time,
    pub window_end: Time,
}

impl Network {
    pub fn footpaths_from(&self, s: StationIdx) -> &[Footpath] {
        let (a, b) = (self.fp_start[s as usize], self.fp_start[s as usize + 1]);
        &self.footpaths[a as usize..b as usize]
    }

    pub fn trip_connections(&self, t: TripIdx) -> &[ConnIdx] {
        let trip = &self.trips[t as usize];
        &self.trip_conns[trip.conns_start as usize..trip.conns_end as usize]
    }

    /// Index of the first connection departing at or after `t`.
    pub fn first_conn_at(&self, t: Time) -> usize {
        self.connections.partition_point(|c| c.dep < t)
    }

    pub fn target_mask(&self) -> Vec<bool> {
        let mut m = vec![false; self.stations.len()];
        for &t in &self.targets {
            m[t as usize] = true;
        }
        m
    }

    /// Check the invariants the routing code relies on.
    pub fn validate(&self) -> Result<(), String> {
        let ns = self.stations.len() as u32;
        if self.fp_start.len() != self.stations.len() + 1
            || self.change_time.len() != self.stations.len()
        {
            return Err("station-indexed arrays have the wrong length".into());
        }
        for w in self.connections.windows(2) {
            if w[0].dep > w[1].dep {
                return Err("connections are not sorted by departure".into());
            }
        }
        for c in &self.connections {
            if c.arr < c.dep || c.dep_station >= ns || c.arr_station >= ns {
                return Err(format!("bad connection {c:?}"));
            }
        }
        for (ti, _) in self.trips.iter().enumerate() {
            let cs = self.trip_connections(ti as u32);
            for (k, &ci) in cs.iter().enumerate() {
                let c = &self.connections[ci as usize];
                if c.trip != ti as u32 || c.pos as usize != k {
                    return Err(format!("trip {ti} connection list is inconsistent"));
                }
                if k > 0 {
                    let p = &self.connections[cs[k - 1] as usize];
                    if p.arr_station != c.dep_station || p.arr > c.dep {
                        return Err(format!("trip {ti} hops do not chain"));
                    }
                }
            }
        }
        for t in &self.targets {
            if *t >= ns {
                return Err("target out of range".into());
            }
        }
        Ok(())
    }
}
