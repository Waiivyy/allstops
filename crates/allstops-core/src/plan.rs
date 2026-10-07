//! Plans as sequences of legs, the visits they make, and the greedy
//! construction heuristic.

use crate::csa::{Csa, End, JLeg, Origin};
use crate::network::{INF, Network, StationIdx, Time, TripIdx, flag};

/// A route in network terms. The visited set is always recomputed from the
/// legs with [`visits`]; nothing else is trusted.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub legs: Vec<JLeg>,
}

/// The first time each station is visited along `legs`, in visit order.
/// A ride visits the station where it is boarded (if that call counts) and
/// every later call it reaches that counts. Walks visit nothing.
pub fn visits(net: &Network, legs: &[JLeg]) -> Vec<(StationIdx, Time)> {
    let mut first = vec![INF; net.stations.len()];
    let mut order = Vec::new();
    let mut mark = |s: StationIdx, t: Time, order: &mut Vec<(StationIdx, Time)>| {
        if t < first[s as usize] {
            if first[s as usize] == INF {
                order.push((s, t));
            }
            first[s as usize] = t;
        }
    };
    for leg in legs {
        if let JLeg::Ride {
            trip,
            from_pos,
            to_pos,
            continues_origin,
        } = *leg
        {
            let conns = net.trip_connections(trip);
            for p in from_pos..=to_pos {
                let c = &net.connections[conns[p as usize] as usize];
                if p == from_pos && !continues_origin && c.has(flag::VISIT_DEP) {
                    mark(c.dep_station, c.dep, &mut order);
                }
                if c.has(flag::VISIT_ARR) {
                    mark(c.arr_station, c.arr, &mut order);
                }
            }
        }
    }
    // Times can only improve for already-seen stations if legs go back in
    // time, which valid plans never do; keep the recorded order.
    order
        .into_iter()
        .map(|(s, _)| (s, first[s as usize]))
        .collect()
}

impl Plan {
    /// Time from the first visit to the last visit of a target, if every
    /// target is visited.
    pub fn duration(&self, net: &Network) -> Option<(Time, Time)> {
        let v = visits(net, &self.legs);
        let is_target = net.target_mask();
        let mut seen = 0;
        let mut lo = INF;
        let mut hi = -INF;
        for (s, t) in v {
            if is_target[s as usize] {
                seen += 1;
                lo = lo.min(t);
                hi = hi.max(t);
            }
        }
        (seen == net.targets.len()).then_some((lo, hi))
    }

    /// Append a journey's legs, merging a ride that continues the previous
    /// ride on the same trip.
    pub fn extend(&mut self, legs: &[JLeg]) {
        for &leg in legs {
            if let (
                Some(JLeg::Ride {
                    trip: pt,
                    to_pos: pend,
                    ..
                }),
                JLeg::Ride {
                    trip,
                    from_pos,
                    to_pos,
                    continues_origin: true,
                },
            ) = (self.legs.last_mut(), leg)
                && *pt == trip
                && (*pend + 1 == from_pos || *pend == from_pos)
            {
                *pend = (*pend).max(to_pos);
                continue;
            }
            let leg = match leg {
                JLeg::Ride {
                    trip,
                    from_pos,
                    to_pos,
                    ..
                } => JLeg::Ride {
                    trip,
                    from_pos,
                    to_pos,
                    continues_origin: false,
                },
                w => w,
            };
            self.legs.push(leg);
        }
    }
}

