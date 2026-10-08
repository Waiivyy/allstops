//! The loaded feed: every table this project uses, with string IDs interned
//! to dense indices. Field meanings follow the GTFS Schedule reference at
//! https://gtfs.org/documentation/schedule/reference/.

use std::collections::HashMap;
use std::ops::Range;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::archive::Archive;
use crate::error::{Error, Result};
use crate::limits::Limits;
use crate::table::Table;
use crate::time::{ServiceSeconds, parse_time};

pub type StopIdx = u32;
pub type RouteIdx = u32;
pub type TripIdx = u32;
pub type ServiceIdx = u32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedInfo {
    pub publisher_name: String,
    pub publisher_url: String,
    pub lang: String,
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agency {
    pub id: String,
    pub name: String,
    pub url: String,
    pub timezone: String,
}

/// `location_type` values from the reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum LocationType {
    Platform,
    Station,
    Entrance,
    GenericNode,
    BoardingArea,
}

impl LocationType {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "" | "0" => LocationType::Platform,
            "1" => LocationType::Station,
            "2" => LocationType::Entrance,
            "3" => LocationType::GenericNode,
            "4" => LocationType::BoardingArea,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stop {
    pub id: String,
    pub code: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub location_type: LocationType,
    pub parent: Option<StopIdx>,
    pub platform_code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub id: String,
    pub agency_id: String,
    pub short_name: String,
    pub long_name: String,
    pub route_type: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trip {
    pub id: String,
    pub route: RouteIdx,
    pub service: ServiceIdx,
    pub headsign: String,
    pub short_name: String,
    pub direction: Option<u8>,
    pub block_id: String,
    pub shape_id: String,
    /// Range into [`Feed::stop_times`], ordered by stop_sequence.
    pub stop_times: Range<u32>,
    /// Set for a run expanded from frequencies.txt: `Some(true)` when the
    /// service is schedule-based (`exact_times = 1`), `Some(false)` when it
    /// only keeps headways, so the departure times are approximate.
    pub frequency: Option<bool>,
    /// A trip with frequencies.txt rows. Its stop times are only the travel
    /// time pattern for its expanded runs; the template itself never runs.
    pub frequency_template: bool,
}

/// pickup_type / drop_off_type: 0 regular, 1 none, 2 phone agency,
/// 3 coordinate with driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopTime {
    pub trip: TripIdx,
    pub stop: StopIdx,
    pub sequence: u32,
    pub arrival: ServiceSeconds,
    pub departure: ServiceSeconds,
    pub pickup_type: u8,
    pub drop_off_type: u8,
}

impl StopTime {
    /// Both pickup and drop-off forbidden: the vehicle passes without a
    /// scheduled stop for passengers.
    pub fn is_pass_through(&self) -> bool {
        self.pickup_type == 1 && self.drop_off_type == 1
    }
    pub fn pickup_allowed(&self) -> bool {
        self.pickup_type != 1
    }
    pub fn drop_off_allowed(&self) -> bool {
        self.drop_off_type != 1
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Calendar {
    pub service: ServiceIdx,
    /// Monday first.
    pub weekdays: [bool; 7],
    pub start: NaiveDate,
    pub end: NaiveDate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Exception {
    Added,
    Removed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarDate {
    pub service: ServiceIdx,
    pub date: NaiveDate,
    pub exception: Exception,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frequency {
    pub trip: TripIdx,
    pub start: ServiceSeconds,
    pub end: ServiceSeconds,
    pub headway: i32,
    pub exact_times: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// A transfers.txt row. A stop may name a station (`location_type = 1`), in
/// which case the rule applies to all of its child stops. Route and trip
/// fields narrow the rule; see the specificity ranking in the GTFS reference.
pub struct Transfer {
    pub from_stop: Option<StopIdx>,
    pub to_stop: Option<StopIdx>,
    pub from_route: Option<RouteIdx>,
    pub to_route: Option<RouteIdx>,
    pub from_trip: Option<TripIdx>,
    pub to_trip: Option<TripIdx>,
    /// 0 recommended, 1 timed, 2 minimum time, 3 not possible, 4 in-seat
    /// allowed, 5 in-seat not allowed.
    pub transfer_type: u8,
    pub min_transfer_time: Option<i32>,
}

/// Problems that do not stop loading but that a user should see.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadWarnings {
    pub stop_times_unknown_trip: u64,
    pub stop_times_unknown_stop: u64,
    pub stop_times_filled_time: u64,
    pub stop_times_interpolated: u64,
    pub stop_times_missing_time: u64,
    pub trips_unknown_route: u64,
    pub trips_without_stop_times: u64,
    pub stops_unknown_parent: u64,
    pub calendar_dates_bad_exception: u64,
    pub frequencies_unknown_trip: u64,
    /// transfers.txt rows skipped: unknown stop, route or trip, an unknown
    /// transfer_type, or type 2 without a minimum time.
    pub transfers_skipped: u64,
}

/// A loaded feed. Its serde form (used by network packs) leaves out the stop
/// times, which packs encode separately, and the ID indexes, which are
/// rebuilt from the tables.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feed {
    /// Base names of every file in the archive.
    pub files: Vec<String>,
    /// Files present but holding no data rows.
    pub empty_files: Vec<String>,
    pub feed_info: Option<FeedInfo>,
    pub agencies: Vec<Agency>,
    pub stops: Vec<Stop>,
    pub routes: Vec<Route>,
    pub trips: Vec<Trip>,
    #[serde(skip)]
    pub stop_times: Vec<StopTime>,
    pub service_ids: Vec<String>,
    pub calendars: Vec<Calendar>,
    pub calendar_dates: Vec<CalendarDate>,
    pub frequencies: Vec<Frequency>,
    pub transfers: Vec<Transfer>,
    #[serde(skip)]
    pub stop_index: HashMap<String, StopIdx>,
    #[serde(skip)]
    pub trip_index: HashMap<String, TripIdx>,
    #[serde(skip)]
    pub route_index: HashMap<String, RouteIdx>,
    pub warnings: LoadWarnings,
}

pub const REQUIRED_FILES: [&str; 5] = [
    "agency.txt",
    "stops.txt",
    "routes.txt",
    "trips.txt",
    "stop_times.txt",
];

impl Feed {
    /// Load a GTFS zip from memory under the given limits.
    pub fn from_zip_bytes(bytes: &[u8], limits: &Limits) -> Result<Feed> {
        let mut ar = Archive::open(bytes, limits)?;
        for f in REQUIRED_FILES {
            if !ar.contains(f) {
                return Err(Error::MissingFile(f));
            }
        }
        if !ar.contains("calendar.txt") && !ar.contains("calendar_dates.txt") {
            return Err(Error::MissingFile("calendar.txt or calendar_dates.txt"));
        }
        let max_rows = limits.max_rows_per_file;
        let mut feed = Feed {
            files: ar.file_names().map(str::to_string).collect(),
            empty_files: Vec::new(),
            feed_info: None,
            agencies: Vec::new(),
            stops: Vec::new(),
            routes: Vec::new(),
            trips: Vec::new(),
            stop_times: Vec::new(),
            service_ids: Vec::new(),
            calendars: Vec::new(),
            calendar_dates: Vec::new(),
            frequencies: Vec::new(),
            transfers: Vec::new(),
            stop_index: HashMap::new(),
            trip_index: HashMap::new(),
            route_index: HashMap::new(),
            warnings: LoadWarnings::default(),
        };
        let mut services: HashMap<String, ServiceIdx> = HashMap::new();

        ar.with_reader("feed_info.txt", |r| {
            feed.feed_info = load_feed_info(&mut Table::new("feed_info.txt", r, max_rows)?)?;
            Ok(())
        })?;
        ar.with_reader("agency.txt", |r| {
            feed.agencies = load_agencies(&mut Table::new("agency.txt", r, max_rows)?)?;
            Ok(())
        })?;
        ar.with_reader("stops.txt", |r| {
            load_stops(&mut Table::new("stops.txt", r, max_rows)?, &mut feed)
        })?;
        ar.with_reader("routes.txt", |r| {
            load_routes(&mut Table::new("routes.txt", r, max_rows)?, &mut feed)
        })?;
        ar.with_reader("trips.txt", |r| {
            load_trips(
                &mut Table::new("trips.txt", r, max_rows)?,
                &mut feed,
                &mut services,
            )
        })?;
        ar.with_reader("calendar.txt", |r| {
            load_calendar(
                &mut Table::new("calendar.txt", r, max_rows)?,
                &mut feed,
                &mut services,
            )
        })?;
        ar.with_reader("calendar_dates.txt", |r| {
            load_calendar_dates(
                &mut Table::new("calendar_dates.txt", r, max_rows)?,
                &mut feed,
                &mut services,
            )
        })?;
        ar.with_reader("stop_times.txt", |r| {
            load_stop_times(&mut Table::new("stop_times.txt", r, max_rows)?, &mut feed)
        })?;
        ar.with_reader("frequencies.txt", |r| {
            load_frequencies(&mut Table::new("frequencies.txt", r, max_rows)?, &mut feed)
        })?;
        expand_frequencies(&mut feed, max_rows)?;
        ar.with_reader("transfers.txt", |r| {
            load_transfers(&mut Table::new("transfers.txt", r, max_rows)?, &mut feed)
        })?;

        let names = feed.files.clone();
        for name in names.iter().filter(|n| n.ends_with(".txt")) {
            let has_rows = ar.with_reader(name, |r| Table::new(name, r, max_rows)?.next_row())?;
            if has_rows == Some(false) {
                feed.empty_files.push(name.clone());
            }
        }

        let mut ids: Vec<(String, ServiceIdx)> = services.into_iter().collect();
        ids.sort_by_key(|(_, i)| *i);
        feed.service_ids = ids.into_iter().map(|(s, _)| s).collect();
        Ok(feed)
    }

    /// The feed time zone, from agency.txt. GTFS requires all agencies in a
    /// feed to share one.
    pub fn timezone(&self) -> Result<chrono_tz::Tz> {
        let tz = self
            .agencies
            .first()
            .map(|a| a.timezone.as_str())
            .ok_or(Error::MissingFile("agency.txt (no rows)"))?;
        tz.parse().map_err(|_| Error::TimeZone(tz.to_string()))
    }

    pub fn trip_stop_times(&self, trip: TripIdx) -> &[StopTime] {
        let r = &self.trips[trip as usize].stop_times;
        &self.stop_times[r.start as usize..r.end as usize]
    }

    /// The topmost ancestor of a stop through parent_station links.
    pub fn root_stop(&self, mut stop: StopIdx) -> StopIdx {
        for _ in 0..8 {
            match self.stops[stop as usize].parent {
                Some(p) if p != stop => stop = p,
                _ => break,
            }
        }
        stop
    }
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    if s.len() != 8 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    NaiveDate::parse_from_str(s, "%Y%m%d").ok()
}

fn load_feed_info(t: &mut Table) -> Result<Option<FeedInfo>> {
    let name = t.column("feed_publisher_name");
    let url = t.column("feed_publisher_url");
    let lang = t.column("feed_lang");
    let start = t.column("feed_start_date");
    let end = t.column("feed_end_date");
    let version = t.column("feed_version");
    if !t.next_row()? {
        return Ok(None);
    }
    let date = |t: &Table, c| -> Result<Option<NaiveDate>> {
        let s = t.get(c)?;
        if s.is_empty() {
            return Ok(None);
        }
        parse_date(s)
            .map(Some)
            .ok_or_else(|| t.error(format!("bad date {s:?}")))
    };
    Ok(Some(FeedInfo {
        publisher_name: t.get(name)?.to_string(),
        publisher_url: t.get(url)?.to_string(),
        lang: t.get(lang)?.to_string(),
        start_date: date(t, start)?,
        end_date: date(t, end)?,
        version: t.get(version)?.to_string(),
    }))
}

fn load_agencies(t: &mut Table) -> Result<Vec<Agency>> {
    let id = t.column("agency_id");
    let name = t.require("agency_name")?;
    let url = t.column("agency_url");
    let tz = t.require("agency_timezone")?;
    let mut out = Vec::new();
    while t.next_row()? {
        out.push(Agency {
            id: t.get(id)?.to_string(),
            name: t.get(Some(name))?.to_string(),
            url: t.get(url)?.to_string(),
            timezone: t.get(Some(tz))?.to_string(),
        });
    }
    Ok(out)
}

fn parse_coord(t: &Table, c: Option<usize>, what: &str, bound: f64) -> Result<f64> {
    let s = t.get(c)?;
    if s.is_empty() {
        return Ok(f64::NAN);
    }
    let v: f64 = s
        .parse()
        .map_err(|_| t.error(format!("bad {what} {s:?}")))?;
    if !v.is_finite() || v.abs() > bound {
        return Err(t.error(format!("{what} {v} out of range")));
    }
    Ok(v)
}

fn load_stops(t: &mut Table, feed: &mut Feed) -> Result<()> {
    let id = t.require("stop_id")?;
    let code = t.column("stop_code");
    let name = t.column("stop_name");
    let lat = t.column("stop_lat");
    let lon = t.column("stop_lon");
    let lt = t.column("location_type");
    let parent = t.column("parent_station");
    let platform = t.column("platform_code");
    let mut parents: Vec<String> = Vec::new();
    while t.next_row()? {
        let sid = t.get(Some(id))?;
        if sid.is_empty() {
            return Err(t.error("empty stop_id"));
        }
        let location_type =
            LocationType::parse(t.get(lt)?).ok_or_else(|| t.error("bad location_type"))?;
        let idx = feed.stops.len() as StopIdx;
        if feed.stop_index.insert(sid.to_string(), idx).is_some() {
            return Err(t.error(format!("duplicate stop_id {sid:?}")));
        }
        feed.stops.push(Stop {
            id: sid.to_string(),
            code: t.get(code)?.to_string(),
            name: t.get(name)?.to_string(),
            lat: parse_coord(t, lat, "stop_lat", 90.0)?,
            lon: parse_coord(t, lon, "stop_lon", 180.0)?,
            location_type,
            parent: None,
            platform_code: t.get(platform)?.to_string(),
        });
        parents.push(t.get(parent)?.to_string());
    }
    for (i, p) in parents.iter().enumerate() {
        if p.is_empty() {
            continue;
        }
        match feed.stop_index.get(p) {
            Some(&pi) => feed.stops[i].parent = Some(pi),
            None => feed.warnings.stops_unknown_parent += 1,
        }
    }
    Ok(())
}

fn load_routes(t: &mut Table, feed: &mut Feed) -> Result<()> {
    let id = t.require("route_id")?;
    let agency = t.column("agency_id");
    let short = t.column("route_short_name");
    let long = t.column("route_long_name");
    let rt = t.require("route_type")?;
    while t.next_row()? {
        let rid = t.get(Some(id))?;
        let route_type: u16 = t
            .get(Some(rt))?
            .parse()
            .map_err(|_| t.error("bad route_type"))?;
        let idx = feed.routes.len() as RouteIdx;
        if feed.route_index.insert(rid.to_string(), idx).is_some() {
            return Err(t.error(format!("duplicate route_id {rid:?}")));
        }
        feed.routes.push(Route {
            id: rid.to_string(),
            agency_id: t.get(agency)?.to_string(),
            short_name: t.get(short)?.to_string(),
            long_name: t.get(long)?.to_string(),
            route_type,
        });
    }
    Ok(())
}

fn intern(map: &mut HashMap<String, ServiceIdx>, s: &str) -> ServiceIdx {
    if let Some(&i) = map.get(s) {
        return i;
    }
    let i = map.len() as ServiceIdx;
    map.insert(s.to_string(), i);
    i
}

fn load_trips(
    t: &mut Table,
    feed: &mut Feed,
    services: &mut HashMap<String, ServiceIdx>,
) -> Result<()> {
    let route = t.require("route_id")?;
    let service = t.require("service_id")?;
    let id = t.require("trip_id")?;
    let headsign = t.column("trip_headsign");
    let short = t.column("trip_short_name");
    let dir = t.column("direction_id");
    let block = t.column("block_id");
    let shape = t.column("shape_id");
    while t.next_row()? {
        let Some(&r) = feed.route_index.get(t.get(Some(route))?) else {
            feed.warnings.trips_unknown_route += 1;
            continue;
        };
        let tid = t.get(Some(id))?;
        let idx = feed.trips.len() as TripIdx;
        if feed.trip_index.insert(tid.to_string(), idx).is_some() {
            return Err(t.error(format!("duplicate trip_id {tid:?}")));
        }
        let direction = match t.get(dir)? {
            "" => None,
            "0" => Some(0),
            "1" => Some(1),
            other => return Err(t.error(format!("bad direction_id {other:?}"))),
        };
        feed.trips.push(Trip {
            id: tid.to_string(),
            route: r,
            service: intern(services, t.get(Some(service))?),
            headsign: t.get(headsign)?.to_string(),
            short_name: t.get(short)?.to_string(),
            direction,
            block_id: t.get(block)?.to_string(),
            shape_id: t.get(shape)?.to_string(),
            stop_times: 0..0,
            frequency: None,
            frequency_template: false,
        });
    }
    Ok(())
}

fn load_calendar(
    t: &mut Table,
    feed: &mut Feed,
    services: &mut HashMap<String, ServiceIdx>,
) -> Result<()> {
    let id = t.require("service_id")?;
    let days = [
        t.require("monday")?,
        t.require("tuesday")?,
        t.require("wednesday")?,
        t.require("thursday")?,
        t.require("friday")?,
        t.require("saturday")?,
        t.require("sunday")?,
    ];
    let start = t.require("start_date")?;
    let end = t.require("end_date")?;
    while t.next_row()? {
        let mut weekdays = [false; 7];
        for (i, c) in days.iter().enumerate() {
            weekdays[i] = match t.get(Some(*c))? {
                "1" => true,
                "0" => false,
                other => return Err(t.error(format!("bad weekday flag {other:?}"))),
            };
        }
        let d = |c| -> Result<NaiveDate> {
            let s = t.get(Some(c))?;
            parse_date(s).ok_or_else(|| t.error(format!("bad date {s:?}")))
        };
        let (s, e) = (d(start)?, d(end)?);
        feed.calendars.push(Calendar {
            service: intern(services, t.get(Some(id))?),
            weekdays,
            start: s,
            end: e,
        });
    }
    Ok(())
}

fn load_calendar_dates(
    t: &mut Table,
    feed: &mut Feed,
    services: &mut HashMap<String, ServiceIdx>,
) -> Result<()> {
    let id = t.require("service_id")?;
    let date = t.require("date")?;
    let ex = t.require("exception_type")?;
    while t.next_row()? {
        let ds = t.get(Some(date))?;
        let d = parse_date(ds).ok_or_else(|| t.error(format!("bad date {ds:?}")))?;
        let exception = match t.get(Some(ex))? {
            "1" => Exception::Added,
            "2" => Exception::Removed,
            _ => {
                feed.warnings.calendar_dates_bad_exception += 1;
                continue;
            }
        };
        feed.calendar_dates.push(CalendarDate {
            service: intern(services, t.get(Some(id))?),
            date: d,
            exception,
        });
    }
    Ok(())
}

fn parse_flag(t: &Table, c: Option<usize>, what: &str) -> Result<u8> {
    match t.get(c)? {
        "" | "0" => Ok(0),
        "1" => Ok(1),
        "2" => Ok(2),
        "3" => Ok(3),
        other => Err(t.error(format!("bad {what} {other:?}"))),
    }
}

/// A stop_times row as read, before times are completed.
struct RawStopTime {
    st: StopTime,
    has_arrival: bool,
    has_departure: bool,
    dist: Option<f64>,
}

fn load_stop_times(t: &mut Table, feed: &mut Feed) -> Result<()> {
    let trip = t.require("trip_id")?;
    let arr = t.column("arrival_time");
    let dep = t.column("departure_time");
    let stop = t.require("stop_id")?;
    let seq = t.require("stop_sequence")?;
    let pickup = t.column("pickup_type");
    let drop = t.column("drop_off_type");
    let dist = t.column("shape_dist_traveled");

    let mut raw: Vec<RawStopTime> = Vec::new();
    // Trip IDs repeat on consecutive rows; remember the last lookup.
    let mut last: Option<(String, TripIdx)> = None;
    while t.next_row()? {
        let tid = t.get(Some(trip))?;
        let trip_idx = match &last {
            Some((s, i)) if s == tid => *i,
            _ => match feed.trip_index.get(tid) {
                Some(&i) => {
                    last = Some((tid.to_string(), i));
                    i
                }
                None => {
                    feed.warnings.stop_times_unknown_trip += 1;
                    continue;
                }
            },
        };
        let Some(&stop_idx) = feed.stop_index.get(t.get(Some(stop))?) else {
            feed.warnings.stop_times_unknown_stop += 1;
            continue;
        };
        let sequence: u32 = t
            .get(Some(seq))?
            .parse()
            .map_err(|_| t.error("bad stop_sequence"))?;
        let time = |c| -> Result<Option<ServiceSeconds>> {
            let s = t.get(c)?;
            if s.is_empty() {
                return Ok(None);
            }
            parse_time(s)
                .map(Some)
                .ok_or_else(|| t.error(format!("bad time {s:?}")))
        };
        let (a, d) = (time(arr)?, time(dep)?);
        let dist_v = match t.get(dist)? {
            "" => None,
            s => s.parse::<f64>().ok().filter(|v| v.is_finite()),
        };
        raw.push(RawStopTime {
            st: StopTime {
                trip: trip_idx,
                stop: stop_idx,
                sequence,
                arrival: a.or(d).unwrap_or(0),
                departure: d.or(a).unwrap_or(0),
                pickup_type: parse_flag(t, pickup, "pickup_type")?,
                drop_off_type: parse_flag(t, drop, "drop_off_type")?,
            },
            has_arrival: a.is_some(),
            has_departure: d.is_some(),
            dist: dist_v,
        });
    }

    raw.sort_by_key(|r| (r.st.trip, r.st.sequence));
    for w in raw.windows(2) {
        if w[0].st.trip == w[1].st.trip && w[0].st.sequence == w[1].st.sequence {
            return Err(Error::File {
                file: "stop_times.txt".into(),
                message: format!(
                    "trip {:?} repeats stop_sequence {}",
                    feed.trips[w[0].st.trip as usize].id, w[0].st.sequence
                ),
            });
        }
    }

    let mut start = 0;
    while start < raw.len() {
        let trip_idx = raw[start].st.trip;
        let mut end = start;
        while end < raw.len() && raw[end].st.trip == trip_idx {
            end += 1;
        }
        complete_times(&mut raw[start..end], &mut feed.warnings);
        feed.trips[trip_idx as usize].stop_times = start as u32..end as u32;
        start = end;
    }
    feed.warnings.trips_without_stop_times = feed
        .trips
        .iter()
        .filter(|t| t.stop_times.is_empty())
        .count() as u64;
    feed.stop_times = raw.into_iter().map(|r| r.st).collect();
    Ok(())
}

/// Fill in times GTFS allows to be omitted. A row with one of the two times
/// uses it for both. Rows with neither are interpolated linearly between the
/// surrounding timed rows, by shape_dist_traveled when every row involved has
/// it and by row position otherwise.
fn complete_times(rows: &mut [RawStopTime], w: &mut LoadWarnings) {
    for r in rows.iter() {
        if r.has_arrival != r.has_departure {
            w.stop_times_filled_time += 1;
        }
    }
    let timed: Vec<usize> = (0..rows.len())
        .filter(|&i| rows[i].has_arrival || rows[i].has_departure)
        .collect();
    for i in 0..rows.len() {
        if rows[i].has_arrival || rows[i].has_departure {
            continue;
        }
        let prev = timed.iter().rev().find(|&&j| j < i).copied();
        let next = timed.iter().find(|&&j| j > i).copied();
        let (Some(p), Some(n)) = (prev, next) else {
            w.stop_times_missing_time += 1;
            continue;
        };
        let t0 = rows[p].st.departure as f64;
        let t1 = rows[n].st.arrival as f64;
        let frac = match (rows[p].dist, rows[i].dist, rows[n].dist) {
            (Some(a), Some(b), Some(c)) if c > a => (b - a) / (c - a),
            _ => (i - p) as f64 / (n - p) as f64,
        };
        let v = (t0 + (t1 - t0) * frac.clamp(0.0, 1.0)).round() as ServiceSeconds;
        rows[i].st.arrival = v;
        rows[i].st.departure = v;
        w.stop_times_interpolated += 1;
    }
}

fn load_frequencies(t: &mut Table, feed: &mut Feed) -> Result<()> {
    let trip = t.require("trip_id")?;
    let start = t.require("start_time")?;
    let end = t.require("end_time")?;
    let headway = t.require("headway_secs")?;
    let exact = t.column("exact_times");
    while t.next_row()? {
        let Some(&ti) = feed.trip_index.get(t.get(Some(trip))?) else {
            feed.warnings.frequencies_unknown_trip += 1;
            continue;
        };
        let time = |c| -> Result<ServiceSeconds> {
            let s = t.get(Some(c))?;
            parse_time(s).ok_or_else(|| t.error(format!("bad time {s:?}")))
        };
        let h: i32 = t
            .get(Some(headway))?
            .parse()
            .map_err(|_| t.error("bad headway_secs"))?;
        if h <= 0 {
            return Err(t.error("headway_secs must be positive"));
        }
        let (s, e) = (time(start)?, time(end)?);
        if e <= s {
            return Err(t.error("end_time must be after start_time"));
        }
        let exact_times = match t.get(exact)? {
            "" | "0" => false,
            "1" => true,
            other => return Err(t.error(format!("bad exact_times {other:?}"))),
        };
        feed.frequencies.push(Frequency {
            trip: ti,
            start: s,
            end: e,
            headway: h,
            exact_times,
        });
    }
    Ok(())
}

/// Turn every trip with frequencies.txt rows into concrete runs: one per
/// departure `start_time + k * headway_secs` before `end_time`, shifted from
/// the template's first departure, named `<trip>@<HH:MM:SS>`. The template is
/// kept, flagged, and never run. The expanded stop times count against the
/// row limit like any other.
fn expand_frequencies(feed: &mut Feed, max_rows: u64) -> Result<()> {
    if feed.frequencies.is_empty() {
        return Ok(());
    }
    let mut by_trip: std::collections::BTreeMap<TripIdx, Vec<Frequency>> =
        std::collections::BTreeMap::new();
    for f in &feed.frequencies {
        by_trip.entry(f.trip).or_default().push(f.clone());
    }
    let file_err = |message: String| Error::File {
        file: "frequencies.txt".into(),
        message,
    };
    let mut rows = feed.stop_times.len() as u64;
    for (trip, mut freqs) in by_trip {
        freqs.sort_by_key(|f| f.start);
        if let Some(w) = freqs.windows(2).find(|w| w[1].start < w[0].end) {
            return Err(file_err(format!(
                "headways for trip {:?} overlap at {}",
                feed.trips[trip as usize].id,
                crate::time::format_time(w[1].start)
            )));
        }
        let pattern: Vec<StopTime> = feed.trip_stop_times(trip).to_vec();
        let Some(first) = pattern.first() else {
            continue;
        };
        let base = first.departure;
        feed.trips[trip as usize].frequency_template = true;
        for f in freqs {
            let mut start = f.start;
            while start < f.end {
                rows += pattern.len() as u64;
                if rows > max_rows {
                    return Err(Error::Limit(crate::error::LimitKind::RowCount {
                        file: "frequencies.txt (expanded stop times)".into(),
                        limit: max_rows,
                    }));
                }
                let shift = start - base;
                let new_trip = feed.trips.len() as TripIdx;
                let from = feed.stop_times.len() as u32;
                for (k, st) in pattern.iter().enumerate() {
                    let mut arrival = st.arrival + shift;
                    let departure = st.departure + shift;
                    // A first-stop arrival before the service day starts
                    // has no meaning; use the departure.
                    if k == 0 && arrival < 0 {
                        arrival = departure;
                    }
                    feed.stop_times.push(StopTime {
                        trip: new_trip,
                        arrival,
                        departure,
                        ..*st
                    });
                }
                let mut run = feed.trips[trip as usize].clone();
                run.id = format!("{}@{}", run.id, crate::time::format_time(start));
                run.frequency = Some(f.exact_times);
                run.frequency_template = false;
                run.stop_times = from..feed.stop_times.len() as u32;
                if feed.trip_index.insert(run.id.clone(), new_trip).is_some() {
                    return Err(file_err(format!(
                        "expanded trip ID {:?} clashes with an existing trip",
                        run.id
                    )));
                }
                feed.trips.push(run);
                start += f.headway;
            }
        }
    }
    Ok(())
}

fn load_transfers(t: &mut Table, feed: &mut Feed) -> Result<()> {
    let from = t.column("from_stop_id");
    let to = t.column("to_stop_id");
    let from_route = t.column("from_route_id");
    let to_route = t.column("to_route_id");
    let from_trip = t.column("from_trip_id");
    let to_trip = t.column("to_trip_id");
    let ty = t.require("transfer_type")?;
    let min = t.column("min_transfer_time");
    while t.next_row()? {
        // An empty field means "not set"; a set field must resolve.
        fn lookup<T: Copy>(
            map: &HashMap<String, T>,
            v: &str,
        ) -> std::result::Result<Option<T>, ()> {
            if v.is_empty() {
                Ok(None)
            } else {
                map.get(v).copied().map(Some).ok_or(())
            }
        }
        let refs = (
            lookup(&feed.stop_index, t.get(from)?),
            lookup(&feed.stop_index, t.get(to)?),
            lookup(&feed.route_index, t.get(from_route)?),
            lookup(&feed.route_index, t.get(to_route)?),
            lookup(&feed.trip_index, t.get(from_trip)?),
            lookup(&feed.trip_index, t.get(to_trip)?),
        );
        let (Ok(from_stop), Ok(to_stop), Ok(fr), Ok(tr), Ok(ft), Ok(tt)) = refs else {
            feed.warnings.transfers_skipped += 1;
            continue;
        };
        let transfer_type: u8 = match t.get(Some(ty))? {
            "" => 0,
            s => match s.parse() {
                Ok(v) if v <= 5 => v,
                _ => {
                    feed.warnings.transfers_skipped += 1;
                    continue;
                }
            },
        };
        let min_transfer_time = match t.get(min)? {
            "" => None,
            s => match s.parse::<i32>() {
                Ok(v) if v >= 0 => Some(v),
                _ => return Err(t.error(format!("bad min_transfer_time {s:?}"))),
            },
        };
        if transfer_type == 2 && min_transfer_time.is_none() {
            feed.warnings.transfers_skipped += 1;
            continue;
        }
        feed.transfers.push(Transfer {
            from_stop,
            to_stop,
            from_route: fr,
            to_route: tr,
            from_trip: ft,
            to_trip: tt,
            transfer_type,
            min_transfer_time,
        });
    }
    Ok(())
}
