//! Itineraries written by the planner validate against the published JSON
//! Schema (docs/schema/itinerary-0.schema.json), and the schema rejects
//! documents that break it.

use allstops_core::builder::random;
use allstops_core::builder::test_support::{call, trip, with_stations};
use allstops_core::csa::{Csa, JLeg};
use allstops_core::itinerary::{FeedRef, to_itinerary};
use allstops_core::network::TripPart;
use allstops_core::plan::{Plan, greedy};
use allstops_core::rules::Rules;
use serde_json::{Value, json};

const SCHEMA: &str = include_str!("../../../docs/schema/itinerary-0.schema.json");

fn validator() -> jsonschema::Validator {
    let schema: Value = serde_json::from_str(SCHEMA).expect("schema is JSON");
    jsonschema::validator_for(&schema).expect("schema compiles")
}

fn feed() -> FeedRef {
    FeedRef {
        id: "synthetic".into(),
        sha256: "0".repeat(64),
        feed_version: "1".into(),
        attribution: "Timetable data: synthetic".into(),
    }
}

fn errors(v: &jsonschema::Validator, doc: &Value) -> Vec<String> {
    v.iter_errors(doc)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect()
}

#[test]
fn planned_itineraries_validate() {
    let v = validator();
    let mut checked = 0;
    for seed in 0..200 {
        let net = random::network(seed, 7);
        let mut csa = Csa::new(&net);
        let Some(&start) = net.targets.first() else {
            continue;
        };
        let Some(plan) = greedy(&mut csa, start, net.window_start) else {
            continue;
        };
        let Some(mut it) = to_itinerary(&net, &plan, &Rules::default(), feed(), "Europe/Berlin")
        else {
            continue;
        };
        it.lower_bound_s = Some(60);
        it.gap = Some(0.5);
        let doc = serde_json::to_value(&it).unwrap();
        assert_eq!(errors(&v, &doc), Vec::<String>::new(), "seed {seed}");
        checked += 1;
    }
    assert!(checked > 50, "only {checked} synthetic plans to check");
}

#[test]
fn a_stay_aboard_ride_validates() {
    let mut b = with_stations(3, 60);
    let mut t = trip("A", true);
    t.continues_as.push(TripPart {
        gtfs_id: "B".into(),
        route: "U2".into(),
        headsign: "Three".into(),
        route_type: 1,
        first_hop: 2,
    });
    let (mut a_end, mut b_start) = (call(1, 100, 100), call(1, 160, 160));
    a_end.pickup = false;
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
    let it = to_itinerary(&net, &plan, &Rules::default(), feed(), "Europe/Berlin").unwrap();
    let doc = serde_json::to_value(&it).unwrap();
    assert_eq!(doc["legs"][1]["stay_aboard"], json!(true));
    assert_eq!(errors(&validator(), &doc), Vec::<String>::new());
}

#[test]
fn the_schema_rejects_broken_documents() {
    let v = validator();
    let net = random::network(3, 7);
    let mut csa = Csa::new(&net);
    let plan = net
        .targets
        .iter()
        .find_map(|&s| greedy(&mut csa, s, net.window_start))
        .expect("a plan");
    let it = to_itinerary(&net, &plan, &Rules::default(), feed(), "Europe/Berlin").unwrap();
    let good = serde_json::to_value(&it).unwrap();
    assert!(v.is_valid(&good));
    let mut bad = Vec::new();
    let mut d = good.clone();
    d["schema"] = json!("allstops-itinerary/1");
    bad.push(("schema version", d));
    let mut d = good.clone();
    d["surprise"] = json!(1);
    bad.push(("unknown field", d));
    let mut d = good.clone();
    d["summary"]["last_visit"] = json!("8:00");
    bad.push(("time format", d));
    let mut d = good.clone();
    d["legs"] = json!([{"type": "teleport"}]);
    bad.push(("unknown leg", d));
    let mut d = good.clone();
    d["feed"]["sha256"] = json!("not a hash");
    bad.push(("feed hash", d));
    for (what, doc) in bad {
        assert!(!v.is_valid(&doc), "{what} should be rejected");
    }
}
