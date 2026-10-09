//! Staying aboard through a terminus (section 10 test 3): with the rule on,
//! a trip that the same vehicle continues as (by `block_id`, or by a
//! transfers.txt linked-trips row) joins it into one network trip; only
//! along a real continuation, and never with the rule off.

use allstops_core::network::{Network, flag};
use allstops_core::rules::Rules;
use allstops_gtfs::calendar::ServiceCalendar;
use allstops_gtfs::cluster::{ClusterConfig, cluster};
use allstops_gtfs::fixture::minimal_with;
use allstops_gtfs::network::build_network;
use allstops_gtfs::{Feed, Limits};

const STOPS: &str = "stop_id,stop_name,stop_lat,stop_lon,location_type,parent_station\n\
    S1,One,48.10,11.50,1,\nS1a,One,48.10,11.50,0,S1\n\
    S2,Two,48.11,11.50,1,\nS2a,Two,48.11,11.50,0,S2\nS2b,Two,48.11,11.50,0,S2\n\
    S3,Three,48.12,11.50,1,\nS3a,Three,48.12,11.50,0,S3\n";

/// A runs S1 to S2 (08:00 to 08:10), B runs S2 to S3 (08:20 to 08:30).
const STOP_TIMES: &str = "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
    A,08:00:00,08:00:00,S1a,1\nA,08:10:00,08:10:00,S2a,2\n\
    B,08:20:00,08:20:00,S2b,1\nB,08:30:00,08:30:00,S3a,2\n";

fn network(trips: &str, stop_times: &str, transfers: Option<&str>, stay: bool) -> Network {
    let mut files = vec![
        ("stops.txt", STOPS),
        ("trips.txt", trips),
        ("stop_times.txt", stop_times),
        (
            "calendar.txt",
            "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\n\
             WD,1,1,1,1,1,0,0,20261001,20261213\nWE,0,0,0,0,0,1,1,20261001,20261213\n",
        ),
    ];
    if let Some(t) = transfers {
        files.push(("transfers.txt", t));
    }
    let feed = Feed::from_zip_bytes(&minimal_with(&files), &Limits::default()).unwrap();
    let c = cluster(&feed, &ClusterConfig::default());
    let targets: Vec<u32> = ["S1a", "S2a", "S3a"]
        .iter()
        .map(|s| c.station_of_stop[feed.stop_index[*s] as usize])
        .collect();
    let rules = Rules {
        date: "2026-11-12".into(),
        stay_aboard_through_terminus: stay,
        ..Rules::default()
    };
    build_network(
        &feed,
        &ServiceCalendar::new(&feed),
        &c,
        &targets,
        &[1..=1],
        &rules,
        &Default::default(),
    )
    .unwrap()
    .0
}

const BLOCK: &str =
    "route_id,service_id,trip_id,trip_headsign,block_id\nR,WD,A,Two,X\nR,WD,B,Three,X\n";

fn trip_ids(net: &Network) -> Vec<Vec<String>> {
    net.trips
        .iter()
        .map(|t| {
            std::iter::once(t.gtfs_id.clone())
                .chain(t.continues_as.iter().map(|p| p.gtfs_id.clone()))
                .collect()
        })
        .collect()
}

#[test]
fn a_block_joins_only_when_the_rules_allow_it() {
    assert_eq!(
        trip_ids(&network(BLOCK, STOP_TIMES, None, false)),
        vec![vec!["A"], vec!["B"]]
    );
    let net = network(BLOCK, STOP_TIMES, None, true);
    assert_eq!(trip_ids(&net), vec![vec!["A", "B"]]);
    // Hop 0: S1 to S2 on A; hop 1: the terminus, A's last stop to B's
    // first; hop 2: S2 to S3 on B, which starts the second part.
    let t = &net.trips[0];
    assert_eq!(t.continues_as[0].first_hop, 2);
    assert_eq!(t.continues_as[0].headsign, "Three");
    let hops: Vec<_> = net
        .trip_connections(0)
        .iter()
        .map(|&c| net.connections[c as usize])
        .collect();
    assert_eq!(hops.len(), 3);
    let junction = hops[1];
    assert!(!junction.has(flag::PICKUP) && !junction.has(flag::DROP_OFF));
    assert_eq!((junction.dep, junction.arr), (hops[0].arr, hops[2].dep));
    assert!(
        hops[2].has(flag::PICKUP),
        "boarding B at its first stop still works"
    );
}

