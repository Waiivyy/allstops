//! Independent itinerary verifier.
//!
//! Shares nothing with the solver except feed loading: it reads the
//! itinerary JSON into its own types and re-derives every check from the raw
//! stop_times rows, the service calendar and the station clustering of
//! `allstops-gtfs`. It never imports `allstops-core` (a test enforces this).
//!
//! Every problem is reported as a [`Violation`] with a stable code and, where
//! it belongs to one leg, that leg's index.

use std::collections::HashMap;
use std::ops::RangeInclusive;

use allstops_gtfs::calendar::{ServiceCalendar, service_day_origin};
use allstops_gtfs::cluster::{Clustering, distance_m};
use allstops_gtfs::feed::{Feed, StopTime};
use allstops_gtfs::time::parse_time;
use allstops_gtfs::walks::WalkOverrides;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "allstops-itinerary/0";

// ---- Input model (independent of the solver's types) ---------------------

#[derive(Debug, Deserialize)]
pub struct Itinerary {
    pub schema: String,
    pub feed: FeedRef,
    pub timezone: String,
    pub date: String,
    pub rules: RulesIn,
    #[serde(default)]
    pub targets: Vec<String>,
    pub legs: Vec<Leg>,
    pub summary: SummaryIn,
}

#[derive(Debug, Deserialize)]
pub struct FeedRef {
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MinTransfer {
    pub same_station: i64,
    pub walk_link: i64,
}

fn any() -> String {
    "any".into()
}

/// The rules fields the verifier checks. Other fields of the itinerary's
/// rules are ignored.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RulesIn {
    pub earliest_start: String,
    pub latest_end: String,
    #[serde(default = "any")]
    pub start: String,
    #[serde(default = "any")]
    pub end: String,
    pub allow_walking: bool,
    pub walking_speed_kmh: f64,
    pub walk_detour_factor: f64,
    pub max_walk_m: f64,
    pub connector_modes: Vec<String>,
    pub min_transfer_s: MinTransfer,
    pub count_pass_through: bool,
    pub stay_aboard_through_terminus: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Leg {
    Ride {
        trip_id: String,
        service_date: String,
        board_stop_id: String,
        board_time: String,
        alight_stop_id: String,
        alight_time: String,
        #[serde(default)]
        stations: Vec<String>,
        /// Continues the previous ride without leaving the vehicle.
        #[serde(default)]
        stay_aboard: bool,
    },
    Walk {
        from_station: String,
        to_station: String,
        start: String,
        end: String,
    },
    Wait {
        station: String,
        start: String,
        end: String,
    },
}

#[derive(Debug, Deserialize)]
pub struct SummaryIn {
    pub duration_s: i64,
}

// ---- Output ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Violation {
    pub code: &'static str,
    pub leg: Option<usize>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub passed: bool,
    pub violations: Vec<Violation>,
    /// Recomputed time from first to last target visit, when every target
    /// was visited.
    pub duration_s: Option<i64>,
    pub stations_visited: usize,
}

impl Report {
    pub fn codes(&self) -> Vec<&'static str> {
        self.violations.iter().map(|v| v.code).collect()
    }
}

/// What the itinerary is checked against.
pub struct Context<'a> {
    pub feed: &'a Feed,
    pub calendar: &'a ServiceCalendar,
    pub clustering: &'a Clustering,
    /// Target station IDs, recomputed by the caller from the selection.
    pub targets: &'a [String],
    /// Route types whose trips count as visits.
    pub visit_types: &'a [RangeInclusive<u16>],
    /// SHA-256 of the feed bytes, when known.
    pub feed_sha256: Option<&'a str>,
    /// The rules the caller requires. When given, the itinerary's embedded
    /// rules must match them (RULES_MISMATCH otherwise) and these are the
    /// rules checked; when absent, the embedded rules are checked.
    pub expected_rules: Option<&'a RulesIn>,
    /// Measured and forbidden walks (`walks.toml`) the plan was made with.
    pub walks: &'a WalkOverrides,
}

