//! RAPTOR against the Connection Scan engine: the earliest ready-to-board
//! label of every station must be identical. Hand-built cases for change
//! times, walks and overtaking trips, then property tests on random
//! networks.

use allstops_core::builder::test_support::{call, trip, with_stations};
use allstops_core::builder::{Call, NetworkBuilder, random};
use allstops_core::csa::{Csa, Origin};
use allstops_core::network::{INF, Network, StationIdx, Time};
use allstops_core::raptor::Raptor;
use proptest::prelude::*;

fn line(b: &mut NetworkBuilder, name: &str, stops: &[(u32, i32)]) {
    let calls: Vec<Call> = stops.iter().map(|&(s, t)| call(s, t, t)).collect();
    b.add_trip(trip(name, true), &calls);
}

fn csa_board(net: &Network, station: StationIdx, time: Time) -> Vec<Time> {
    let mut csa = Csa::new(net);
    csa.run(Origin::At { station, time }, None).board.clone()
}

#[test]
fn transfer_needs_change_time() {
    // S0 -> S1 arriving 100 with a 60 s change; B1 leaves S1 at 130 (too
    // soon), B2 at 200.
    let mut b = with_stations(3, 60);
    line(&mut b, "A", &[(0, 0), (1, 100)]);
    line(&mut b, "B1", &[(1, 130), (2, 230)]);
    line(&mut b, "B2", &[(1, 200), (2, 300)]);
    let net = b.build();
    let mut r = Raptor::new(&net);
    let got = r.earliest_board(0, 0).to_vec();
    assert_eq!(got[1], 160, "ready at S1 after the change time");
    assert_eq!(got[2], 360, "B2 arrives 300, plus the change time");
    assert_eq!(got, csa_board(&net, 0, 0));
}

#[test]
fn walks_start_at_the_arrival_and_never_chain() {
    // From S0: walk to S1 (50), but not on to S2 (a second walk). Ride B
    // from S1 to S3, arriving 900; walk to S4 from the arrival time (not
    // after the change time), but not on to S5.
    let mut b = with_stations(6, 60);
    b.add_footpath(0, 1, 50, 60.0);
    b.add_footpath(1, 2, 50, 60.0);
    b.add_footpath(3, 4, 120, 150.0);
    b.add_footpath(4, 5, 120, 150.0);
    line(&mut b, "B", &[(1, 100), (3, 900)]);
    let net = b.build();
    let mut r = Raptor::new(&net);
    let got = r.earliest_board(0, 0).to_vec();
    assert_eq!(got[1], 50);
    assert_eq!(got[2], INF, "S2 needs two walks in a row");
    assert_eq!(got[3], 960);
    assert_eq!(got[4], 1020, "the walk leaves at the arrival, 900");
    assert_eq!(got[5], INF, "S5 needs two walks in a row");
    assert_eq!(got, csa_board(&net, 0, 0));
}

#[test]
fn overtaking_trips_get_their_own_pattern() {
    // Same stops and flags, but the express leaving later arrives first.
    let mut b = with_stations(3, 60);
    line(&mut b, "local", &[(0, 0), (1, 500), (2, 1000)]);
    line(&mut b, "express", &[(0, 100), (1, 200), (2, 300)]);
    let net = b.build();
    let mut r = Raptor::new(&net);
    assert_eq!(r.patterns().groups(), 1);
    assert_eq!(r.patterns().len(), 2, "split so each pattern is FIFO");
    let got = r.earliest_board(0, 0).to_vec();
    assert_eq!(got[2], 360, "the express, not the earlier local");
    assert_eq!(got, csa_board(&net, 0, 0));
}

#[test]
fn departures_after_the_window_are_ignored() {
    let mut b = with_stations(3, 60);
    line(&mut b, "A", &[(0, 0), (1, 100), (2, 400)]);
    let mut net = b.build();
    net.window_end = 200;
    let mut r = Raptor::new(&net);
    let got = r.earliest_board(0, 0).to_vec();
    assert_eq!(got[1], 160, "arrivals past the window still count");
    assert_eq!(
        got[2], 460,
        "the hop leaving S1 at 100 is inside the window"
    );
    net.window_end = 50;
    let mut r = Raptor::new(&net);
    let got = r.earliest_board(0, 0).to_vec();
    assert_eq!(got[2], INF, "the hop leaving S1 at 100 is outside");
    assert_eq!(got, csa_board(&net, 0, 0));
}

#[test]
fn buffers_are_reset_between_queries() {
    let net = random::network(7, 8);
    let mut r = Raptor::new(&net);
    let n = net.stations.len() as StationIdx;
    for (s, t) in [(0, 0), (n - 1, 3000), (0, 0), (1 % n, 9000), (0, 600)] {
        assert_eq!(r.earliest_board(s, t), &csa_board(&net, s, t)[..]);
    }
}

