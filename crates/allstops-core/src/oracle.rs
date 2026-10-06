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
use std::collections::BinaryHeap;

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
                    if n.has(flag::VISIT_ARR) {
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
                if c.has(flag::VISIT_ARR) {
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
                        if c.has(flag::VISIT_ARR) {
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
                        if n.has(flag::VISIT_ARR) {
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
