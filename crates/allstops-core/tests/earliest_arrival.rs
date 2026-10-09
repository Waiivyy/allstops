//! Section 10 test 4: earliest arrival. The Connection Scan engine against a
//! brute-force time-expanded Dijkstra oracle on random synthetic networks,
//! plus hand-built cases for transfer times and visit semantics.

use allstops_core::builder::test_support::{call, trip, with_stations};
use allstops_core::builder::{Call, NetworkBuilder};
use allstops_core::csa::{Csa, End, JLeg, Origin};
use allstops_core::network::{INF, Network, Time, flag};
use allstops_core::oracle::earliest_visits;
use allstops_core::plan::visits;
use allstops_core::profile::ProfileTo;
use proptest::prelude::*;

fn line(b: &mut NetworkBuilder, name: &str, stops: &[(u32, i32)], visits: bool) {
    let calls: Vec<Call> = stops.iter().map(|&(s, t)| call(s, t, t)).collect();
    b.add_trip(trip(name, visits), &calls);
}

#[test]
fn transfer_needs_change_time() {
    // S0 -> S1 arriving 100; connecting trip leaves S1 at 130 or 200.
    let mut b = with_stations(3, 60);
    line(&mut b, "A", &[(0, 0), (1, 100)], true);
    line(&mut b, "B1", &[(1, 130), (2, 230)], true);
    line(&mut b, "B2", &[(1, 200), (2, 300)], true);
    let net = b.build();
    let mut csa = Csa::new(&net);
    let l = csa.run(
        Origin::At {
            station: 0,
            time: 0,
        },
        None,
    );
    assert_eq!(l.visit[2], 300, "130 is too soon after arriving at 100");
}

#[test]
fn staying_aboard_needs_no_change_time() {
    let mut b = with_stations(3, 600);
    line(&mut b, "A", &[(0, 0), (1, 100), (2, 200)], true);
    let net = b.build();
    let mut csa = Csa::new(&net);
    let l = csa.run(
        Origin::Aboard {
            trip: 0,
            pos: 0,
            time: 100,
        },
        None,
    );
    assert_eq!(l.visit[2], 200);
}

#[test]
fn walking_does_not_visit_and_does_not_chain() {
    let mut b = with_stations(4, 60);
    b.add_footpath(0, 1, 50, 60.0);
    b.add_footpath(1, 2, 50, 60.0);
    line(&mut b, "A", &[(2, 500), (3, 600)], true);
    line(&mut b, "B", &[(1, 100), (3, 900)], true);
    let net = b.build();
    let mut csa = Csa::new(&net);
    let l = csa.run(
        Origin::At {
            station: 0,
            time: 0,
        },
        None,
    );
    assert_eq!(l.visit[1], 100, "visited by boarding B, not by walking in");
    assert_eq!(l.visit[2], INF, "S2 needs two walks in a row");
    assert_eq!(l.visit[3], 900);
}

#[test]
fn connector_trips_move_but_do_not_visit() {
    let mut b = with_stations(3, 60);
    line(&mut b, "bus", &[(0, 0), (1, 100), (2, 200)], false);
    line(&mut b, "metro", &[(2, 300), (1, 400)], true);
    let net = b.build();
    let mut csa = Csa::new(&net);
    let l = csa.run(
        Origin::At {
            station: 0,
            time: 0,
        },
        None,
    );
    assert_eq!(l.visit[1], 400, "the bus passing S1 at 100 does not count");
    assert_eq!(l.visit[2], 300, "boarding the metro at S2 counts");
}

#[test]
fn no_walk_straight_after_a_boarding_visit() {
    // The runner walked to S1 and is boarding the metro there. Walking on to
    // S2 now would chain two walks.
    let mut b = with_stations(3, 60);
    line(&mut b, "metro", &[(1, 100), (0, 200)], true);
    line(&mut b, "metro2", &[(2, 300), (0, 400)], true);
    b.add_footpath(1, 2, 50, 60.0);
    let net = b.build();
    let mut csa = Csa::new(&net);
    let l = csa.run(
        Origin::Boarding {
            trip: 0,
            pos: 0,
            time: 100,
        },
        None,
    );
    assert_eq!(
        l.visit[2], INF,
        "S2 is only reachable on foot from the boarding point"
    );
    assert_eq!(l.visit[0], 200);
}

#[test]
fn pickup_and_drop_off_rules_hold() {
    let mut b = with_stations(3, 1);
    let mut calls = vec![call(0, 0, 0), call(1, 100, 100), call(2, 200, 200)];
    calls[1].drop_off = false;
    b.add_trip(trip("A", true), &calls);
    line(&mut b, "B", &[(1, 150), (2, 160)], true);
    let net = b.build();
    let mut csa = Csa::new(&net);
    let l = csa.run(
        Origin::At {
            station: 0,
            time: 0,
        },
        None,
    );
    assert_eq!(
        l.visit[1], 100,
        "aboard through a stop counts even without alighting"
    );
    assert_eq!(l.visit[2], 200, "cannot alight at S1 to catch B");
}