/// GTFS route types for a connector mode name (basic and extended types).
/// Kept separate from the solver's table on purpose.
fn mode_types(mode: &str) -> Option<Vec<RangeInclusive<u16>>> {
    Some(match mode {
        "tram" => vec![0..=0, 900..=999],
        "subway" | "metro" => vec![1..=1, 400..=499],
        "rail" => vec![2..=2, 100..=199],
        "bus" => vec![3..=3, 11..=11, 200..=299, 700..=799],
        "ferry" => vec![4..=4, 1000..=1099, 1200..=1299],
        "cable_tram" => vec![5..=5],
        "aerial_lift" => vec![6..=6, 1300..=1399],
        "funicular" => vec![7..=7, 1400..=1499],
        _ => return None,
    })
}

fn in_any(t: u16, r: &[RangeInclusive<u16>]) -> bool {
    r.iter().any(|x| x.contains(&t))
}

fn clock(s: &str) -> Option<i64> {
    // HH:MM (rules) or HH:MM:SS (legs).
    let parts: Vec<&str> = s.split(':').collect();
    match parts.len() {
        2 => parse_time(&format!("{s}:00")).map(i64::from),
        3 => parse_time(s).map(i64::from),
        _ => None,
    }
}

/// Where the runner is after a leg.
#[derive(Clone)]
struct Place {
    station: String,
    /// Latest time accounted for, including waits; nothing may start earlier.
    /// Change times count from the last alighting instead.
    clock: i64,
    /// Stop, trip key (`trip@date`) and trip of the last ride, if the
    /// previous movement was a ride.
    alight: Option<(u32, String, u32)>,
    /// The last alighting (stop, trip, time), kept across a walk.
    last_alight: Option<(u32, u32, i64)>,
    after_walk: bool,
}

/// What transfers.txt says about one change between two trips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransferRule {
    /// No row applies, or the most specific one sets no constraint.
    Default,
    /// `transfer_type = 2`: at least this many seconds.
    AtLeast(i64),
    /// `transfer_type = 3`: not possible.
    Forbidden,
}

/// transfers.txt rows indexed by their (from, to) stop or station.
struct Transfers<'f> {
    feed: &'f Feed,
    by_pair: HashMap<(u32, u32), Vec<&'f allstops_gtfs::feed::Transfer>>,
}

impl<'f> Transfers<'f> {
    fn new(feed: &'f Feed) -> Self {
        let mut by_pair: HashMap<(u32, u32), Vec<&allstops_gtfs::feed::Transfer>> = HashMap::new();
        for t in &feed.transfers {
            // Rows without stops only describe in-seat transfers (types 4
            // and 5), which never apply to a change between two rides.
            if let (Some(f), Some(to)) = (t.from_stop, t.to_stop) {
                by_pair.entry((f, to)).or_default().push(t);
            }
        }
        Transfers { feed, by_pair }
    }

    /// The rule for changing from `from_trip` at `from_stop` to `to_trip` at
    /// `to_stop`. Rows may name the stop or its parent station. Among the
    /// rows that apply, the most specific wins, ranked as in the GTFS
    /// reference: both trips; a trip and a route; one trip; both routes;
    /// one route; stops only. A trip ID outranks a route ID on the same
    /// side. Between equally specific rows, one naming the stops themselves
    /// beats one naming their stations. A run expanded from frequencies.txt
    /// matches rows that name its template trip.
    fn rule(&self, from_stop: u32, from_trip: u32, to_stop: u32, to_trip: u32) -> TransferRule {
        let feed = self.feed;
        let (from_trip, to_trip) = (
            feed.trips[from_trip as usize].template.unwrap_or(from_trip),
            feed.trips[to_trip as usize].template.unwrap_or(to_trip),
        );
        let candidates = |s: u32| {
            let mut v = vec![(s, true)];
            if let Some(p) = feed.stops[s as usize].parent {
                v.push((p, false));
            }
            v
        };
        let from_route = feed.trips[from_trip as usize].route;
        let to_route = feed.trips[to_trip as usize].route;
        let mut best: Option<((u8, u8), &allstops_gtfs::feed::Transfer)> = None;
        for (fs, f_exact) in candidates(from_stop) {
            for (ts, t_exact) in candidates(to_stop) {
                for row in self.by_pair.get(&(fs, ts)).into_iter().flatten() {
                    let side = |trip: Option<u32>,
                                route: Option<u32>,
                                actual_trip: u32,
                                actual_route: u32| {
                        match (trip, route) {
                            (Some(t), _) if t != actual_trip => None,
                            (Some(_), _) => Some(2u8),
                            (None, Some(r)) if r != actual_route => None,
                            (None, Some(_)) => Some(1),
                            (None, None) => Some(0),
                        }
                    };
                    let (Some(a), Some(b)) = (
                        side(row.from_trip, row.from_route, from_trip, from_route),
                        side(row.to_trip, row.to_route, to_trip, to_route),
                    ) else {
                        continue;
                    };
                    // Ranks 1 (most specific) to 6, as in the reference.
                    let rank = match (a.max(b), a.min(b)) {
                        (2, 2) => 1,
                        (2, 1) => 2,
                        (2, 0) => 3,
                        (1, 1) => 4,
                        (1, 0) => 5,
                        _ => 6,
                    };
                    let exactness = 2 - (f_exact as u8 + t_exact as u8);
                    let key = (rank, exactness);
                    if best.is_none_or(|(k, _)| key < k) {
                        best = Some((key, row));
                    }
                }
            }
        }
        match best {
            Some((_, row)) if row.transfer_type == 3 => TransferRule::Forbidden,
            Some((_, row)) if row.transfer_type == 2 => {
                TransferRule::AtLeast(i64::from(row.min_transfer_time.unwrap_or(0)))
            }
            _ => TransferRule::Default,
        }
    }
}

