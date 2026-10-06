//! Earliest-arrival routing with the Connection Scan Algorithm (Dibbelt,
//! Pajor, Strasser, Wagner: "Connection Scan Algorithm", ACM JEA 23, 2018,
//! arXiv:1703.05997), extended with visit labels.
//!
//! Connections are scanned once in departure order. A trip becomes
//! reachable when its departure can be boarded; every later connection of
//! a reachable trip is then reachable too. Two labels per station:
//!
//! - `board`: earliest time a departure from the station can be caught.
//!   Arriving by train adds the station's change time; arriving on foot
//!   does not (walk durations already include the walk-link minimum).
//! - `visit`: earliest time the station is visited, either aboard a
//!   visiting trip with a scheduled stop there or by boarding such a trip
//!   there.
//!
//! Walks are single legs and are never chained, so the footpath set does
//! not need to be transitively closed: the rules forbid walking from one
//! station through another to a third.

use crate::network::{ConnIdx, INF, Network, StationIdx, Time, TripIdx, flag};

const NONE: u32 = u32::MAX;

/// Where a search starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Standing at a station, free to board from `time`.
    At { station: StationIdx, time: Time },
    /// Aboard `trip`, which has just arrived at the arrival station of its
    /// hop number `pos` (so the next hop is `pos + 1`) at `time`.
    Aboard { trip: TripIdx, pos: u16, time: Time },
    /// At the departure station of hop `pos` of `trip` at its departure
    /// `time`, having just boarded it. The runner may stay on it or take
    /// another train from this station, but not start with a walk.
    Boarding { trip: TripIdx, pos: u16, time: Time },
}

impl Origin {
    /// The station and time the search starts from.
    pub fn place(&self, net: &Network) -> (StationIdx, Time) {
        match *self {
            Origin::At { station, time } => (station, time),
            Origin::Aboard { trip, pos, time } => {
                let c = &net.connections[net.trip_connections(trip)[pos as usize] as usize];
                (c.arr_station, time)
            }
            Origin::Boarding { trip, pos, time } => {
                let c = &net.connections[net.trip_connections(trip)[pos as usize] as usize];
                (c.dep_station, time)
            }
        }
    }
}

/// How a station's `board` label was reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardVia {
    None,
    Origin,
    /// Alighted from a trip ridden from `enter` (or from the origin trip
    /// when `enter == NONE`) to `exit`.
    Alight {
        enter: ConnIdx,
        exit: ConnIdx,
    },
    /// Walked from the station where `exit` arrived (or from the origin
    /// when `exit == NONE`), along footpath `fp` of that station.
    Walk {
        enter: ConnIdx,
        exit: ConnIdx,
        fp: u32,
    },
}

/// How a station's `visit` label was reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisitVia {
    None,
    /// The origin itself (only when starting aboard at this station).
    Origin,
    /// Aboard a trip ridden from `enter` (NONE: origin trip) whose hop
    /// `exit` arrives here.
    Ride {
        enter: ConnIdx,
        exit: ConnIdx,
    },
    /// Boarding the trip of connection `conn` here.
    Board {
        conn: ConnIdx,
    },
}

pub struct Labels {
    pub board: Vec<Time>,
    pub board_via: Vec<BoardVia>,
    pub visit: Vec<Time>,
    pub visit_via: Vec<VisitVia>,
    /// First connection index of each reachable trip, NONE if unreachable.
    trip_enter: Vec<u32>,
    /// Lowest hop position usable on each reachable trip.
    trip_from: Vec<u16>,
    /// Earliest time walks have been started from each station after
    /// alighting. A walk from a later alighting there is dominated.
    walked_from: Vec<Time>,
    pub origin: Origin,
    pub scanned: usize,
}

/// Reusable search state, so repeated queries do not reallocate.
pub struct Csa<'n> {
    pub net: &'n Network,
    labels: Labels,
    touched_trips: Vec<TripIdx>,
}

pub const ORIGIN_TRIP: u32 = NONE - 1;

