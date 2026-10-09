//! rkyv mirrors of the core network types. The core crate carries no rkyv
//! derives, so the pack comparison copies a network into these types and
//! back. Field order and types match `allstops_core::network`.

use allstops_core::network as core;
use rkyv::{Archive, Deserialize, Serialize};

#[derive(Archive, Serialize, Deserialize)]
pub struct Station {
    pub id: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Archive, Serialize, Deserialize)]
pub struct Stop {
    pub id: String,
    pub station: u32,
    pub platform: String,
}

#[derive(Archive, Serialize, Deserialize)]
pub struct Trip {
    pub gtfs_id: String,
    pub service_date: String,
    pub offset: i32,
    pub route: String,
    pub headsign: String,
    pub route_type: u16,
    pub visits: bool,
    pub conns_start: u32,
    pub conns_end: u32,
    pub continues_as: Vec<TripPart>,
}

#[derive(Archive, Serialize, Deserialize)]
pub struct TripPart {
    pub gtfs_id: String,
    pub route: String,
    pub headsign: String,
    pub route_type: u16,
    pub first_hop: u16,
}

#[derive(Archive, Serialize, Deserialize)]
pub struct Connection {
    pub dep_station: u32,
    pub arr_station: u32,
    pub dep_stop: u32,
    pub arr_stop: u32,
    pub dep: i32,
    pub arr: i32,
    pub trip: u32,
    pub pos: u16,
    pub flags: u8,
}

#[derive(Archive, Serialize, Deserialize)]
pub struct Footpath {
    pub to: u32,
    pub duration: i32,
    pub metres: f32,
}

#[derive(Archive, Serialize, Deserialize)]
pub struct Network {
    pub stations: Vec<Station>,
    pub stops: Vec<Stop>,
    pub trips: Vec<Trip>,
    pub connections: Vec<Connection>,
    pub trip_conns: Vec<u32>,
    pub fp_start: Vec<u32>,
    pub footpaths: Vec<Footpath>,
    pub change_time: Vec<i32>,
    pub targets: Vec<u32>,
    pub window_start: i32,
    pub window_end: i32,
}

impl From<&core::Network> for Network {
    fn from(n: &core::Network) -> Self {
        Network {
            stations: n
                .stations
                .iter()
                .map(|s| Station {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    lat: s.lat,
                    lon: s.lon,
                })
                .collect(),
            stops: n
                .stops
                .iter()
                .map(|s| Stop {
                    id: s.id.clone(),
                    station: s.station,
                    platform: s.platform.clone(),
                })
                .collect(),
            trips: n
                .trips
                .iter()
                .map(|t| Trip {
                    gtfs_id: t.gtfs_id.clone(),
                    service_date: t.service_date.clone(),
                    offset: t.offset,
                    route: t.route.clone(),
                    headsign: t.headsign.clone(),
                    route_type: t.route_type,
                    visits: t.visits,
                    conns_start: t.conns_start,
                    conns_end: t.conns_end,
                    continues_as: t
                        .continues_as
                        .iter()
                        .map(|p| TripPart {
                            gtfs_id: p.gtfs_id.clone(),
                            route: p.route.clone(),
                            headsign: p.headsign.clone(),
                            route_type: p.route_type,
                            first_hop: p.first_hop,
                        })
                        .collect(),
                })
                .collect(),
            connections: n
                .connections
                .iter()
                .map(|c| Connection {
                    dep_station: c.dep_station,
                    arr_station: c.arr_station,
                    dep_stop: c.dep_stop,
                    arr_stop: c.arr_stop,
                    dep: c.dep,
                    arr: c.arr,
                    trip: c.trip,
                    pos: c.pos,
                    flags: c.flags,
                })
                .collect(),
            trip_conns: n.trip_conns.clone(),
            fp_start: n.fp_start.clone(),
            footpaths: n
                .footpaths
                .iter()
                .map(|f| Footpath {
                    to: f.to,
                    duration: f.duration,
                    metres: f.metres,
                })
                .collect(),
            change_time: n.change_time.clone(),
            targets: n.targets.clone(),
            window_start: n.window_start,
            window_end: n.window_end,
        }
    }
}

impl From<Network> for core::Network {
    fn from(n: Network) -> Self {
        core::Network {
            stations: n
                .stations
                .into_iter()
                .map(|s| core::Station {
                    id: s.id,
                    name: s.name,
                    lat: s.lat,
                    lon: s.lon,
                })
                .collect(),
            stops: n
                .stops
                .into_iter()
                .map(|s| core::Stop {
                    id: s.id,
                    station: s.station,
                    platform: s.platform,
                })
                .collect(),
            trips: n
                .trips
                .into_iter()
                .map(|t| core::Trip {
                    gtfs_id: t.gtfs_id,
                    service_date: t.service_date,
                    offset: t.offset,
                    route: t.route,
                    headsign: t.headsign,
                    route_type: t.route_type,
                    visits: t.visits,
                    conns_start: t.conns_start,
                    conns_end: t.conns_end,
                    continues_as: t
                        .continues_as
                        .into_iter()
                        .map(|p| core::TripPart {
                            gtfs_id: p.gtfs_id,
                            route: p.route,
                            headsign: p.headsign,
                            route_type: p.route_type,
                            first_hop: p.first_hop,
                        })
                        .collect(),
                })
                .collect(),
            connections: n
                .connections
                .into_iter()
                .map(|c| core::Connection {
                    dep_station: c.dep_station,
                    arr_station: c.arr_station,
                    dep_stop: c.dep_stop,
                    arr_stop: c.arr_stop,
                    dep: c.dep,
                    arr: c.arr,
                    trip: c.trip,
                    pos: c.pos,
                    flags: c.flags,
                })
                .collect(),
            trip_conns: n.trip_conns,
            fp_start: n.fp_start,
            footpaths: n
                .footpaths
                .into_iter()
                .map(|f| core::Footpath {
                    to: f.to,
                    duration: f.duration,
                    metres: f.metres,
                })
                .collect(),
            change_time: n.change_time,
            targets: n.targets,
            window_start: n.window_start,
            window_end: n.window_end,
        }
    }
}
