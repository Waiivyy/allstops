//! Independent itinerary verifier.
//!
//! Shares nothing with the solver except feed loading: it reads the
//! itinerary JSON into its own types and re-derives every check from the raw
//! stop_times rows, the service calendar and the station clustering of
//! `allstops-gtfs`. It never imports `allstops-core` (a test enforces this).
//!
//! Every problem is reported as a [`Violation`] with a stable code and, where
//! it belongs to one leg, that leg's index.

use std::collections::{BTreeSet, HashMap};
use std::ops::RangeInclusive;

use allstops_gtfs::calendar::{ServiceCalendar, service_day_origin};
use allstops_gtfs::cluster::{Clustering, distance_m};
use allstops_gtfs::feed::{Feed, StopTime};
use allstops_gtfs::time::parse_time;
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

#[derive(Debug, Deserialize)]
pub struct MinTransfer {
    pub same_station: i64,
    pub walk_link: i64,
}

#[derive(Debug, Deserialize)]
pub struct RulesIn {
    pub earliest_start: String,
    pub latest_end: String,
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
    time: i64,
    /// Stop and trip of the last ride, if the previous leg was a ride.
    alight: Option<(u32, String)>,
    /// Stop of the last alighting, kept across a walk.
    last_alight_stop: Option<u32>,
    after_walk: bool,
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
    let r = &it.rules;
    if r.stay_aboard_through_terminus {
        push(
            "RULE_UNSUPPORTED",
            None,
            "stay_aboard_through_terminus is not supported".into(),
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
    let forbidden: BTreeSet<(u32, u32)> = feed
        .transfers
        .iter()
        .filter(|t| t.transfer_type == 3)
        .map(|t| (t.from_stop, t.to_stop))
        .collect();
    let min_transfer: HashMap<(u32, u32), i64> = feed
        .transfers
        .iter()
        .filter(|t| t.transfer_type == 2)
        .filter_map(|t| Some(((t.from_stop, t.to_stop), i64::from(t.min_transfer_time?))))
        .collect();

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
            } => {
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
                if !rows[bi].pickup_allowed() {
                    push(
                        "PICKUP_NOT_ALLOWED",
                        li_,
                        format!("no pickup at {board_stop_id}"),
                    );
                }
                if !rows[ai].drop_off_allowed() {
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
                    if start < p.time {
                        push(
                            "TIME_TRAVEL",
                            li_,
                            "ride departs before the previous leg ends".into(),
                        );
                    }
                    if let Some((prev_stop, prev_trip)) = &p.alight {
                        let continuing = *prev_trip == format!("{trip_id}@{service_date}");
                        let need = if continuing {
                            0
                        } else {
                            ctx_min(
                                &min_transfer,
                                *prev_stop,
                                rows[bi].stop,
                                r.min_transfer_s.same_station,
                            )
                        };
                        if start < p.time + need {
                            push(
                                "TRANSFER_TOO_SHORT",
                                li_,
                                format!("{}s to change, {need}s needed", start - p.time),
                            );
                        }
                    }
                    if let Some(prev_stop) = p.last_alight_stop
                        && forbidden.contains(&(prev_stop, rows[bi].stop))
                    {
                        push(
                            "FORBIDDEN_TRANSFER",
                            li_,
                            "transfers.txt forbids this change".into(),
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
                    time: end,
                    alight: Some((rows[ai].stop, format!("{trip_id}@{service_date}"))),
                    last_alight_stop: Some(rows[ai].stop),
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
                    if s < p.time {
                        push(
                            "TIME_TRAVEL",
                            li_,
                            "walk starts before the previous leg ends".into(),
                        );
                    }
                }
                let last_alight_stop = place.as_ref().and_then(|p| p.last_alight_stop);
                let (sa, sb) = (&ctx.clustering.stations[a], &ctx.clustering.stations[b]);
                let d = distance_m(sa.lat, sa.lon, sb.lat, sb.lon);
                if d > r.max_walk_m + 0.5 {
                    push(
                        "WALK_TOO_LONG",
                        li_,
                        format!("{d:.0} m is over the {} m limit", r.max_walk_m),
                    );
                }
                let speed = r.walking_speed_kmh / 3.6;
                let need = ((d * r.walk_detour_factor / speed).ceil() as i64)
                    .max(r.min_transfer_s.walk_link);
                if e - s < need {
                    push(
                        "WALK_TOO_FAST",
                        li_,
                        format!("{}s for {d:.0} m, at least {need}s needed", e - s),
                    );
                }
                place = Some(Place {
                    station: to_station.clone(),
                    time: e,
                    alight: None,
                    last_alight_stop,
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
                    if s < p.time {
                        push(
                            "TIME_TRAVEL",
                            li_,
                            "wait starts before the previous leg ends".into(),
                        );
                    }
                    // A wait keeps the place and arrival state; it only
                    // advances the clock for contiguity, not for transfers.
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

fn ctx_min(min_transfer: &HashMap<(u32, u32), i64>, from: u32, to: u32, default: i64) -> i64 {
    min_transfer
        .get(&(from, to))
        .copied()
        .unwrap_or(default)
        .max(default)
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