impl<'n> Csa<'n> {
    pub fn new(net: &'n Network) -> Self {
        let ns = net.stations.len();
        Csa {
            net,
            labels: Labels {
                board: vec![INF; ns],
                board_via: vec![BoardVia::None; ns],
                visit: vec![INF; ns],
                visit_via: vec![VisitVia::None; ns],
                trip_enter: vec![NONE; net.trips.len()],
                trip_from: vec![0; net.trips.len()],
                walked_from: vec![INF; ns],
                origin: Origin::At {
                    station: 0,
                    time: 0,
                },
                scanned: 0,
            },
            touched_trips: Vec::new(),
        }
    }

    fn reset(&mut self) {
        let l = &mut self.labels;
        l.board.fill(INF);
        l.walked_from.fill(INF);
        l.board_via.fill(BoardVia::None);
        l.visit.fill(INF);
        l.visit_via.fill(VisitVia::None);
        for &t in &self.touched_trips {
            l.trip_enter[t as usize] = NONE;
        }
        self.touched_trips.clear();
        l.scanned = 0;
    }

    /// Run a scan from `origin`. When `stop_when` is given, the scan stops as
    /// soon as no remaining connection can improve the earliest visit to any
    /// station flagged `true` in it.
    pub fn run(&mut self, origin: Origin, stop_when: Option<&[bool]>) -> &Labels {
        self.reset();
        let net = self.net;
        let l = &mut self.labels;
        l.origin = origin;

        let start_time = match origin {
            Origin::At { station, time } => {
                l.board[station as usize] = time;
                l.board_via[station as usize] = BoardVia::Origin;
                relax_walks(net, l, station, time, NONE, NONE);
                time
            }
            Origin::Aboard { trip, pos, time } => {
                let conns = net.trip_connections(trip);
                let here = &net.connections[conns[pos as usize] as usize];
                let station = here.arr_station;
                l.trip_enter[trip as usize] = ORIGIN_TRIP;
                l.trip_from[trip as usize] = pos + 1;
                self.touched_trips.push(trip);
                l.visit[station as usize] = time;
                l.visit_via[station as usize] = VisitVia::Origin;
                if here.has(flag::DROP_OFF) {
                    let b = time + net.change_time[station as usize];
                    l.board[station as usize] = b;
                    l.board_via[station as usize] = BoardVia::Alight {
                        enter: ORIGIN_TRIP,
                        exit: conns[pos as usize],
                    };
                    l.walked_from[station as usize] = time;
                    relax_walks(net, l, station, time, ORIGIN_TRIP, conns[pos as usize]);
                }
                time
            }
            Origin::Boarding { trip, pos, time } => {
                let conns = net.trip_connections(trip);
                let station = net.connections[conns[pos as usize] as usize].dep_station;
                l.trip_enter[trip as usize] = ORIGIN_TRIP;
                l.trip_from[trip as usize] = pos;
                self.touched_trips.push(trip);
                l.visit[station as usize] = time;
                l.visit_via[station as usize] = VisitVia::Origin;
                l.board[station as usize] = time;
                l.board_via[station as usize] = BoardVia::Origin;
                // No walk from here: the runner may have walked in, and
                // walks never follow walks. Another train is fine.
                time
            }
        };

        // Earliest visit among the stop set, for early termination.
        let mut best_stop = INF;
        let mut first = net.first_conn_at(start_time);
        let next_hop = match origin {
            Origin::Aboard { trip, pos, .. } => Some((trip, pos as usize + 1)),
            Origin::Boarding { trip, pos, .. } => Some((trip, pos as usize)),
            Origin::At { .. } => None,
        };
        if let Some((trip, k)) = next_hop
            && let Some(&next) = net.trip_connections(trip).get(k)
        {
            // Hops of the origin trip may depart at exactly `time` but sort
            // before it; make sure the next hop is not skipped.
            first = first.min(next as usize);
        }
        for ci in first..net.connections.len() {
            let c = &net.connections[ci];
            if c.dep >= best_stop {
                break;
            }
            if c.dep > net.window_end {
                break;
            }
            l.scanned += 1;
            let t = c.trip as usize;
            let entered = l.trip_enter[t] != NONE;
            if entered && c.pos < l.trip_from[t] {
                // A hop before the point the trip was entered.
                continue;
            }
            if !entered {
                if !(c.has(flag::PICKUP) && l.board[c.dep_station as usize] <= c.dep) {
                    continue;
                }
                l.trip_enter[t] = ci as u32;
                l.trip_from[t] = c.pos;
                self.touched_trips.push(c.trip);
                if c.has(flag::VISIT_DEP) && c.dep < l.visit[c.dep_station as usize] {
                    l.visit[c.dep_station as usize] = c.dep;
                    l.visit_via[c.dep_station as usize] = VisitVia::Board { conn: ci as u32 };
                    if let Some(s) = stop_when
                        && s[c.dep_station as usize]
                    {
                        best_stop = best_stop.min(c.dep);
                    }
                }
            }
            let enter = l.trip_enter[t];
            let a = c.arr_station as usize;
            if c.has(flag::VISIT_ARR) && c.arr <= net.window_end && c.arr < l.visit[a] {
                l.visit[a] = c.arr;
                l.visit_via[a] = VisitVia::Ride {
                    enter,
                    exit: ci as u32,
                };
                if let Some(s) = stop_when
                    && s[a]
                {
                    best_stop = best_stop.min(c.arr);
                }
            }
            if c.has(flag::DROP_OFF) {
                let b = c.arr + net.change_time[a];
                if b < l.board[a] {
                    l.board[a] = b;
                    l.board_via[a] = BoardVia::Alight {
                        enter,
                        exit: ci as u32,
                    };
                }
                // Walks from an earlier alighting here reach everything no
                // later, so only an earlier alighting needs to walk.
                if c.arr < l.walked_from[a] {
                    l.walked_from[a] = c.arr;
                    relax_walks(net, l, c.arr_station, c.arr, enter, ci as u32);
                }
            }
        }
        &self.labels
    }

