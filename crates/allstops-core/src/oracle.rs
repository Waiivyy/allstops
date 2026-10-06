//! A slow, independent earliest-visit oracle for tests: Dijkstra over
//! explicit states of a time-expanded model. It shares the network data with
//! the Connection Scan engine but none of its logic.
//!
//! States:
//! - `Ready(s)`: may board any departure from station `s` from this time on;
//!   may not start a walk (walks are never chained).
//! - `Arrived(s)`: has just alighted at `s`; may start a walk now and board
//!   after the station's change time.
//! - `Free(s)`: at the origin; may walk or board from now.
//! - `OnHop(c)`: aboard, at the arrival of connection `c`.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use crate::csa::Origin;
use crate::network::{INF, Network, Time, flag};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum State {
    Ready(u32),
    Arrived(u32),
    Free(u32),
    OnHop(u32),
}

/// Earliest visit time of every station from `origin`.
pub fn earliest_visits(net: &Network, origin: Origin) -> Vec<Time> {
    let ns = net.stations.len();
    let mut by_dep: Vec<Vec<u32>> = vec![Vec::new(); ns];
    for (i, c) in net.connections.iter().enumerate() {
        if c.dep <= net.window_end {
            by_dep[c.dep_station as usize].push(i as u32);
        }
    }
    let mut visit = vec![INF; ns];
    let mut heap: BinaryHeap<Reverse<(Time, State)>> = BinaryHeap::new();
    let key = |s: State| -> usize {
        match s {
            State::Ready(x) => x as usize,
            State::Arrived(x) => ns + x as usize,
            State::Free(x) => 2 * ns + x as usize,
            State::OnHop(c) => 3 * ns + c as usize,
        }
    };
    let mut best = vec![INF; 3 * ns + net.connections.len()];
    let push =
        |heap: &mut BinaryHeap<Reverse<(Time, State)>>, best: &mut Vec<Time>, t: Time, s: State| {
            let k = key(s);
            if t < best[k] {
                best[k] = t;
                heap.push(Reverse((t, s)));
            }
        };

    match origin {
        Origin::At { station, time } => push(&mut heap, &mut best, time, State::Free(station)),
        Origin::Aboard { trip, pos, time } => {
            let ci = net.trip_connections(trip)[pos as usize];
            let c = net.connections[ci as usize];
            visit[c.arr_station as usize] = time;
            // Continue on the trip, and alight here if allowed.
            if let Some(&next) = net.trip_connections(trip).get(pos as usize + 1) {
                let n = net.connections[next as usize];
                if n.dep <= net.window_end {
                    push(&mut heap, &mut best, n.arr, State::OnHop(next));
                    if n.has(flag::VISIT_ARR) && n.arr <= net.window_end {
                        visit[n.arr_station as usize] = visit[n.arr_station as usize].min(n.arr);
                    }
                }
            }
            if c.has(flag::DROP_OFF) {
                push(&mut heap, &mut best, time, State::Arrived(c.arr_station));
            }
        }
        Origin::Boarding { trip, pos, time } => {
            let ci = net.trip_connections(trip)[pos as usize];
            let c = net.connections[ci as usize];
            visit[c.dep_station as usize] = time;
            if c.dep <= net.window_end {
                push(&mut heap, &mut best, c.arr, State::OnHop(ci));
                if c.has(flag::VISIT_ARR) && c.arr <= net.window_end {
                    visit[c.arr_station as usize] = visit[c.arr_station as usize].min(c.arr);
                }
            }
            push(&mut heap, &mut best, time, State::Free(c.dep_station));
        }
    }

    while let Some(Reverse((t, s))) = heap.pop() {
        if t > best[key(s)] {
            continue;
        }
        match s {
            State::Free(st) | State::Ready(st) => {
                if let State::Free(_) = s {
                    for f in net.footpaths_from(st) {
                        push(&mut heap, &mut best, t + f.duration, State::Ready(f.to));
                    }
                }
                for &ci in &by_dep[st as usize] {
                    let c = net.connections[ci as usize];
                    if c.dep >= t && c.has(flag::PICKUP) {
                        if c.has(flag::VISIT_DEP) {
                            visit[st as usize] = visit[st as usize].min(c.dep);
                        }
                        if c.has(flag::VISIT_ARR) && c.arr <= net.window_end {
                            visit[c.arr_station as usize] =
                                visit[c.arr_station as usize].min(c.arr);
                        }
                        push(&mut heap, &mut best, c.arr, State::OnHop(ci));
                    }
                }
            }
            State::Arrived(st) => {
                push(
                    &mut heap,
                    &mut best,
                    t + net.change_time[st as usize],
                    State::Ready(st),
                );
                for f in net.footpaths_from(st) {
                    push(&mut heap, &mut best, t + f.duration, State::Ready(f.to));
                }
            }
            State::OnHop(ci) => {
                let c = net.connections[ci as usize];
                if c.has(flag::DROP_OFF) {
                    push(&mut heap, &mut best, c.arr, State::Arrived(c.arr_station));
                }
                if let Some(&next) = net.trip_connections(c.trip).get(c.pos as usize + 1) {
                    let n = net.connections[next as usize];
                    if n.dep <= net.window_end {
                        if n.has(flag::VISIT_ARR) && n.arr <= net.window_end {
                            visit[n.arr_station as usize] =
                                visit[n.arr_station as usize].min(n.arr);
                        }
                        push(&mut heap, &mut best, n.arr, State::OnHop(next));
                    }
                }
            }
        }
    }
    visit
}