/// Which trip each vehicle runs next, so a runner may stay aboard from a
/// trip's last stop into the next. From the GTFS reference: a linked-trips
/// row (`transfer_type = 4`) from a trip names the next trip; without one,
/// the next trip of its block (same `block_id`, running on the same service
/// date, by first departure) is, unless a `transfer_type = 5` row forbids
/// it. Only one-to-one continuations count; the next trip must start at the
/// station where the trip ends, no earlier than it arrives there; both need
/// at least two calls and must be of the same kind (counting as visits or
/// not). Worked out once per service date, on first use.
struct InSeat<'c> {
    ctx: &'c Context<'c>,
    linked: HashMap<u32, Vec<u32>>,
    vetoed: std::collections::HashSet<(u32, u32)>,
    blocks: HashMap<&'c str, Vec<u32>>,
    by_date: HashMap<NaiveDate, HashMap<u32, u32>>,
}

impl<'c> InSeat<'c> {
    fn new(ctx: &'c Context<'c>) -> Self {
        let feed = ctx.feed;
        let mut linked: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut vetoed = std::collections::HashSet::new();
        for t in &feed.transfers {
            if let (Some(a), Some(z)) = (t.from_trip, t.to_trip) {
                match t.transfer_type {
                    4 => linked.entry(a).or_default().push(z),
                    5 => {
                        vetoed.insert((a, z));
                    }
                    _ => {}
                }
            }
        }
        let mut blocks: HashMap<&str, Vec<u32>> = HashMap::new();
        for (i, trip) in feed.trips.iter().enumerate() {
            if !trip.block_id.is_empty() {
                blocks
                    .entry(trip.block_id.as_str())
                    .or_default()
                    .push(i as u32);
            }
        }
        InSeat {
            ctx,
            linked,
            vetoed,
            blocks,
            by_date: HashMap::new(),
        }
    }

    /// Whether the vehicle of trip `a` runs trip `z` next on `sd`.
    fn continues(&mut self, a: u32, z: u32, sd: NaiveDate) -> bool {
        if !self.by_date.contains_key(&sd) {
            let next = self.next_trips(sd);
            self.by_date.insert(sd, next);
        }
        self.by_date[&sd].get(&a) == Some(&z)
    }