/// Nearest-unvisited-target greedy: from the current place, go to whichever
/// unvisited target can be visited earliest, then repeat. Returns `None`
/// when some target cannot be reached inside the window.
pub fn greedy(csa: &mut Csa, start: StationIdx, t0: Time) -> Option<Plan> {
    let net = csa.net;
    let is_target = net.target_mask();
    let mut plan = Plan { legs: Vec::new() };
    let mut origin = Origin::At {
        station: start,
        time: t0,
    };
    let mut last_end: Option<End> = None;
    // A target visited by boarding at the end of a journey has no ride leg
    // yet; it counts as visited only for choosing the next target.
    let mut pending: Option<StationIdx> = None;
    // Every round makes progress or re-queues at most one lost boarding.
    for _ in 0..4 * net.targets.len() + 4 {
        let mut done = vec![false; net.stations.len()];
        for (s, _) in visits(net, &plan.legs) {
            done[s as usize] = true;
        }
        if let Some(p) = pending.take() {
            done[p as usize] = true;
        }
        let unvisited: Vec<bool> = (0..net.stations.len())
            .map(|s| is_target[s] && !done[s])
            .collect();
        if !unvisited.iter().any(|&u| u) {
            if plan.duration(net).is_none()
                && let Some(End::Boarding { trip, pos, .. }) = last_end
            {
                plan.extend(&[JLeg::Ride {
                    trip,
                    from_pos: pos,
                    to_pos: pos,
                    continues_origin: false,
                }]);
            }
            end_where_alighting_is_allowed(net, &mut plan);
            return plan.duration(net).map(|_| plan);
        }
        // A run from `start` begins by visiting `start` whenever it can,
        // even if another target could be visited sooner by walking there.
        // The search for it stops at `start` alone, so an earlier visit
        // elsewhere cannot end it first.
        let mut target = None;
        if plan.legs.is_empty() && pending.is_none() && unvisited[start as usize] {
            let mut only = vec![false; net.stations.len()];
            only[start as usize] = true;
            if csa.run(origin, Some(&only)).visit[start as usize] < INF {
                target = Some(start);
            }
        }
        if target.is_none() {
            let labels = csa.run(origin, Some(&unvisited));
            let mut best: Option<(Time, StationIdx)> = None;
            for (s, &u) in unvisited.iter().enumerate() {
                if u && labels.visit[s] < INF && best.is_none_or(|(t, _)| labels.visit[s] < t) {
                    best = Some((labels.visit[s], s as StationIdx));
                }
            }
            target = best.map(|(_, s)| s);
        }
        // The journey comes from whichever search found the target last.
        let target = target?;
        let journey = csa.journey_to_visit(target)?;
        plan.extend(&journey.legs);
        if matches!(journey.end, End::Boarding { .. }) {
            pending = Some(target);
        }
        last_end = Some(journey.end);
        origin = journey.end.as_origin();
    }
    None
}