/// Exact minimum time from the first to the last target visit, by
/// exhaustive search over (state, set of visited targets). Exponential in
/// the number of targets; only for tiny test instances (at most 16 targets).
/// Returns `None` when no itinerary inside the window visits every target.
pub fn optimum(net: &Network) -> Option<Time> {
    let k = net.targets.len();
    assert!(k <= 16, "optimum() is for tiny instances");
    if k == 0 {
        return Some(0);
    }
    let mut bit = vec![0u32; net.stations.len()];
    for (i, &t) in net.targets.iter().enumerate() {
        bit[t as usize] = 1 << i;
    }
    let full: u32 = (1u32 << k) - 1;
    let ns = net.stations.len();
    let mut by_dep: Vec<Vec<u32>> = vec![Vec::new(); ns];
    for (i, c) in net.connections.iter().enumerate() {
        if c.dep <= net.window_end {
            by_dep[c.dep_station as usize].push(i as u32);
        }
    }
    let visit_dep = |c: &crate::network::Connection| -> u32 {
        if c.has(flag::VISIT_DEP) && c.dep >= net.window_start && c.dep <= net.window_end {
            bit[c.dep_station as usize]
        } else {
            0
        }
    };
    let visit_arr = |c: &crate::network::Connection| -> u32 {
        if c.has(flag::VISIT_ARR) && c.arr >= net.window_start && c.arr <= net.window_end {
            bit[c.arr_station as usize]
        } else {
            0
        }
    };

    // Start events: the first target visit, as (time, connection, mask),
    // where the runner is aboard `connection` arriving at its end.
    let mut starts: Vec<(Time, u32, u32)> = Vec::new();
    for (ci, c) in net.connections.iter().enumerate() {
        let vd = visit_dep(c);
        if vd != 0 && c.has(flag::PICKUP) {
            starts.push((c.dep, ci as u32, vd | visit_arr(c)));
        }
        let va = visit_arr(c);
        if va != 0 {
            // Boarded earlier on this trip without visiting a target there.
            let conns = net.trip_connections(c.trip);
            let boardable = conns[..=c.pos as usize].iter().any(|&e| {
                let e = &net.connections[e as usize];
                e.has(flag::PICKUP)
                    && visit_dep(e) == 0
                    && conns[e.pos as usize..c.pos as usize]
                        .iter()
                        .all(|&m| visit_arr(&net.connections[m as usize]) == 0)
            });
            if boardable {
                starts.push((c.arr, ci as u32, va));
            }
        }
    }

    let mut best_total: Option<Time> = None;
    for (t0, c0, m0) in starts {
        if m0 == full {
            // The first train completes the set: at once if the first
            // visit alone does, otherwise on arriving at the hop's end.
            let c = net.connections[c0 as usize];
            let total = if visit_dep(&c) == full || t0 == c.arr {
                0
            } else {
                c.arr - t0
            };
            best_total = Some(best_total.map_or(total, |b| b.min(total)));
            continue;
        }
        // States: 0..ns Ready(s), ns..2ns Arrived(s), 2ns.. OnHop(c); each
        // paired with the set of targets visited so far. Full sets are never
        // stored: reaching one records a completion time instead.
        let mut dist: HashMap<(u32, u32), Time> = HashMap::new();
        let mut heap: BinaryHeap<Reverse<(Time, u32, u32)>> = BinaryHeap::new();
        let push = |heap: &mut BinaryHeap<Reverse<(Time, u32, u32)>>,
                    dist: &mut HashMap<(u32, u32), Time>,
                    t: Time,
                    s: u32,
                    m: u32| {
            let e = dist.entry((s, m)).or_insert(INF);
            if t < *e {
                *e = t;
                heap.push(Reverse((t, s, m)));
            }
        };
        let on_hop = |ci: u32| (2 * ns) as u32 + ci;
        let c0c = net.connections[c0 as usize];
        push(&mut heap, &mut dist, c0c.arr, on_hop(c0), m0);
        let mut done = INF;
        while let Some(Reverse((t, s, m))) = heap.pop() {
            if t >= done || best_total.is_some_and(|b| t - t0 >= b) {
                break;
            }
            if dist.get(&(s, m)).copied().unwrap_or(INF) < t {
                continue;
            }
            let s = s as usize;
            if s < ns {
                for &ci in &by_dep[s] {
                    let c = net.connections[ci as usize];
                    if c.dep < t || !c.has(flag::PICKUP) {
                        continue;
                    }
                    let (vd, va) = (visit_dep(&c), visit_arr(&c));
                    if m | vd == full {
                        done = done.min(c.dep);
                    } else if m | vd | va == full {
                        done = done.min(c.arr);
                    } else {
                        push(&mut heap, &mut dist, c.arr, on_hop(ci), m | vd | va);
                    }
                }
            } else if s < 2 * ns {
                let st = (s - ns) as u32;
                push(
                    &mut heap,
                    &mut dist,
                    t + net.change_time[st as usize],
                    st,
                    m,
                );
                for f in net.footpaths_from(st) {
                    push(&mut heap, &mut dist, t + f.duration, f.to, m);
                }
            } else {
                let ci = (s - 2 * ns) as u32;
                let c = net.connections[ci as usize];
                if c.has(flag::DROP_OFF) {
                    push(&mut heap, &mut dist, c.arr, ns as u32 + c.arr_station, m);
                }
                if let Some(&next) = net.trip_connections(c.trip).get(c.pos as usize + 1) {
                    let n = net.connections[next as usize];
                    if n.dep <= net.window_end {
                        let nm = m | visit_arr(&n);
                        if nm == full {
                            done = done.min(n.arr);
                        } else {
                            push(&mut heap, &mut dist, n.arr, on_hop(next), nm);
                        }
                    }
                }
            }
        }
        if done < INF {
            let total = done - t0;
            best_total = Some(best_total.map_or(total, |b| b.min(total)));
        }
    }
    best_total
}