#[test]
fn pass_through_does_not_count() {
    let mut b = with_stations(3, 1);
    let mut calls = vec![call(0, 0, 0), call(1, 100, 100), call(2, 200, 200)];
    calls[1].counts = false;
    b.add_trip(trip("A", true), &calls);
    let net = b.build();
    let mut csa = Csa::new(&net);
    assert_eq!(
        csa.run(
            Origin::At {
                station: 0,
                time: 0
            },
            None
        )
        .visit[1],
        INF
    );
}

// ---- Property test against the oracle -----------------------------------

/// Stations of a line, hop times, first departure, headway, whether it
/// visits, and (pickup, drop-off, counts) per call.
type LineSpec = (Vec<u32>, Vec<i32>, i32, i32, bool, Vec<(bool, bool, bool)>);

#[derive(Debug, Clone)]
struct Spec {
    stations: usize,
    change: i32,
    lines: Vec<LineSpec>,
    walks: Vec<(u32, u32, i32)>,
    origin: (u32, i32, u8, u32, u16),
}

fn spec() -> impl Strategy<Value = Spec> {
    (3usize..8, 1i32..180).prop_flat_map(|(n, change)| {
        let n32 = n as u32;
        let line = (
            proptest::collection::vec(0..n32, 2..6),
            // Hop times include 0: real feeds round to whole minutes, so
            // consecutive stops often share a time.
            proptest::collection::vec(prop_oneof![1 => Just(0i32), 3 => 30i32..400], 6),
            0i32..1800,
            120i32..900,
            proptest::bool::weighted(0.8),
            proptest::collection::vec(
                (
                    proptest::bool::weighted(0.9),
                    proptest::bool::weighted(0.9),
                    proptest::bool::weighted(0.9),
                ),
                6,
            ),
        );
        (
            Just(n),
            Just(change),
            proptest::collection::vec(line, 1..5),
            proptest::collection::vec((0..n32, 0..n32, 30i32..600), 0..6),
            (0..n32, 0i32..2400, 0u8..3, 0u32..64, 0u16..6),
        )
            .prop_map(|(stations, change, lines, walks, origin)| Spec {
                stations,
                change,
                lines,
                walks,
                origin,
            })
    })
}

fn build(spec: &Spec) -> Network {
    let mut b = with_stations(spec.stations, spec.change);
    for (li, (stops, hops, first, headway, visits, flags)) in spec.lines.iter().enumerate() {
        let mut seq = stops.clone();
        seq.dedup();
        if seq.len() < 2 {
            continue;
        }
        for k in 0..4 {
            let mut t = first + k * headway;
            let mut calls = Vec::new();
            for (i, &s) in seq.iter().enumerate() {
                let (pickup, drop_off, counts) = flags[i];
                calls.push(Call {
                    stop: s,
                    station: s,
                    arr: t,
                    dep: t + 20,
                    pickup,
                    drop_off,
                    counts,
                });
                t += 20 + hops[i];
            }
            b.add_trip(trip(&format!("L{li}T{k}"), *visits), &calls);
        }
    }
    for &(a, c, d) in &spec.walks {
        if a != c {
            b.add_footpath(a, c, d, d as f32);
        }
    }
    b.build()
}

fn origin(net: &Network, spec: &Spec) -> Origin {
    let (s, t, kind, trip, pos) = spec.origin;
    if net.trips.is_empty() || kind == 0 {
        return Origin::At {
            station: s,
            time: t,
        };
    }
    let trip = trip % net.trips.len() as u32;
    let hops = net.trip_connections(trip).len() as u16;
    let pos = pos % hops;
    let c = &net.connections[net.trip_connections(trip)[pos as usize] as usize];
    if kind == 1 {
        Origin::Aboard {
            trip,
            pos,
            time: c.arr,
        }
    } else {
        Origin::Boarding {
            trip,
            pos,
            time: c.dep,
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: std::env::var("ALLSTOPS_PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(512),
        .. ProptestConfig::default()
    })]

    #[test]
    fn csa_matches_oracle(spec in spec()) {
        let net = build(&spec);
        net.validate().unwrap();
        let o = origin(&net, &spec);
        let want = earliest_visits(&net, o);
        let mut csa = Csa::new(&net);
        let got = csa.run(o, None).visit.clone();
        prop_assert_eq!(&got, &want);

        // Every finite label has a journey that really visits the station
        // then, and journeys never move backwards in time.
        for s in 0..net.stations.len() as u32 {
            if got[s as usize] >= INF {
                continue;
            }
            let j = csa.journey_to_visit(s).expect("journey for finite label");
            let mut legs = j.legs.clone();
            if let End::Boarding { trip, pos, .. } = j.end {
                legs.push(JLeg::Ride { trip, from_pos: pos, to_pos: pos, continues_origin: false });
            }
            let v = visits(&net, &legs);
            let at_origin = o.place(&net).0 == s;
            let found = v.iter().any(|&(vs, vt)| vs == s && vt == got[s as usize]);
            prop_assert!(found || (at_origin && j.legs.is_empty()), "station {} label {} legs {:?}", s, got[s as usize], j.legs);
            if let Err(e) = check_journey(&net, o, &j.legs, j.end) {
                prop_assert!(false, "station {}: {} in {:?}", s, e, j.legs);
            }
        }
    }

    /// Profile queries: one backward scan per destination answers "earliest
    /// visit if ready at this station at time t" for every station and time,
    /// exactly as a forward scan from there would.
    #[test]
    fn profiles_match_repeated_queries(spec in spec(), times in proptest::collection::vec(-100i32..3000, 4)) {
        let net = build(&spec);
        let mut csa = Csa::new(&net);
        for dest in 0..net.stations.len() as u32 {
            let p = ProfileTo::new(&net, dest);
            for s in 0..net.stations.len() as u32 {
                let pairs = p.pairs(&net, s);
                prop_assert!(pairs.windows(2).all(|w| w[0].0 < w[1].0 && w[0].1 < w[1].1), "{:?}", pairs);
                for &t in &times {
                    let want = csa.run(Origin::At { station: s, time: t }, None).visit[dest as usize];
                    prop_assert_eq!(p.earliest_visit(&net, s, t), want, "from {} at {} to {}", s, t, dest);
                    let from_pairs = pairs.iter().find(|&&(r, _)| r >= t).map_or(INF, |&(_, v)| v);
                    prop_assert_eq!(from_pairs, want);
                }
            }
        }
    }
}

