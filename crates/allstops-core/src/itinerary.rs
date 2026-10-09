//! The canonical itinerary: a versioned JSON document that names trips and
//! stops by their GTFS IDs, so it can be checked against the raw feed by a
//! verifier that shares no code with the solver.
//!
//! Times:
//! - ride `board_time` / `alight_time` are GTFS times of the trip's own
//!   `service_date`, exactly as they appear in stop_times.txt;
//! - walk and wait times, and the summary, are clock times of the plan
//!   `date`'s service day (seconds after noon minus 12 hours), formatted
//!   `HH:MM:SS` and possibly past 24:00.

use serde::{Deserialize, Serialize};

use crate::csa::JLeg;
use crate::network::{Network, Time, flag};
use crate::plan::{Plan, visits};
use crate::rules::Rules;

pub const SCHEMA: &str = "allstops-itinerary/0";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedRef {
    pub id: String,
    pub sha256: String,
    pub feed_version: String,
    pub attribution: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Leg {
    Ride {
        trip_id: String,
        service_date: String,
        route: String,
        headsign: String,
        board_stop_id: String,
        board_time: String,
        alight_stop_id: String,
        alight_time: String,
        /// Stations this leg visits, in order.
        stations: Vec<String>,
        /// The runner stayed aboard from the previous ride: this trip is the
        /// one the vehicle continues as after the previous trip's terminus,
        /// and nobody alights or boards in between. Only written when true.
        #[serde(default, skip_serializing_if = "is_false")]
        stay_aboard: bool,
    },
    Walk {
        from_station: String,
        to_station: String,
        start: String,
        end: String,
        metres: f64,
        walking_speed_kmh: f64,
    },
    Wait {
        station: String,
        start: String,
        end: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub first_visit: String,
    pub last_visit: String,
    pub duration_s: i32,
    pub stations_visited: usize,
    pub transfers: usize,
    pub walk_m: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Itinerary {
    pub schema: String,
    pub feed: FeedRef,
    pub timezone: String,
    /// Plan date; walk, wait and summary times count from its service day.
    pub date: String,
    pub rules: Rules,
    pub targets: Vec<String>,
    pub legs: Vec<Leg>,
    pub summary: Summary,
    pub lower_bound_s: Option<i32>,
    pub gap: Option<f64>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// The GTFS trips one network ride covers, as (part, first hop, last hop).
/// A network trip holds several GTFS trips when the runner may stay aboard
/// through a terminus; the hop joining two of them is in neither range.
pub fn ride_parts(net: &Network, trip: u32, from_pos: u16, to_pos: u16) -> Vec<(usize, u16, u16)> {
    let t = &net.trips[trip as usize];
    let mut out = Vec::new();
    let mut p = from_pos;
    loop {
        let part = t.part_of_hop(p);
        match t.continues_as.get(part).map(|next| next.first_hop) {
            // The ride goes on into the next trip: this part ends at its
            // last stop, the arrival of the hop before the joining hop.
            Some(next_first) if to_pos >= next_first => {
                if p + 2 <= next_first {
                    out.push((part, p, next_first - 2));
                }
                p = next_first;
            }
            _ => {
                // A ride never ends on a joining hop (nobody can alight
                // there); if it did, it ends at the earlier trip's terminus.
                let last = match t.continues_as.get(part) {
                    Some(next) if to_pos + 1 == next.first_hop => to_pos.saturating_sub(1),
                    _ => to_pos,
                };
                if last >= p {
                    out.push((part, p, last));
                }
                return out;
            }
        }
    }
}

/// Stations a ride visits, in order: the boarding station and every later
/// call that counts, with immediate repeats dropped. A loop line that comes
/// back to an earlier station lists it again.
pub fn ride_stations(net: &Network, trip: u32, from_pos: u16, to_pos: u16) -> Vec<String> {
    let conns = net.trip_connections(trip);
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: u32| {
        let id = &net.stations[s as usize].id;
        if out.last() != Some(id) {
            out.push(id.clone());
        }
    };
    for p in from_pos..=to_pos {
        let c = &net.connections[conns[p as usize] as usize];
        if p == from_pos && c.has(flag::VISIT_DEP) {
            push(c.dep_station);
        }
        if c.has(flag::VISIT_ARR) {
            push(c.arr_station);
        }
    }
    out
}

/// `HH:MM:SS`, hours may exceed 23; negative times get a leading `-`.
pub fn clock(t: Time) -> String {
    let sign = if t < 0 { "-" } else { "" };
    let a = t.unsigned_abs();
    format!("{sign}{:02}:{:02}:{:02}", a / 3600, (a / 60) % 60, a % 60)
}

/// Turn a plan into the canonical itinerary. Returns `None` if the plan
/// does not visit every target.
pub fn to_itinerary(
    net: &Network,
    plan: &Plan,
    rules: &Rules,
    feed: FeedRef,
    timezone: &str,
) -> Option<Itinerary> {
    let (first, last) = plan.duration(net)?;
    let mut legs = Vec::new();
    let mut clock_at: Option<(u32, Time)> = None;
    let mut transfers = 0usize;
    let mut walk_m = 0.0f64;
    let mut rides = 0usize;
    for leg in &plan.legs {
        let (start, end, out) = match *leg {
            JLeg::Ride {
                trip,
                from_pos,
                to_pos,
                ..
            } => {
                let t = &net.trips[trip as usize];
                let cs = net.trip_connections(trip);
                rides += 1;
                let mut parts = Vec::new();
                for (k, (part, p, q)) in ride_parts(net, trip, from_pos, to_pos)
                    .into_iter()
                    .enumerate()
                {
                    let a = &net.connections[cs[p as usize] as usize];
                    let b = &net.connections[cs[q as usize] as usize];
                    let (gtfs_id, route, headsign) = match part {
                        0 => (&t.gtfs_id, &t.route, &t.headsign),
                        n => {
                            let x = &t.continues_as[n - 1];
                            (&x.gtfs_id, &x.route, &x.headsign)
                        }
                    };
                    parts.push((
                        a.dep,
                        b.arr,
                        Leg::Ride {
                            trip_id: gtfs_id.clone(),
                            service_date: t.service_date.clone(),
                            route: route.clone(),
                            headsign: headsign.clone(),
                            board_stop_id: net.stops[a.dep_stop as usize].id.clone(),
                            board_time: clock(a.dep - t.offset),
                            alight_stop_id: net.stops[b.arr_stop as usize].id.clone(),
                            alight_time: clock(b.arr - t.offset),
                            stations: ride_stations(net, trip, p, q),
                            stay_aboard: k > 0,
                        },
                    ));
                }
                let Some(last) = parts.pop() else {
                    continue;
                };
                let start = parts.first().map_or(last.0, |p| p.0);
                if let Some((s, t)) = clock_at
                    && start > t
                {
                    legs.push(Leg::Wait {
                        station: net.stations[s as usize].id.clone(),
                        start: clock(t),
                        end: clock(start),
                    });
                }
                legs.extend(parts.into_iter().map(|p| p.2));
                clock_at = None;
                (last.0, last.1, last.2)
            }
            JLeg::Walk {
                from,
                to,
                start,
                end,
                metres,
            } => {
                walk_m += metres as f64;
                (
                    start,
                    end,
                    Leg::Walk {
                        from_station: net.stations[from as usize].id.clone(),
                        to_station: net.stations[to as usize].id.clone(),
                        start: clock(start),
                        end: clock(end),
                        metres: (metres as f64 * 10.0).round() / 10.0,
                        walking_speed_kmh: rules.walking_speed_kmh,
                    },
                )
            }
        };
        if let Some((s, t)) = clock_at
            && start > t
        {
            legs.push(Leg::Wait {
                station: net.stations[s as usize].id.clone(),
                start: clock(t),
                end: clock(start),
            });
        }
        legs.push(out);
        let end_station = match *leg {
            JLeg::Ride { trip, to_pos, .. } => {
                net.connections[net.trip_connections(trip)[to_pos as usize] as usize].arr_station
            }
            JLeg::Walk { to, .. } => to,
        };
        clock_at = Some((end_station, end));
    }
    if rides > 0 {
        transfers = rides - 1;
    }
    let v = visits(net, &plan.legs);
    Some(Itinerary {
        schema: SCHEMA.into(),
        feed,
        timezone: timezone.into(),
        date: rules.date.clone(),
        rules: rules.clone(),
        targets: net
            .targets
            .iter()
            .map(|&t| net.stations[t as usize].id.clone())
            .collect(),
        legs,
        summary: Summary {
            first_visit: clock(first),
            last_visit: clock(last),
            duration_s: last - first,
            stations_visited: v.len(),
            transfers,
            walk_m: (walk_m * 10.0).round() / 10.0,
        },
        lower_bound_s: None,
        gap: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::test_support::{call, trip, with_stations};

    #[test]
    fn a_loop_lists_its_return_to_the_first_station() {
        let mut b = with_stations(3, 60);
        b.add_trip(
            trip("ring", true),
            &[
                call(0, 0, 0),
                call(1, 100, 100),
                call(2, 200, 200),
                call(0, 300, 300),
            ],
        );
        let net = b.build();
        assert_eq!(ride_stations(&net, 0, 0, 2), vec!["S0", "S1", "S2", "S0"]);
        assert_eq!(ride_stations(&net, 0, 1, 2), vec!["S1", "S2", "S0"]);
    }

    #[test]
    fn a_ride_through_a_terminus_is_written_as_two_trips() {
        use crate::network::TripPart;
        let mut b = with_stations(3, 60);
        let mut t = trip("A", true);
        t.continues_as.push(TripPart {
            gtfs_id: "B".into(),
            route: "U2".into(),
            headsign: "Three".into(),
            route_type: 1,
            first_hop: 2,
        });
        let mut a_end = call(1, 100, 100);
        a_end.pickup = false;
        let mut b_start = call(1, 160, 160);
        b_start.drop_off = false;
        b.add_trip(t, &[call(0, 0, 0), a_end, b_start, call(2, 300, 300)]);
        for s in 0..3 {
            b.add_target(s);
        }
        let net = b.build();
        let plan = Plan {
            legs: vec![JLeg::Ride {
                trip: 0,
                from_pos: 0,
                to_pos: 2,
                continues_origin: false,
            }],
        };
        let feed = FeedRef {
            id: "t".into(),
            sha256: String::new(),
            feed_version: String::new(),
            attribution: String::new(),
        };
        let it = to_itinerary(&net, &plan, &Rules::default(), feed, "Europe/Berlin").unwrap();
        type Row<'a> = (
            &'a str,
            &'a str,
            &'a str,
            &'a str,
            &'a str,
            bool,
            Vec<String>,
        );
        let rides: Vec<Row> = it
            .legs
            .iter()
            .map(|l| match l {
                Leg::Ride {
                    trip_id,
                    board_stop_id,
                    board_time,
                    alight_stop_id,
                    alight_time,
                    stay_aboard,
                    stations,
                    ..
                } => (
                    trip_id.as_str(),
                    board_stop_id.as_str(),
                    board_time.as_str(),
                    alight_stop_id.as_str(),
                    alight_time.as_str(),
                    *stay_aboard,
                    stations.clone(),
                ),
                other => panic!("unexpected leg {other:?}"),
            })
            .collect();
        let st = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            rides,
            vec![
                (
                    "A",
                    "S0",
                    "00:00:00",
                    "S1",
                    "00:01:40",
                    false,
                    st(&["S0", "S1"])
                ),
                (
                    "B",
                    "S1",
                    "00:02:40",
                    "S2",
                    "00:05:00",
                    true,
                    st(&["S1", "S2"])
                ),
            ]
        );
        assert_eq!(it.summary.transfers, 0, "staying aboard is not a change");
        let json = serde_json::to_value(&it.legs[0]).unwrap();
        assert!(json.get("stay_aboard").is_none(), "only written when true");
    }
}