#[test]
fn only_a_real_continuation_joins() {
    // Different blocks.
    let other =
        "route_id,service_id,trip_id,trip_headsign,block_id\nR,WD,A,Two,X\nR,WD,B,Three,Y\n";
    assert_eq!(trip_ids(&network(other, STOP_TIMES, None, true)).len(), 2);
    // No block at all.
    let none = "route_id,service_id,trip_id,trip_headsign\nR,WD,A,Two\nR,WD,B,Three\n";
    assert_eq!(trip_ids(&network(none, STOP_TIMES, None, true)).len(), 2);
    // The same block on different service days is a different block.
    let days = "route_id,service_id,trip_id,trip_headsign,block_id\nR,WD,A,Two,X\nR,WE,B,Three,X\n";
    assert_eq!(
        trip_ids(&network(days, STOP_TIMES, None, true)),
        vec![vec!["A"]]
    );
    // B starts at another station.
    let elsewhere = STOP_TIMES.replace("B,08:20:00,08:20:00,S2b", "B,08:20:00,08:20:00,S1a");
    assert_eq!(trip_ids(&network(BLOCK, &elsewhere, None, true)).len(), 2);
    // B leaves before A arrives.
    let early = STOP_TIMES.replace("B,08:20:00,08:20:00,S2b", "B,08:05:00,08:05:00,S2b");
    assert_eq!(trip_ids(&network(BLOCK, &early, None, true)).len(), 2);
}

#[test]
fn linked_trips_rows_win_over_blocks() {
    let header =
        "from_stop_id,to_stop_id,from_trip_id,to_trip_id,transfer_type,min_transfer_time\n";
    // transfer_type 5: no in-seat transfer, whatever the block says.
    let veto = format!("{header},,A,B,5,\n");
    assert_eq!(
        trip_ids(&network(BLOCK, STOP_TIMES, Some(&veto), true)).len(),
        2
    );
    // transfer_type 4 links trips without a block.
    let none = "route_id,service_id,trip_id,trip_headsign\nR,WD,A,Two\nR,WD,B,Three\n";
    let link = format!("{header},,A,B,4,\n");
    assert_eq!(
        trip_ids(&network(none, STOP_TIMES, Some(&link), true)),
        vec![vec!["A", "B"]]
    );
}

#[test]
fn a_trip_continuing_as_two_trips_joins_neither() {
    // A splits into B and C (1-to-n): one network trip cannot hold both.
    let trips =
        "route_id,service_id,trip_id,trip_headsign\nR,WD,A,Two\nR,WD,B,Three\nR,WD,C,Three\n";
    let stop_times = format!("{STOP_TIMES}C,08:25:00,08:25:00,S2b,1\nC,08:35:00,08:35:00,S3a,2\n");
    let header =
        "from_stop_id,to_stop_id,from_trip_id,to_trip_id,transfer_type,min_transfer_time\n";
    let links = format!("{header},,A,B,4,\n,,A,C,4,\n");
    assert_eq!(
        trip_ids(&network(trips, &stop_times, Some(&links), true)).len(),
        3
    );
}

#[test]
fn a_one_stop_trip_breaks_the_chain() {
    // B has a single call: no hop to ride. A's vehicle runs B next, so it
    // does not continue as C directly, and nothing joins.
    let trips = "route_id,service_id,trip_id,trip_headsign,block_id\n\
                 R,WD,A,Two,X\nR,WD,B,Two,X\nR,WD,C,Three,X\n";
    let stop_times = "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
        A,08:00:00,08:00:00,S1a,1\nA,08:10:00,08:10:00,S2a,2\n\
        B,08:15:00,08:15:00,S2a,1\n\
        C,08:20:00,08:20:00,S2b,1\nC,08:30:00,08:30:00,S3a,2\n";
    assert_eq!(trip_ids(&network(trips, stop_times, None, true)).len(), 2);
}

#[test]
fn a_repeated_linked_trips_row_is_still_one_to_one() {
    let none = "route_id,service_id,trip_id,trip_headsign\nR,WD,A,Two\nR,WD,B,Three\n";
    let header =
        "from_stop_id,to_stop_id,from_trip_id,to_trip_id,transfer_type,min_transfer_time\n";
    let twice = format!("{header},,A,B,4,\n,,A,B,4,\n");
    assert_eq!(
        trip_ids(&network(none, STOP_TIMES, Some(&twice), true)),
        vec![vec!["A", "B"]]
    );
}