/// A greedy result and the start it came from.
/// If the last ride ends where alighting is not allowed, ride on to the
/// first stop where it is. Every target is already visited, so the total
/// time does not change; the run simply has to end somewhere the runner can
/// get off.
fn end_where_alighting_is_allowed(net: &Network, plan: &mut Plan) {
    if let Some(JLeg::Ride { trip, to_pos, .. }) = plan.legs.last_mut() {
        let conns = net.trip_connections(*trip);
        let mut p = *to_pos as usize;
        while !net.connections[conns[p] as usize].has(flag::DROP_OFF) && p + 1 < conns.len() {
            p += 1;
        }
        *to_pos = p as u16;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Best {
    pub plan: Plan,
    /// First and last target visit.
    pub first: Time,
    pub last: Time,
    pub start: StationIdx,
    pub t0: Time,
}

/// [`greedy`] from `start` at `t0`, kept only when it visits every target.
pub fn greedy_from(csa: &mut Csa, start: StationIdx, t0: Time) -> Option<Best> {
    let plan = greedy(csa, start, t0)?;
    let (first, last) = plan.duration(csa.net)?;
    Some(Best {
        plan,
        first,
        last,
        start,
        t0,
    })
}

/// Every (start station, start time) pair, station by station: the runs
/// [`best_greedy`] makes, in its order.
pub fn greedy_jobs(starts: &[StationIdx], times: &[Time]) -> Vec<(StationIdx, Time)> {
    starts
        .iter()
        .flat_map(|&s| times.iter().map(move |&t| (s, t)))
        .collect()
}

/// The shortest of some greedy results, and how many there were. Ties go to
/// the earlier start time and then the lower station index, so the choice
/// does not depend on the order the results arrive in (or on thread
/// scheduling when they are computed in parallel).
pub fn shortest(results: impl IntoIterator<Item = Option<Best>>) -> (Option<Best>, usize) {
    let mut feasible = 0;
    let best = results
        .into_iter()
        .flatten()
        .inspect(|_| feasible += 1)
        .min_by_key(|b| (b.last - b.first, b.t0, b.start));
    (best, feasible)
}

/// Run the greedy from every start station at every start time on this
/// thread and keep the shortest result (see [`shortest`] for ties). Also
/// returns how many runs visited every target.
pub fn best_greedy(net: &Network, starts: &[StationIdx], times: &[Time]) -> (Option<Best>, usize) {
    let mut csa = Csa::new(net);
    shortest(
        greedy_jobs(starts, times)
            .into_iter()
            .map(|(s, t0)| greedy_from(&mut csa, s, t0)),
    )
}

/// A change between two rides of a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    /// Index into the plan's legs of the ride boarded after the change.
    pub leg: usize,
    /// Station where that ride is boarded.
    pub station: StationIdx,
    /// Whether the runner walked between the two rides.
    pub walked: bool,
    /// Seconds to spare; negative when the plan breaks the change rules.
    pub slack: Time,
}

/// Every change between two rides of `plan`, with its slack: the next
/// departure minus the earliest moment it could be caught. Staying at the
/// station, that moment is the previous arrival plus the station's change
/// time; after a walk it is the end of the walk (walk durations already
/// include the walk-link minimum). Staying aboard the same trip is not a
/// change.
pub fn transfer_slacks(net: &Network, plan: &Plan) -> Vec<Change> {
    let mut out = Vec::new();
    // Trip, alighting station and arrival of the last ride.
    let mut prev: Option<(TripIdx, StationIdx, Time)> = None;
    let mut walk_end: Option<Time> = None;
    for (i, leg) in plan.legs.iter().enumerate() {
        match *leg {
            JLeg::Walk { end, .. } => walk_end = Some(end),
            JLeg::Ride {
                trip,
                from_pos,
                to_pos,
                ..
            } => {
                let cs = net.trip_connections(trip);
                let board = &net.connections[cs[from_pos as usize] as usize];
                let alight = &net.connections[cs[to_pos as usize] as usize];
                if let Some((prev_trip, at, arr)) = prev
                    && prev_trip != trip
                {
                    let ready = walk_end.unwrap_or(arr + net.change_time[at as usize]);
                    out.push(Change {
                        leg: i,
                        station: board.dep_station,
                        walked: walk_end.is_some(),
                        slack: board.dep - ready,
                    });
                }
                prev = Some((trip, alight.arr_station, alight.arr));
                walk_end = None;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::test_support::{call, trip, with_stations};

    #[test]
    fn a_run_from_a_station_starts_by_visiting_it() {
        // S0's first departure is at 1000; S1, a 120 s walk away, has one at
        // 200. A run from S0 must still visit S0 first.
        let mut b = with_stations(3, 60);
        b.add_footpath(0, 1, 120, 100.0);
        b.add_trip(
            trip("late", true),
            &[call(0, 1000, 1000), call(2, 1100, 1100)],
        );
        b.add_trip(trip("early", true), &[call(1, 200, 200), call(2, 300, 300)]);
        b.add_trip(
            trip("back", true),
            &[call(2, 1200, 1200), call(1, 1300, 1300)],
        );
        for t in 0..3 {
            b.add_target(t);
        }
        let net = b.build();
        let mut csa = Csa::new(&net);
        let p = greedy(&mut csa, 0, 0).expect("a route");
        assert_eq!(visits(&net, &p.legs)[0], (0, 1000));
    }

    #[test]
    fn the_last_ride_ends_where_alighting_is_allowed() {
        let mut b = with_stations(3, 60);
        let mut calls = vec![call(0, 0, 0), call(1, 100, 100), call(2, 200, 200)];
        calls[1].drop_off = false;
        b.add_trip(trip("A", true), &calls);
        b.add_target(0);
        b.add_target(1);
        let net = b.build();
        let mut csa = Csa::new(&net);
        let p = greedy(&mut csa, 0, 0).expect("a route");
        assert!(
            matches!(p.legs.last(), Some(JLeg::Ride { to_pos: 1, .. })),
            "the ride goes on to S2, where alighting is allowed: {:?}",
            p.legs
        );
        assert_eq!(p.duration(&net), Some((0, 100)), "the time is unchanged");
    }

    #[test]
    fn a_one_way_pair_still_has_a_static_bound() {
        let mut b = with_stations(2, 60);
        b.add_trip(trip("A", true), &[call(0, 0, 0), call(1, 100, 100)]);
        b.add_target(0);
        b.add_target(1);
        let net = b.build();
        let bound = crate::bound::lower_bound(&net, 100).expect("a bound");
        assert_eq!(bound.seconds, 100);
    }

    /// Trips 0 (S0 to S1), 1 and 2 (S1 to S2, the second too early for a
    /// 60 s change), 3 (S3 to S4, S3 a 150 s walk from S1) and 4 (S2, S3,
    /// S4).
    fn net() -> Network {
        let mut b = with_stations(5, 60);
        b.add_trip(trip("A", true), &[call(0, 100, 100), call(1, 200, 200)]);
        b.add_trip(trip("B", true), &[call(1, 300, 300), call(2, 400, 400)]);
        b.add_trip(trip("C", true), &[call(1, 230, 230), call(2, 330, 330)]);
        b.add_trip(trip("D", true), &[call(3, 400, 400), call(4, 500, 500)]);
        b.add_trip(
            trip("E", true),
            &[call(2, 600, 600), call(3, 700, 700), call(4, 800, 800)],
        );
        b.add_footpath(1, 3, 150, 140.0);
        let net = b.build();
        net.validate().unwrap();
        net
    }

    fn ride(trip: u32, from_pos: u16, to_pos: u16) -> JLeg {
        JLeg::Ride {
            trip,
            from_pos,
            to_pos,
            continues_origin: false,
        }
    }

    fn walk(from: StationIdx, to: StationIdx, start: Time, end: Time) -> JLeg {
        JLeg::Walk {
            from,
            to,
            start,
            end,
            metres: 140.0,
        }
    }

    #[test]
    fn change_at_a_station_counts_the_change_time() {
        let net = net();
        let plan = Plan {
            legs: vec![ride(0, 0, 0), ride(1, 0, 0)],
        };
        assert_eq!(
            transfer_slacks(&net, &plan),
            vec![Change {
                leg: 1,
                station: 1,
                walked: false,
                slack: 300 - (200 + 60),
            }]
        );
        // A departure inside the change time gives a negative slack.
        let plan = Plan {
            legs: vec![ride(0, 0, 0), ride(2, 0, 0)],
        };
        assert_eq!(transfer_slacks(&net, &plan)[0].slack, -30);
    }

    #[test]
    fn change_after_a_walk_counts_from_the_walk_end() {
        let net = net();
        let plan = Plan {
            legs: vec![ride(0, 0, 0), walk(1, 3, 200, 350), ride(3, 0, 0)],
        };
        assert_eq!(
            transfer_slacks(&net, &plan),
            vec![Change {
                leg: 2,
                station: 3,
                walked: true,
                slack: 400 - 350,
            }]
        );
    }

    #[test]
    fn walks_before_the_first_ride_and_staying_aboard_are_not_changes() {
        let net = net();
        let plan = Plan {
            legs: vec![walk(1, 3, 200, 350), ride(3, 0, 0)],
        };
        assert!(transfer_slacks(&net, &plan).is_empty());
        let plan = Plan {
            legs: vec![ride(4, 0, 0), ride(4, 1, 1)],
        };
        assert!(transfer_slacks(&net, &plan).is_empty());
    }
}