/// The journey rules, checked independently of the scan: every ride
/// boards where pickup is allowed and alights where drop-off is allowed;
/// every departure is no earlier than the previous arrival plus the
/// station's change time, or the end of a walk (whose duration already
/// includes the walk-link minimum); walks follow a footpath at its duration,
/// start no earlier than the arrival, and never follow a walk or a boarding
/// origin. A journey that ends aboard has not alighted from its last ride;
/// one that ends boarding must be able to board there.
fn check_journey(net: &Network, o: Origin, legs: &[JLeg], end: End) -> Result<(), String> {
    let (station, time) = o.place(net);
    // Where the runner is, when they got there, the earliest departure they
    // can catch there, and whether a walk is allowed next.
    let mut at = station;
    let mut arrived: Time = time;
    let mut ready: Time = match o {
        Origin::Aboard { .. } => time + net.change_time[station as usize],
        _ => time,
    };
    let mut may_walk = !matches!(o, Origin::Boarding { .. });
    for (i, leg) in legs.iter().enumerate() {
        match *leg {
            JLeg::Walk {
                from,
                to,
                start,
                end,
                ..
            } => {
                if !may_walk {
                    return Err("a walk after a walk or from a boarding origin".into());
                }
                if from != at || start < arrived {
                    return Err(format!(
                        "walk from {from} at {start}, runner at {at} since {arrived}"
                    ));
                }
                let f = net
                    .footpaths_from(from)
                    .iter()
                    .find(|f| f.to == to)
                    .ok_or_else(|| format!("no footpath {from} to {to}"))?;
                if end - start != f.duration {
                    return Err(format!(
                        "walk {from} to {to} takes {} not {}",
                        end - start,
                        f.duration
                    ));
                }
                (at, arrived, ready, may_walk) = (to, end, end, false);
            }
            JLeg::Ride {
                trip,
                from_pos,
                to_pos,
                continues_origin,
            } => {
                let cs = net.trip_connections(trip);
                let board = &net.connections[cs[from_pos as usize] as usize];
                let alight = &net.connections[cs[to_pos as usize] as usize];
                if board.dep_station != at {
                    return Err(format!("ride from {}, runner at {at}", board.dep_station));
                }
                if !(i == 0 && continues_origin) {
                    if !board.has(flag::PICKUP) {
                        return Err("boards where pickup is not allowed".into());
                    }
                    if board.dep < ready {
                        return Err(format!(
                            "departs {} before the runner can board at {ready}",
                            board.dep
                        ));
                    }
                }
                let stays_aboard = i + 1 == legs.len() && matches!(end, End::Aboard { .. });
                if !stays_aboard && !alight.has(flag::DROP_OFF) {
                    return Err("alights where drop-off is not allowed".into());
                }
                let a = alight.arr_station;
                (at, arrived, ready, may_walk) = (
                    a,
                    alight.arr,
                    alight.arr + net.change_time[a as usize],
                    true,
                );
            }
        }
    }
    if let End::Boarding { trip, pos, .. } = end
        && !legs.is_empty()
    {
        let c = &net.connections[net.trip_connections(trip)[pos as usize] as usize];
        if c.dep_station != at || !c.has(flag::PICKUP) || c.dep < ready {
            return Err(format!("cannot board at the end: at {at}, ready {ready}"));
        }
    }
    Ok(())
}