    fn next_trips(&self, sd: NaiveDate) -> HashMap<u32, u32> {
        let (ctx, feed) = (self.ctx, self.ctx.feed);
        let running = |t: u32| {
            let trip = &feed.trips[t as usize];
            !trip.frequency_template
                && !feed.trip_stop_times(t).is_empty()
                && ctx.calendar.is_active(trip.service, sd)
        };
        let dep = |t: u32| feed.trip_stop_times(t)[0].departure;
        let mut block_next: HashMap<u32, u32> = HashMap::new();
        for trips in self.blocks.values() {
            let mut same: Vec<u32> = trips.iter().copied().filter(|&t| running(t)).collect();
            same.sort_by_key(|&t| (dep(t), t));
            for w in same.windows(2) {
                if dep(w[0]) != dep(w[1]) {
                    block_next.insert(w[0], w[1]);
                }
            }
        }
        let kind = |t: u32| {
            in_any(
                feed.routes[feed.trips[t as usize].route as usize].route_type,
                ctx.visit_types,
            )
        };
        let joins = |a: u32, z: u32| {
            let (ra, rz) = (feed.trip_stop_times(a), feed.trip_stop_times(z));
            if ra.len() < 2 || rz.len() < 2 || a == z {
                return false;
            }
            let (last, first) = (ra[ra.len() - 1], rz[0]);
            ctx.clustering.station_of_stop[last.stop as usize]
                == ctx.clustering.station_of_stop[first.stop as usize]
                && first.arrival >= last.departure
                && kind(a) == kind(z)
        };
        let mut next: HashMap<u32, u32> = HashMap::new();
        let mut preds: HashMap<u32, usize> = HashMap::new();
        for a in (0..feed.trips.len() as u32).filter(|&t| running(t)) {
            let mut to: Vec<u32> = self
                .linked
                .get(&a)
                .map(|to| to.iter().copied().filter(|&t| running(t)).collect())
                .unwrap_or_default();
            to.sort_unstable();
            to.dedup();
            let candidate = match to.as_slice() {
                // No linked trip runs: the block decides, unless vetoed.
                [] => block_next
                    .get(&a)
                    .copied()
                    .filter(|&z| !self.vetoed.contains(&(a, z))),
                [z] => Some(*z),
                _ => None,
            };
            if let Some(z) = candidate
                && joins(a, z)
            {
                next.insert(a, z);
                *preds.entry(z).or_default() += 1;
            }
        }
        next.retain(|_, z| preds[z] == 1);
        next
    }
}