// ---- Property tests --------------------------------------------------------

fn cases(default: u32) -> u32 {
    std::env::var("ALLSTOPS_PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Stations of a line (may repeat), base hop times, first departure,
/// headway, and (pickup, drop-off) per call.
type LineSpec = (Vec<u32>, Vec<i32>, i32, i32, Vec<(bool, bool)>);

/// Harder networks than `random::network`: stations repeat within a line,
/// trips of one line run at different speeds and overtake each other,
/// change times may be zero and walks go anywhere.
#[derive(Debug, Clone)]
struct Spec {
    stations: usize,
    change: i32,
    lines: Vec<LineSpec>,
    /// Per trip (four per line): slow-down factor of the hop times.
    speeds: Vec<i32>,
    walks: Vec<(u32, u32, i32)>,
}

fn spec() -> impl Strategy<Value = Spec> {
    (3usize..8, 0i32..180).prop_flat_map(|(n, change)| {
        let n32 = n as u32;
        let line = (
            proptest::collection::vec(0..n32, 2..7),
            proptest::collection::vec(30i32..400, 7),
            0i32..1800,
            60i32..900,
            proptest::collection::vec(
                (
                    proptest::bool::weighted(0.85),
                    proptest::bool::weighted(0.85),
                ),
                7,
            ),
        );
        (
            Just(n),
            Just(change),
            proptest::collection::vec(line, 1..5),
            proptest::collection::vec(1i32..4, 16),
            proptest::collection::vec((0..n32, 0..n32, 30i32..600), 0..8),
        )
            .prop_map(|(stations, change, lines, speeds, walks)| Spec {
                stations,
                change,
                lines,
                speeds,
                walks,
            })
    })
}

fn build(spec: &Spec) -> Network {
    let mut b = with_stations(spec.stations, spec.change);
    for (li, (stops, hops, first, headway, flags)) in spec.lines.iter().enumerate() {
        let mut seq = stops.clone();
        seq.dedup();
        if seq.len() < 2 {
            continue;
        }
        for k in 0..4 {
            let speed = spec.speeds[(li * 4 + k as usize) % spec.speeds.len()];
            let mut t = first + k * headway;
            let mut calls = Vec::new();
            for (i, &s) in seq.iter().enumerate() {
                let (pickup, drop_off) = flags[i];
                calls.push(Call {
                    stop: s,
                    station: s,
                    arr: t,
                    dep: t + 20,
                    pickup,
                    drop_off,
                    counts: true,
                });
                t += 20 + hops[i] * speed;
            }
            b.add_trip(trip(&format!("L{li}T{k}"), true), &calls);
        }
    }
    for &(a, c, d) in &spec.walks {
        if a != c {
            b.add_footpath(a, c, d, d as f32);
        }
    }
    b.build()
}

/// Mostly the network's own window; sometimes one that cuts service off.
fn window() -> impl Strategy<Value = Option<Time>> {
    prop_oneof![3 => Just(None), 1 => (0i32..12_000).prop_map(Some)]
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: cases(2000),
        .. ProptestConfig::default()
    })]

    #[test]
    fn raptor_matches_csa_on_random_networks(
        seed in any::<u64>(),
        station in 0u32..8,
        time in 0i32..12_000,
        window in window(),
    ) {
        let mut net = random::network(seed, 8);
        if let Some(w) = window {
            net.window_end = w;
        }
        net.validate().unwrap();
        let station = station % net.stations.len() as u32;
        let want = csa_board(&net, station, time);
        let mut r = Raptor::new(&net);
        prop_assert_eq!(r.earliest_board(station, time), &want[..]);
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: cases(1000),
        .. ProptestConfig::default()
    })]

    #[test]
    fn raptor_matches_csa_with_overtaking_and_loops(
        spec in spec(),
        station in 0u32..8,
        time in 0i32..6000,
        window in window(),
    ) {
        let mut net = build(&spec);
        if let Some(w) = window {
            net.window_end = w;
        }
        net.validate().unwrap();
        let station = station % net.stations.len() as u32;
        let want = csa_board(&net, station, time);
        let mut r = Raptor::new(&net);
        prop_assert_eq!(r.earliest_board(station, time), &want[..]);
        // A second query on the same buffers.
        let other = (station + 1) % net.stations.len() as u32;
        let want = csa_board(&net, other, time / 2);
        prop_assert_eq!(r.earliest_board(other, time / 2), &want[..]);
    }
}