    pub fn labels(&self) -> &Labels {
        &self.labels
    }

    /// Rebuild the journey that reaches the `visit` label of `target`.
    pub fn journey_to_visit(&self, target: StationIdx) -> Option<Journey> {
        let l = &self.labels;
        let net = self.net;
        let mut legs: Vec<JLeg> = Vec::new();
        let end;
        match l.visit_via[target as usize] {
            VisitVia::None => return None,
            VisitVia::Origin => {
                return Some(Journey {
                    legs,
                    end: self.origin_end(),
                });
            }
            VisitVia::Ride { enter, exit } => {
                let c = &net.connections[exit as usize];
                end = End::Aboard {
                    trip: c.trip,
                    pos: c.pos,
                    time: c.arr,
                };
                self.push_ride(&mut legs, enter, exit);
                self.walk_back(&mut legs, enter)?;
            }
            VisitVia::Board { conn } => {
                let c = &net.connections[conn as usize];
                // Visit happens by boarding; the journey ends at the moment
                // of departure, aboard, before the first hop.
                end = End::Boarding {
                    trip: c.trip,
                    pos: c.pos,
                    time: c.dep,
                };
                self.back_from_board(&mut legs, c.dep_station)?;
            }
        }
        legs.reverse();
        Some(Journey { legs, end })
    }

    fn origin_end(&self) -> End {
        match self.labels.origin {
            Origin::At { station, time } => End::At { station, time },
            Origin::Aboard { trip, pos, time } => End::Aboard { trip, pos, time },
            Origin::Boarding { trip, pos, time } => End::Boarding { trip, pos, time },
        }
    }

    fn push_ride(&self, legs: &mut Vec<JLeg>, enter: ConnIdx, exit: ConnIdx) {
        let net = self.net;
        let x = &net.connections[exit as usize];
        let from_pos = if enter == ORIGIN_TRIP {
            self.labels.trip_from[x.trip as usize]
        } else {
            net.connections[enter as usize].pos
        };
        if x.pos < from_pos {
            // Alighting or walking off the origin trip at the origin
            // station itself: no ride happened in this journey.
            return;
        }
        legs.push(JLeg::Ride {
            trip: x.trip,
            from_pos,
            to_pos: x.pos,
            continues_origin: enter == ORIGIN_TRIP,
        });
    }

