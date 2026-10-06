//! Plans as sequences of legs, the visits they make, and the greedy
//! construction heuristic.

use crate::csa::{Csa, End, JLeg, Origin};
use crate::network::{INF, Network, StationIdx, Time, flag};

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
            return plan.duration(net).map(|_| plan);
        }
        let labels = csa.run(origin, Some(&unvisited));
        let mut best: Option<(Time, StationIdx)> = None;
        for (s, &u) in unvisited.iter().enumerate() {
            if u && labels.visit[s] < INF && best.is_none_or(|(t, _)| labels.visit[s] < t) {
                best = Some((labels.visit[s], s as StationIdx));
            }
        }
        let (_, target) = best?;
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