pub fn verify(ctx: &Context, it: &Itinerary) -> Report {
    let mut v: Vec<Violation> = Vec::new();
    let mut push = |code: &'static str, leg: Option<usize>, message: String| {
        v.push(Violation { code, leg, message });
    };
    let feed = ctx.feed;

    if it.schema != SCHEMA {
        push(
            "SCHEMA",
            None,
            format!("expected schema {SCHEMA}, got {:?}", it.schema),
        );
    }
    if let Some(h) = ctx.feed_sha256
        && h != it.feed.sha256
    {
        push(
            "FEED_MISMATCH",
            None,
            format!(
                "itinerary was built from feed {}, checking against {h}",
                it.feed.sha256
            ),
        );
    }
    let tz: chrono_tz::Tz = match feed.timezone() {
        Ok(tz) => tz,
        Err(e) => {
            push("FEED_TIMEZONE", None, e.to_string());
            return finish(v, None, 0);
        }
    };
    if it.timezone != tz.name() {
        push(
            "TIMEZONE_MISMATCH",
            None,
            format!("itinerary says {}, feed says {}", it.timezone, tz.name()),
        );
    }
    let Ok(plan_date) = NaiveDate::parse_from_str(&it.date, "%Y-%m-%d") else {
        push("BAD_DATE", None, format!("bad plan date {:?}", it.date));
        return finish(v, None, 0);
    };
    let Some(plan_origin) = service_day_origin(&tz, plan_date) else {
        push(
            "BAD_DATE",
            None,
            format!(
                "plan date {plan_date} does not exist in time zone {}",
                tz.name()
            ),
        );
        return finish(v, None, 0);
    };
    let r: &RulesIn = match ctx.expected_rules {
        Some(expected) => {
            if *expected != it.rules {
                push(
                    "RULES_MISMATCH",
                    None,
                    "the itinerary was planned under different rules than the ones required".into(),
                );
            }
            expected
        }
        None => &it.rules,
    };
    // Rules that make the checks meaningless are themselves a violation.
    if !(r.walking_speed_kmh.is_finite() && r.walking_speed_kmh > 0.0) {
        push(
            "BAD_RULES",
            None,
            "walking_speed_kmh must be above 0".into(),
        );
    }
    if !(r.walk_detour_factor.is_finite() && r.walk_detour_factor >= 1.0) {
        push(
            "BAD_RULES",
            None,
            "walk_detour_factor must be at least 1".into(),
        );
    }
    if !(r.max_walk_m.is_finite() && r.max_walk_m >= 0.0) {
        push("BAD_RULES", None, "max_walk_m must be 0 or more".into());
    }
    if r.min_transfer_s.same_station < 1 || r.min_transfer_s.walk_link < 1 {
        push(
            "BAD_RULES",
            None,
            "minimum transfer times must be at least 1 second".into(),
        );
    }
    let mut allowed: Vec<RangeInclusive<u16>> = ctx.visit_types.to_vec();
    for m in &r.connector_modes {
        match mode_types(m) {
            Some(t) => allowed.extend(t),
            None => push("BAD_RULES", None, format!("unknown connector mode {m:?}")),
        }
    }
    let window = (clock(&r.earliest_start), clock(&r.latest_end));

    let station_by_id: HashMap<&str, usize> = ctx
        .clustering
        .stations
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();
    let station_id_of_stop = |stop: u32| -> String {
        let s = ctx.clustering.station_of_stop[stop as usize];
        ctx.clustering.stations[s as usize].id.clone()
    };
    let transfers = Transfers::new(feed);
    let mut in_seat = InSeat::new(ctx);

    // First visit time of each station, absolute.
    let mut visited: HashMap<String, i64> = HashMap::new();
    let mut visit = |station: String, t: i64| {
        let e = visited.entry(station).or_insert(t);
        if t < *e {
            *e = t;
        }
    };
    let mut place: Option<Place> = None;

    for (li, leg) in it.legs.iter().enumerate() {
        let li_ = Some(li);
        match leg {
            Leg::Ride {
                trip_id,
                service_date,
                board_stop_id,
                board_time,
                alight_stop_id,
                alight_time,
                stations,
                stay_aboard,
            } => {
                let stay_aboard = *stay_aboard;
                // The next ride stays aboard: this one does not alight.
                let next_stays = matches!(
                    it.legs.get(li + 1),
                    Some(Leg::Ride {
                        stay_aboard: true,
                        ..
                    })
                );
                let Some(&ti) = feed.trip_index.get(trip_id) else {
                    push(
                        "TRIP_UNKNOWN",
                        li_,
                        format!("no trip {trip_id:?} in the feed"),
                    );
                    place = None;
                    continue;
                };
                let trip = &feed.trips[ti as usize];
                if trip.frequency_template {
                    push(
                        "TRIP_NOT_RUN",
                        li_,
                        format!(
                            "{trip_id} only describes the pattern of its frequencies.txt runs ({trip_id}@HH:MM:SS)"
                        ),
                    );
                    place = None;
                    continue;
                }
                let route_type = feed.routes[trip.route as usize].route_type;
                let Ok(sd) = NaiveDate::parse_from_str(service_date, "%Y-%m-%d") else {
                    push(
                        "BAD_DATE",
                        li_,
                        format!("bad service date {service_date:?}"),
                    );
                    place = None;
                    continue;
                };
                if !ctx.calendar.is_active(trip.service, sd) {
                    push(
                        "SERVICE_NOT_RUNNING",
                        li_,
                        format!("trip {trip_id} does not run on {sd}"),
                    );
                }
                if !in_any(route_type, &allowed) {
                    push(
                        "MODE_NOT_ALLOWED",
                        li_,
                        format!("route_type {route_type} is not allowed by the rules"),
                    );
                }
                let visits_count = in_any(route_type, ctx.visit_types);
                let rows: &[StopTime] = feed.trip_stop_times(ti);
                let (Some(bt), Some(at)) = (clock(board_time), clock(alight_time)) else {
                    push("BAD_TIME", li_, "unreadable board or alight time".into());
                    place = None;
                    continue;
                };
                let board = rows.iter().position(|s| {
                    feed.stops[s.stop as usize].id == *board_stop_id && i64::from(s.departure) == bt
                });
                let Some(bi) = board else {
                    push(
                        "BOARD_STOP_NOT_ON_TRIP",
                        li_,
                        format!("trip {trip_id} does not depart {board_stop_id} at {board_time}"),
                    );
                    place = None;
                    continue;
                };
                let alight = rows
                    .iter()
                    .enumerate()
                    .skip(bi + 1)
                    .find(|(_, s)| {
                        feed.stops[s.stop as usize].id == *alight_stop_id
                            && i64::from(s.arrival) == at
                    })
                    .map(|(i, _)| i);
                let Some(ai) = alight else {
                    push(
                        "ALIGHT_STOP_NOT_ON_TRIP",
                        li_,
                        format!(
                            "trip {trip_id} does not reach {alight_stop_id} at {alight_time} after boarding"
                        ),
                    );
                    place = None;
                    continue;
                };
                if !stay_aboard && !rows[bi].pickup_allowed() {
                    push(
                        "PICKUP_NOT_ALLOWED",
                        li_,
                        format!("no pickup at {board_stop_id}"),
                    );
                }
                if !next_stays && !rows[ai].drop_off_allowed() {
                    push(
                        "DROP_OFF_NOT_ALLOWED",
                        li_,
                        format!("no drop-off at {alight_stop_id}"),
                    );
                }
                let Some(origin) = service_day_origin(&tz, sd) else {
                    push(
                        "BAD_DATE",
                        li_,
                        format!(
                            "service date {sd} does not exist in time zone {}",
                            tz.name()
                        ),
                    );
                    place = None;
                    continue;
                };
                let start = origin + i64::from(rows[bi].departure);
                let end = origin + i64::from(rows[ai].arrival);
                let board_station = station_id_of_stop(rows[bi].stop);

                // Joins with the previous leg.
                if let Some(p) = &place {
                    if p.station != board_station {
                        push(
                            "NOT_CONTIGUOUS",
                            li_,
                            format!(
                                "previous leg ends at {}, this ride starts at {board_station}",
                                p.station
                            ),
                        );
                    }
                    if start < p.clock {
                        push(
                            "TIME_TRAVEL",
                            li_,
                            "ride departs before the previous leg ends".into(),
                        );
                    }
                    let continuing = stay_aboard
                        || p.alight
                            .as_ref()
                            .is_some_and(|(_, key, _)| *key == format!("{trip_id}@{service_date}"));
                    if let Some((prev_stop, prev_trip, alighted)) = p.last_alight
                        && !continuing
                    {
                        let rule = transfers.rule(prev_stop, prev_trip, rows[bi].stop, ti);
                        if rule == TransferRule::Forbidden {
                            push(
                                "FORBIDDEN_TRANSFER",
                                li_,
                                "transfers.txt forbids this change".into(),
                            );
                        }
                        // Directly after a ride the runner's own change time
                        // applies too; after a walk the walk already took
                        // its own time.
                        let own = if p.alight.is_some() {
                            r.min_transfer_s.same_station
                        } else {
                            0
                        };
                        let need = match rule {
                            TransferRule::AtLeast(m) => m.max(own),
                            _ => own,
                        };
                        if start < alighted + need {
                            push(
                                "TRANSFER_TOO_SHORT",
                                li_,
                                format!("{}s to change, {need}s needed", start - alighted),
                            );
                        }
                    }
                }

                if stay_aboard {
                    if !r.stay_aboard_through_terminus {
                        push(
                            "STAY_ABOARD_NOT_ALLOWED",
                            li_,
                            "the rules do not allow staying aboard through a terminus".into(),
                        );
                    }
                    // The previous ride ended at its trip's last stop on the
                    // same service date, this one starts at its trip's first
                    // stop, and the vehicle runs this trip next.
                    let from_terminus = place.as_ref().and_then(|p| {
                        let (stop, key, prev) = p.alight.as_ref()?;
                        let prev_rows = feed.trip_stop_times(*prev);
                        let last = prev_rows.last()?;
                        (*stop == last.stop
                            && key.ends_with(&format!("@{service_date}"))
                            && p.clock == origin + i64::from(last.arrival))
                        .then_some(*prev)
                    });
                    let ok = bi == 0
                        && from_terminus.is_some_and(|prev| in_seat.continues(prev, ti, sd));
                    if !ok {
                        push(
                            "NOT_A_CONTINUATION",
                            li_,
                            format!(
                                "trip {trip_id} does not continue the previous ride's trip at its terminus"
                            ),
                        );
                    }
                }

                let mut recomputed = Vec::new();
                for (k, s) in rows.iter().enumerate().take(ai + 1).skip(bi) {
                    let counts = visits_count && (!s.is_pass_through() || r.count_pass_through);
                    if !counts {
                        continue;
                    }
                    let t = if k == bi { s.departure } else { s.arrival };
                    let sid = station_id_of_stop(s.stop);
                    visit(sid.clone(), origin + i64::from(t));
                    if recomputed.last() != Some(&sid) {
                        recomputed.push(sid);
                    }
                }
                if !stations.is_empty() {
                    let mut claimed: Vec<&String> = stations.iter().collect();
                    let mut actual: Vec<&String> = recomputed.iter().collect();
                    claimed.dedup();
                    actual.dedup();
                    if claimed != actual {
                        push(
                            "STATIONS_MISMATCH",
                            li_,
                            format!(
                                "leg lists {} stations, the trip visits {}",
                                claimed.len(),
                                actual.len()
                            ),
                        );
                    }
                }
                place = Some(Place {
                    station: station_id_of_stop(rows[ai].stop),
                    clock: end,
                    alight: Some((rows[ai].stop, format!("{trip_id}@{service_date}"), ti)),
                    last_alight: Some((rows[ai].stop, ti, end)),
                    after_walk: false,
                });
            }
            Leg::Walk {
                from_station,
                to_station,
                start,
                end,
            } => {
                if !r.allow_walking {
                    push(
                        "WALK_NOT_ALLOWED",
                        li_,
                        "the rules do not allow walking".into(),
                    );
                }
                let (Some(s), Some(e)) = (clock(start), clock(end)) else {
                    push("BAD_TIME", li_, "unreadable walk times".into());
                    place = None;
                    continue;
                };
                let (s, e) = (plan_origin + s, plan_origin + e);
                let (Some(&a), Some(&b)) = (
                    station_by_id.get(from_station.as_str()),
                    station_by_id.get(to_station.as_str()),
                ) else {
                    push(
                        "UNKNOWN_STATION",
                        li_,
                        format!("{from_station} or {to_station} is not a station"),
                    );
                    place = None;
                    continue;
                };
                if a == b {
                    push(
                        "WALK_SAME_STATION",
                        li_,
                        format!("a walk from {from_station} to itself"),
                    );
                }
                if let Some(p) = &place {
                    if p.after_walk {
                        push("WALK_CHAINED", li_, "two walks in a row".into());
                    }
                    if p.station != *from_station {
                        push(
                            "NOT_CONTIGUOUS",
                            li_,
                            format!(
                                "previous leg ends at {}, walk starts at {from_station}",
                                p.station
                            ),
                        );
                    }
                    if s < p.clock {
                        push(
                            "TIME_TRAVEL",
                            li_,
                            "walk starts before the previous leg ends".into(),
                        );
                    }
                }
                let last_alight = place.as_ref().and_then(|p| p.last_alight);
                let (sa, sb) = (&ctx.clustering.stations[a], &ctx.clustering.stations[b]);
                let d = distance_m(sa.lat, sa.lon, sb.lat, sb.lon);
                if !d.is_finite() {
                    push(
                        "WALK_UNMEASURABLE",
                        li_,
                        format!("{from_station} or {to_station} has no coordinates"),
                    );
                } else if d > r.max_walk_m + 0.5 {
                    push(
                        "WALK_TOO_LONG",
                        li_,
                        format!("{d:.0} m is over the {} m limit", r.max_walk_m),
                    );
                }
                // walks.toml: the last entry covering this direction applies.
                let measured = ctx.walks.walk.iter().rev().find(|w| {
                    (w.from == *from_station && w.to == *to_station)
                        || (w.both_ways && w.from == *to_station && w.to == *from_station)
                });
                if measured.is_some_and(|w| w.forbid) {
                    push(
                        "WALK_FORBIDDEN",
                        li_,
                        format!("walks.toml forbids walking from {from_station} to {to_station}"),
                    );
                }
                let speed = r.walking_speed_kmh / 3.6;
                let need = match measured.and_then(|w| w.seconds) {
                    Some(m) => i64::from(m),
                    None => (d * r.walk_detour_factor / speed).ceil() as i64,
                }
                .max(r.min_transfer_s.walk_link);
                if d.is_finite() && e - s < need {
                    push(
                        "WALK_TOO_FAST",
                        li_,
                        format!("{}s for {d:.0} m, at least {need}s needed", e - s),
                    );
                }
                place = Some(Place {
                    station: to_station.clone(),
                    clock: e,
                    alight: None,
                    last_alight,
                    after_walk: true,
                });
            }
            Leg::Wait {
                station,
                start,
                end,
            } => {
                let (Some(s), Some(e)) = (clock(start), clock(end)) else {
                    push("BAD_TIME", li_, "unreadable wait times".into());
                    continue;
                };
                let (s, e) = (plan_origin + s, plan_origin + e);
                if e < s {
                    push("TIME_TRAVEL", li_, "wait ends before it starts".into());
                }
                if let Some(p) = &mut place {
                    if p.station != *station {
                        push(
                            "NOT_CONTIGUOUS",
                            li_,
                            format!("waiting at {station} but the runner is at {}", p.station),
                        );
                    }
                    if s < p.clock {
                        push(
                            "TIME_TRAVEL",
                            li_,
                            "wait starts before the previous leg ends".into(),
                        );
                    }
                    // A wait keeps the place and the arrival state used for
                    // change times, and moves the clock on: nothing after
                    // it may start before it ends.
                    p.clock = p.clock.max(e);
                }
            }
        }
    }

    // Targets, window and summary.
    let mut missing = Vec::new();
    for t in ctx.targets {
        if !visited.contains_key(t) {
            missing.push(t.clone());
            push(
                "MISSING_TARGET",
                None,
                format!("target {t} is never visited"),
            );
        }
    }
    let mut it_targets = it.targets.clone();
    let mut want = ctx.targets.to_vec();
    it_targets.sort();
    want.sort();
    if !it.targets.is_empty() && it_targets != want {
        push(
            "TARGETS_MISMATCH",
            None,
            "the itinerary's target list differs from the selection".into(),
        );
    }
    let target_times: Vec<i64> = ctx
        .targets
        .iter()
        .filter_map(|t| visited.get(t).copied())
        .collect();
    let duration = if missing.is_empty() && !target_times.is_empty() {
        let first = *target_times.iter().min().unwrap_or(&0);
        let last = *target_times.iter().max().unwrap_or(&0);
        if let (Some(ws), Some(we)) = window {
            if first < plan_origin + ws {
                push(
                    "OUTSIDE_WINDOW",
                    None,
                    "the first visit is before earliest_start".into(),
                );
            }
            if last > plan_origin + we {
                push(
                    "OUTSIDE_WINDOW",
                    None,
                    "the last visit is after latest_end".into(),
                );
            }
        } else {
            push(
                "BAD_RULES",
                None,
                "unreadable earliest_start or latest_end".into(),
            );
        }
        let earliest: Vec<&String> = ctx
            .targets
            .iter()
            .filter(|t| visited.get(*t) == Some(&first))
            .collect();
        if r.start != "any" && !earliest.iter().any(|t| **t == r.start) {
            push(
                "START_MISMATCH",
                None,
                format!("the first visit is not at the start station {}", r.start),
            );
        }
        let latest: Vec<&String> = ctx
            .targets
            .iter()
            .filter(|t| visited.get(*t) == Some(&last))
            .collect();
        if r.end != "any" && !latest.iter().any(|t| **t == r.end) {
            push(
                "END_MISMATCH",
                None,
                format!("the last visit is not at the end station {}", r.end),
            );
        }
        let d = last - first;
        if d != it.summary.duration_s {
            push(
                "SUMMARY_MISMATCH",
                None,
                format!(
                    "summary says {}s, the legs give {d}s",
                    it.summary.duration_s
                ),
            );
        }
        Some(d)
    } else {
        None
    };
    finish(v, duration, visited.len())
}

fn finish(violations: Vec<Violation>, duration_s: Option<i64>, stations_visited: usize) -> Report {
    Report {
        passed: violations.is_empty(),
        violations,
        duration_s,
        stations_visited,
    }
}

/// Parse an itinerary JSON document.
pub fn parse(json: &str) -> Result<Itinerary, serde_json::Error> {
    serde_json::from_str(json)
}