    /// Before riding from `enter`: how the runner got to its departure station.
    fn walk_back(&self, legs: &mut Vec<JLeg>, enter: ConnIdx) -> Option<()> {
        if enter == ORIGIN_TRIP {
            return Some(());
        }
        let c = &self.net.connections[enter as usize];
        self.back_from_board(legs, c.dep_station)
    }

    fn back_from_board(&self, legs: &mut Vec<JLeg>, station: StationIdx) -> Option<()> {
        let net = self.net;
        let mut station = station;
        // Each step moves strictly back in time, so this terminates; the
        // bound guards against a corrupted label graph.
        for _ in 0..net.connections.len() + 2 {
            match self.labels.board_via[station as usize] {
                BoardVia::None => return None,
                BoardVia::Origin => return Some(()),
                BoardVia::Alight { enter, exit } => {
                    self.push_ride(legs, enter, exit);
                    if enter == ORIGIN_TRIP {
                        return Some(());
                    }
                    station = net.connections[enter as usize].dep_station;
                }
                BoardVia::Walk { enter, exit, fp } => {
                    let (from, start) = if exit == NONE {
                        match self.labels.origin {
                            Origin::Aboard { .. } => return None,
                            o => o.place(net),
                        }
                    } else {
                        let x = &net.connections[exit as usize];
                        (x.arr_station, x.arr)
                    };
                    let f = net.footpaths[fp as usize];
                    legs.push(JLeg::Walk {
                        from,
                        to: f.to,
                        start,
                        end: start + f.duration,
                        metres: f.metres,
                    });
                    if exit == NONE {
                        return Some(());
                    }
                    self.push_ride(legs, enter, exit);
                    if enter == ORIGIN_TRIP {
                        return Some(());
                    }
                    station = net.connections[enter as usize].dep_station;
                }
            }
        }
        None
    }
}

fn relax_walks(net: &Network, l: &mut Labels, from: StationIdx, at: Time, enter: u32, exit: u32) {
    let base = net.fp_start[from as usize];
    for (k, f) in net.footpaths_from(from).iter().enumerate() {
        let t = at + f.duration;
        if t < l.board[f.to as usize] {
            l.board[f.to as usize] = t;
            l.board_via[f.to as usize] = BoardVia::Walk {
                enter,
                exit,
                fp: base + k as u32,
            };
        }
    }
}

/// One leg of a journey, in network terms.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JLeg {
    /// Ride `trip` from the departure stop of hop `from_pos` to the arrival
    /// stop of hop `to_pos`.
    Ride {
        trip: TripIdx,
        from_pos: u16,
        to_pos: u16,
        /// The runner was already aboard at the start of this leg.
        continues_origin: bool,
    },
    Walk {
        from: StationIdx,
        to: StationIdx,
        start: Time,
        end: Time,
        metres: f32,
    },
}

/// Where a journey leaves the runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    At {
        station: StationIdx,
        time: Time,
    },
    /// Aboard `trip`, at the arrival station of hop `pos`.
    Aboard {
        trip: TripIdx,
        pos: u16,
        time: Time,
    },
    /// About to depart aboard `trip` on hop `pos` (boarded, not yet moved).
    Boarding {
        trip: TripIdx,
        pos: u16,
        time: Time,
    },
}

impl End {
    pub fn time(&self) -> Time {
        match *self {
            End::At { time, .. } | End::Aboard { time, .. } | End::Boarding { time, .. } => time,
        }
    }

    /// The search origin that continues from here.
    pub fn as_origin(&self) -> Origin {
        match *self {
            End::At { station, time } => Origin::At { station, time },
            End::Aboard { trip, pos, time } => Origin::Aboard { trip, pos, time },
            End::Boarding { trip, pos, time } => Origin::Boarding { trip, pos, time },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Journey {
    pub legs: Vec<JLeg>,
    pub end: End,
}
