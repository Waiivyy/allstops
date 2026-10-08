//! Network packs: everything plans for one selection can use, in one
//! versioned file that the same input always turns into the same bytes.
//!
//! A pack holds a subset of the feed (only the routes of the target and
//! connector modes, their trips, stop times, services, stops and
//! transfers), the stations after clustering and overrides, the targets and
//! the walk overrides. The subset is cut from the GTFS rows and loaded by
//! the normal loader, then stored as the loaded tables, so reading a pack
//! gives exactly the [`Feed`] that loading the subset's rows would. Networks
//! for any date, and for any rules that keep the pack's connector modes, are
//! built from it exactly as from the full feed.
//!
//! Stop times take most of the space, so they are stored as deduplicated
//! stop patterns (stops, sequence numbers, pickup and drop-off types) and
//! timing patterns (times relative to the first departure), with one
//! (pattern, timing, first departure) triple per trip.
//!
//! Layout, all integers little-endian:
//!
//! | bytes | content |
//! |---|---|
//! | 12 | [`MAGIC`] |
//! | 4 | format version ([`FORMAT_VERSION`]) |
//! | 4 + n | header length, header ([`PackHeader`], postcard) |
//! | 8 + n | body length, body (postcard, then raw deflate at level 9) |
//! | 32 | SHA-256 of everything before it |

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read, Write};
use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;

use crate::archive::Archive;
use crate::cluster::{AmbiguityCounts, Clustering, Member, MergeReason, Station};
use crate::error::{Error, LimitKind, Result};
use crate::feed::{Feed, StopTime};
use crate::limits::Limits;
use crate::table::Table;
use crate::walks::WalkOverrides;

/// First bytes of every pack.
pub const MAGIC: &[u8] = b"ALLSTOPSPACK";

/// Format of packs this build writes and reads. Any change to the layout or
/// to the header or body types needs a new version.
pub const FORMAT_VERSION: u32 = 1;

/// What a pack was built from. Read without decoding the rest.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PackHeader {
    /// Registry ID of the feed, or `unregistered`.
    pub feed_id: String,
    /// SHA-256 of the full feed zip the pack was built from.
    pub feed_sha256: String,
    pub feed_version: String,
    /// Attribution text required by the feed's licence.
    pub attribution: String,
    pub timezone: String,
    /// First and last service date of the full feed, `YYYY-MM-DD`.
    pub validity: Option<(String, String)>,
    pub selection_name: String,
    /// SHA-256 of the selection, as canonical JSON.
    pub selection_sha256: String,
    /// SHA-256 of the rules the pack was built with, as canonical JSON.
    pub rules_sha256: String,
    pub station_overrides_sha256: Option<String>,
    pub walks_sha256: Option<String>,
    /// Connector modes whose trips the pack holds. Plans from the pack may
    /// use these or fewer.
    pub connector_modes: Vec<String>,
    /// The rules the pack was built with, as TOML: the defaults for plans.
    pub rules_toml: String,
    /// Program that wrote the pack.
    pub generator: String,
    /// Filled in by [`build`].
    pub counts: PackCounts,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackCounts {
    pub stations: usize,
    pub targets: usize,
    pub stops: usize,
    pub routes: usize,
    /// Trips after frequencies.txt expansion.
    pub trips: usize,
    pub stop_times: usize,
    /// Distinct stop patterns and timing patterns among the trips.
    pub stop_patterns: usize,
    pub timing_patterns: usize,
    /// Size of the body before compression.
    pub body_bytes: usize,
}

#[derive(Serialize, Deserialize)]
struct Body {
    stations: Vec<PackStation>,
    /// Indices into `stations`, in selection order.
    targets: Vec<u32>,
    visit_types: Vec<u16>,
    walks: WalkOverrides,
    /// The subset, without its stop times.
    feed: Feed,
    patterns: Vec<Vec<PatternCall>>,
    /// (arrival, departure) of each call, minus the trip's first departure.
    timings: Vec<Vec<(i32, i32)>>,
    /// One per trip of `feed`.
    trip_calls: Vec<TripCalls>,
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct PatternCall {
    stop: u32,
    sequence: u32,
    pickup_type: u8,
    drop_off_type: u8,
}

#[derive(Serialize, Deserialize)]
struct TripCalls {
    pattern: u32,
    timing: u32,
    /// First departure; 0 for a trip without stop times.
    base: i32,
}

#[derive(Serialize, Deserialize)]
struct PackStation {
    id: String,
    name: String,
    lat: f64,
    lon: f64,
    members: Vec<PackMember>,
}

#[derive(Serialize, Deserialize)]
struct PackMember {
    stop_id: String,
    reason: MergeReason,
}

/// Inputs of [`build`]: the full feed and what was derived from it.
pub struct PackSource<'a> {
    /// The full feed zip, whose rows the subset copies.
    pub zip: &'a [u8],
    pub feed: &'a Feed,
    /// Stations of the full feed, after overrides.
    pub clustering: &'a Clustering,
    /// Target stations, as indices into `clustering`.
    pub targets: &'a [u32],
    /// Route types whose trips count as visits.
    pub visit_types: &'a [u16],
    /// Route types of the connector modes.
    pub connector_types: &'a [RangeInclusive<u16>],
    pub walks: &'a WalkOverrides,
    pub header: PackHeader,
}

/// A pack, decoded.
#[derive(Debug)]
pub struct Pack {
    pub header: PackHeader,
    /// The feed subset, exactly as the loader read its rows.
    pub feed: Feed,
    pub clustering: Clustering,
    /// Target stations, as indices into `clustering`.
    pub targets: Vec<u32>,
    pub visit_types: Vec<u16>,
    pub walks: WalkOverrides,
}

fn pack_error(message: impl Into<String>) -> Error {
    Error::Pack(message.into())
}

/// Whether `bytes` start like a pack.
pub fn is_pack(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

/// Write a pack. The same source always gives the same bytes.
pub fn build(src: &PackSource, limits: &Limits) -> Result<Vec<u8>> {
    let feed = src.feed;
    let c = src.clustering;
    let keep_type = |rt: u16| {
        src.visit_types.contains(&rt) || src.connector_types.iter().any(|r| r.contains(&rt))
    };
    let routes: HashSet<String> = feed
        .routes
        .iter()
        .filter(|r| keep_type(r.route_type))
        .map(|r| r.id.clone())
        .collect();

    // Stations: the targets, every station a kept trip calls at, and the
    // stations of their stops' parents, so stops.txt stays consistent.
    let mut keep_station = vec![false; c.stations.len()];
    for &t in src.targets {
        keep_station[t as usize] = true;
    }
    for (ti, trip) in feed.trips.iter().enumerate() {
        if routes.contains(&feed.routes[trip.route as usize].id) {
            for st in feed.trip_stop_times(ti as u32) {
                keep_station[c.station_of_stop[st.stop as usize] as usize] = true;
            }
        }
    }
    loop {
        let mut changed = false;
        for (si, stop) in feed.stops.iter().enumerate() {
            if let Some(p) = stop.parent
                && keep_station[c.station_of_stop[si] as usize]
            {
                let ps = c.station_of_stop[p as usize] as usize;
                if !keep_station[ps] {
                    keep_station[ps] = true;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut new_index = vec![u32::MAX; c.stations.len()];
    let mut stations = Vec::new();
    let mut stops: HashSet<String> = HashSet::new();
    for (i, s) in c.stations.iter().enumerate() {
        if !keep_station[i] {
            continue;
        }
        new_index[i] = stations.len() as u32;
        for m in &s.members {
            stops.insert(m.stop_id.clone());
        }
        stations.push(PackStation {
            id: s.id.clone(),
            name: s.name.clone(),
            lat: s.lat,
            lon: s.lon,
            members: s
                .members
                .iter()
                .map(|m| PackMember {
                    stop_id: m.stop_id.clone(),
                    reason: m.reason,
                })
                .collect(),
        });
    }
    let kept_ids: HashSet<&str> = stations.iter().map(|s| s.id.as_str()).collect();
    let walks = WalkOverrides {
        walk: src
            .walks
            .walk
            .iter()
            .filter(|w| kept_ids.contains(w.from.as_str()) && kept_ids.contains(w.to.as_str()))
            .cloned()
            .collect(),
    };
    let targets: Vec<u32> = src.targets.iter().map(|&t| new_index[t as usize]).collect();

    let gtfs = subset_zip(src.zip, limits, &routes, &stops)?;
    let subset = Feed::from_zip_bytes(&gtfs, limits)?;
    let clustering = clustering_from(&subset, &stations)?;
    let (patterns, timings, trip_calls) = encode_stop_times(&subset)?;
    let mut header = src.header.clone();
    header.counts = PackCounts {
        stations: clustering.stations.len(),
        targets: targets.len(),
        stops: subset.stops.len(),
        routes: subset.routes.len(),
        trips: subset.trips.len(),
        stop_times: subset.stop_times.len(),
        stop_patterns: patterns.len(),
        timing_patterns: timings.len(),
        body_bytes: 0,
    };
    let body = Body {
        stations,
        targets,
        visit_types: src.visit_types.to_vec(),
        walks,
        feed: subset,
        patterns,
        timings,
        trip_calls,
    };
    let raw = encode(&body)?;
    header.counts.body_bytes = raw.len();
    let mut deflate = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(9));
    deflate
        .write_all(&raw)
        .map_err(|e| pack_error(format!("compressing the pack: {e}")))?;
    let packed = deflate
        .finish()
        .map_err(|e| pack_error(format!("compressing the pack: {e}")))?;

    let header = encode(&header)?;
    let mut out = Vec::with_capacity(packed.len() + header.len() + 64);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    let header_len =
        u32::try_from(header.len()).map_err(|_| pack_error("pack header too large"))?;
    out.extend_from_slice(&header_len.to_le_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(&(packed.len() as u64).to_le_bytes());
    out.extend_from_slice(&packed);
    let sum = Sha256::digest(&out);
    out.extend_from_slice(&sum);

    // Every pack is read back once: it must give exactly the feed that
    // loading the subset's rows gave.
    let back = read(&out, limits)?;
    if encode(&back.feed)? != encode(&body.feed)?
        || back.feed.stop_times != body.feed.stop_times
        || back.feed.stop_index != body.feed.stop_index
        || back.feed.trip_index != body.feed.trip_index
        || back.feed.route_index != body.feed.route_index
    {
        return Err(pack_error(
            "internal error: the pack does not read back as the feed it was built from",
        ));
    }
    Ok(out)
}

type StopTimeTables = (Vec<Vec<PatternCall>>, Vec<Vec<(i32, i32)>>, Vec<TripCalls>);

/// Split stop times into deduplicated stop and timing patterns. Fails if
/// the stop times are not laid out trip after trip, which decoding assumes.
fn encode_stop_times(feed: &Feed) -> Result<StopTimeTables> {
    let mut patterns: Vec<Vec<PatternCall>> = Vec::new();
    let mut pattern_ids: HashMap<Vec<PatternCall>, u32> = HashMap::new();
    let mut timings: Vec<Vec<(i32, i32)>> = Vec::new();
    let mut timing_ids: HashMap<Vec<(i32, i32)>, u32> = HashMap::new();
    let mut trip_calls = Vec::with_capacity(feed.trips.len());
    let mut next = 0u32;
    for (ti, trip) in feed.trips.iter().enumerate() {
        let r = &trip.stop_times;
        if !r.is_empty() && r.start != next || r.is_empty() && *r != (0..0) {
            return Err(pack_error(format!(
                "internal error: stop times of trip {:?} are not where packs expect them",
                trip.id
            )));
        }
        next = r.end.max(next);
        let st = feed.trip_stop_times(ti as u32);
        let base = st.first().map_or(0, |s| s.departure);
        let pattern: Vec<PatternCall> = st
            .iter()
            .map(|s| PatternCall {
                stop: s.stop,
                sequence: s.sequence,
                pickup_type: s.pickup_type,
                drop_off_type: s.drop_off_type,
            })
            .collect();
        let timing: Vec<(i32, i32)> = st
            .iter()
            .map(|s| (s.arrival - base, s.departure - base))
            .collect();
        let p = *pattern_ids.entry(pattern).or_insert_with_key(|k| {
            patterns.push(k.clone());
            patterns.len() as u32 - 1
        });
        let t = *timing_ids.entry(timing).or_insert_with_key(|k| {
            timings.push(k.clone());
            timings.len() as u32 - 1
        });
        trip_calls.push(TripCalls {
            pattern: p,
            timing: t,
            base,
        });
    }
    if next as usize != feed.stop_times.len() {
        return Err(pack_error(
            "internal error: some stop times belong to no trip",
        ));
    }
    Ok((patterns, timings, trip_calls))
}

/// Rebuild the stop times from the patterns and check every index in the
/// feed, so a crafted pack cannot make later code index out of bounds.
fn decode_feed(body: &mut Body) -> Result<()> {
    let bad = |m: String| pack_error(format!("the pack's timetable is inconsistent: {m}"));
    let feed = &mut body.feed;
    if body.trip_calls.len() != feed.trips.len() {
        return Err(bad("trip count".into()));
    }
    let mut stop_times: Vec<StopTime> = Vec::new();
    for (ti, (trip, tc)) in feed.trips.iter().zip(&body.trip_calls).enumerate() {
        let (Some(p), Some(t)) = (
            body.patterns.get(tc.pattern as usize),
            body.timings.get(tc.timing as usize),
        ) else {
            return Err(bad(format!("trip {:?} names a missing pattern", trip.id)));
        };
        let r = &trip.stop_times;
        let expected = if p.is_empty() {
            0..0
        } else {
            let start = stop_times.len() as u32;
            start..start + p.len() as u32
        };
        if p.len() != t.len() || *r != expected {
            return Err(bad(format!("trip {:?}", trip.id)));
        }
        for (c, &(a, d)) in p.iter().zip(t) {
            let (Some(arrival), Some(departure)) = (tc.base.checked_add(a), tc.base.checked_add(d))
            else {
                return Err(bad(format!("times of trip {:?}", trip.id)));
            };
            stop_times.push(StopTime {
                trip: ti as u32,
                stop: c.stop,
                sequence: c.sequence,
                arrival,
                departure,
                pickup_type: c.pickup_type,
                drop_off_type: c.drop_off_type,
            });
        }
    }
    feed.stop_times = stop_times;

    let (stops, routes, trips, services) = (
        feed.stops.len(),
        feed.routes.len(),
        feed.trips.len(),
        feed.service_ids.len(),
    );
    let within = |i: Option<u32>, n: usize| i.is_none_or(|i| (i as usize) < n);
    let ok = feed.stops.iter().all(|s| within(s.parent, stops))
        && feed
            .trips
            .iter()
            .all(|t| (t.route as usize) < routes && (t.service as usize) < services)
        && feed.stop_times.iter().all(|s| (s.stop as usize) < stops)
        && feed
            .calendars
            .iter()
            .all(|c| (c.service as usize) < services)
        && feed
            .calendar_dates
            .iter()
            .all(|c| (c.service as usize) < services)
        && feed.frequencies.iter().all(|f| (f.trip as usize) < trips)
        && feed.transfers.iter().all(|t| {
            within(t.from_stop, stops)
                && within(t.to_stop, stops)
                && within(t.from_route, routes)
                && within(t.to_route, routes)
                && within(t.from_trip, trips)
                && within(t.to_trip, trips)
        });
    if !ok {
        return Err(bad("an index is out of range".into()));
    }
    for (i, s) in feed.stops.iter().enumerate() {
        if feed.stop_index.insert(s.id.clone(), i as u32).is_some() {
            return Err(bad(format!("stop {:?} appears twice", s.id)));
        }
    }
    for (i, r) in feed.routes.iter().enumerate() {
        if feed.route_index.insert(r.id.clone(), i as u32).is_some() {
            return Err(bad(format!("route {:?} appears twice", r.id)));
        }
    }
    for (i, t) in feed.trips.iter().enumerate() {
        if feed.trip_index.insert(t.id.clone(), i as u32).is_some() {
            return Err(bad(format!("trip {:?} appears twice", t.id)));
        }
    }
    Ok(())
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    postcard::to_allocvec(value).map_err(|e| pack_error(format!("encoding the pack: {e}")))
}

/// Reads the sections of a pack in order.
struct Sections<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Sections<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.bytes.len());
        let Some(end) = end else {
            return Err(pack_error("the pack is truncated"));
        };
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn section(&mut self, len_bytes: usize) -> Result<&'a [u8]> {
        let raw = self.take(len_bytes)?;
        let mut buf = [0u8; 8];
        buf[..len_bytes].copy_from_slice(raw);
        let len = usize::try_from(u64::from_le_bytes(buf))
            .map_err(|_| pack_error("the pack is truncated"))?;
        self.take(len)
    }
}

/// Check the magic bytes and the format version; return a cursor after them.
fn open(bytes: &[u8]) -> Result<Sections<'_>> {
    if !is_pack(bytes) {
        return Err(pack_error(
            "not an allstops pack (the file does not start with ALLSTOPSPACK)",
        ));
    }
    let mut cur = Sections { bytes, at: 0 };
    cur.take(MAGIC.len())?;
    let version = u32::from_le_bytes(cur.take(4)?.try_into().expect("four bytes"));
    if version != FORMAT_VERSION {
        return Err(Error::PackVersion {
            found: version,
            supported: FORMAT_VERSION,
        });
    }
    Ok(cur)
}

fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8], what: &str) -> Result<T> {
    postcard::from_bytes(bytes)
        .map_err(|e| pack_error(format!("the pack's {what} is unreadable: {e}")))
}

/// Read only the header, without checking or decoding the rest.
pub fn read_header(bytes: &[u8]) -> Result<PackHeader> {
    let mut cur = open(bytes)?;
    decode(cur.section(4)?, "header")
}

/// Read and check a whole pack. The GTFS subset is loaded under `limits`,
/// and the pack itself may not be larger than the compressed-size limit.
pub fn read(bytes: &[u8], limits: &Limits) -> Result<Pack> {
    if bytes.len() as u64 > limits.max_compressed_bytes {
        return Err(Error::Limit(LimitKind::CompressedSize {
            limit: limits.max_compressed_bytes,
        }));
    }
    let mut cur = open(bytes)?;
    let Some(split) = bytes.len().checked_sub(32).filter(|&s| s >= cur.at) else {
        return Err(pack_error("the pack is truncated"));
    };
    if Sha256::digest(&bytes[..split]).as_slice() != &bytes[split..] {
        return Err(pack_error(
            "the pack's checksum does not match: the file is damaged or incomplete",
        ));
    }
    let header: PackHeader = decode(cur.section(4)?, "header")?;
    let packed = cur.section(8)?;
    if cur.at != split {
        return Err(pack_error("the pack has unexpected bytes after its data"));
    }
    let mut raw = Vec::new();
    flate2::read::DeflateDecoder::new(packed)
        .take(limits.max_uncompressed_bytes.saturating_add(1))
        .read_to_end(&mut raw)
        .map_err(|e| pack_error(format!("the pack's body is unreadable: {e}")))?;
    if raw.len() as u64 > limits.max_uncompressed_bytes {
        return Err(Error::Limit(LimitKind::UncompressedSize {
            limit: limits.max_uncompressed_bytes,
        }));
    }
    let mut body: Body = decode(&raw, "body")?;
    drop(raw);
    decode_feed(&mut body)?;
    let feed = body.feed;
    let clustering = clustering_from(&feed, &body.stations)?;
    if let Some(&t) = body
        .targets
        .iter()
        .find(|&&t| t as usize >= clustering.stations.len())
    {
        return Err(pack_error(format!(
            "the pack names target {t}, which is not a station"
        )));
    }
    Ok(Pack {
        header,
        feed,
        clustering,
        targets: body.targets,
        visit_types: body.visit_types,
        walks: body.walks,
    })
}

/// Rebuild the clustering of a pack's stations against its GTFS subset.
/// Every stop must belong to exactly one station.
fn clustering_from(feed: &Feed, stations: &[PackStation]) -> Result<Clustering> {
    let mut station_of_stop = vec![u32::MAX; feed.stops.len()];
    let mut ids = HashSet::new();
    let mut out = Vec::with_capacity(stations.len());
    for (si, ps) in stations.iter().enumerate() {
        if !ids.insert(ps.id.as_str()) {
            return Err(pack_error(format!(
                "the pack lists station {:?} twice",
                ps.id
            )));
        }
        let mut members = Vec::with_capacity(ps.members.len());
        for m in &ps.members {
            let Some(&stop) = feed.stop_index.get(&m.stop_id) else {
                return Err(pack_error(format!(
                    "station {:?} lists stop {:?}, which the pack's stops.txt does not have",
                    ps.id, m.stop_id
                )));
            };
            if station_of_stop[stop as usize] != u32::MAX {
                return Err(pack_error(format!(
                    "stop {:?} is in two stations",
                    m.stop_id
                )));
            }
            station_of_stop[stop as usize] = si as u32;
            members.push(Member {
                stop_id: m.stop_id.clone(),
                name: feed.stops[stop as usize].name.clone(),
                reason: m.reason,
                stop,
            });
        }
        out.push(Station {
            id: ps.id.clone(),
            name: ps.name.clone(),
            lat: ps.lat,
            lon: ps.lon,
            members,
        });
    }
    if let Some(i) = station_of_stop.iter().position(|&s| s == u32::MAX) {
        return Err(pack_error(format!(
            "stop {:?} belongs to no station of the pack",
            feed.stops[i].id
        )));
    }
    Ok(Clustering {
        stations: out,
        ambiguities: Vec::new(),
        ambiguity_counts: AmbiguityCounts::default(),
        complete: true,
        station_of_stop,
    })
}

/// Which rows of a file the subset keeps.
enum Keep<'a> {
    All,
    /// Rows whose value in this column is in the set.
    By(&'static str, &'a HashSet<String>),
    /// As `By`, and remember two more columns of every kept row.
    Trips(&'a HashSet<String>),
    Transfers,
}

/// Files of the subset, in the order they are written. Others (shapes,
/// pathways, fares, translations) are left out.
const FILES: [&str; 11] = [
    "agency.txt",
    "feed_info.txt",
    "attributions.txt",
    "routes.txt",
    "trips.txt",
    "stop_times.txt",
    "frequencies.txt",
    "calendar.txt",
    "calendar_dates.txt",
    "stops.txt",
    "transfers.txt",
];

/// The GTFS subset as a zip: fixed file order, fixed timestamps and
/// permissions, deflate level 6.
fn subset_zip(
    zip: &[u8],
    limits: &Limits,
    routes: &HashSet<String>,
    stops: &HashSet<String>,
) -> Result<Vec<u8>> {
    let mut archive = Archive::open(zip, limits)?;
    let mut trips: HashSet<String> = HashSet::new();
    let mut services: HashSet<String> = HashSet::new();
    let opts = SimpleFileOptions::DEFAULT
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(6))
        .unix_permissions(0o644)
        .system(zip::System::Unix);
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let zip_err = |e: zip::result::ZipError| pack_error(format!("writing the GTFS subset: {e}"));
    for name in FILES {
        let keep = match name {
            "routes.txt" => Keep::By("route_id", routes),
            "trips.txt" => Keep::Trips(routes),
            "stop_times.txt" | "frequencies.txt" => Keep::By("trip_id", &trips),
            "calendar.txt" | "calendar_dates.txt" => Keep::By("service_id", &services),
            "stops.txt" => Keep::By("stop_id", stops),
            "transfers.txt" => Keep::Transfers,
            _ => Keep::All,
        };
        let max_rows = limits.max_rows_per_file;
        let filtered = archive.with_reader(name, |r| {
            let mut t = Table::new(name, r, max_rows)?;
            filter_table(&mut t, &keep, routes, &trips, stops)
        })?;
        let Some((csv, kept_trips, kept_services)) = filtered else {
            continue;
        };
        trips.extend(kept_trips);
        services.extend(kept_services);
        w.start_file(name, opts).map_err(zip_err)?;
        w.write_all(&csv)
            .map_err(|e| pack_error(format!("writing the GTFS subset: {e}")))?;
    }
    Ok(w.finish().map_err(zip_err)?.into_inner())
}

/// Copy the header and the kept rows of one table. For trips.txt, also
/// return the trip and service IDs of the kept rows.
fn filter_table(
    t: &mut Table,
    keep: &Keep,
    routes: &HashSet<String>,
    trips: &HashSet<String>,
    stops: &HashSet<String>,
) -> Result<(Vec<u8>, Vec<String>, Vec<String>)> {
    let file = t.file().to_string();
    let mut out = csv::WriterBuilder::new()
        .flexible(true)
        .from_writer(Vec::new());
    let csv_err = |e: csv::Error| pack_error(format!("writing {file}: {e}"));
    out.write_record(t.headers()).map_err(csv_err)?;
    let col = |name: &str| t.column(name);
    let (key, trip_col, service_col) = match keep {
        Keep::By(c, _) => (col(c), None, None),
        Keep::Trips(_) => (col("route_id"), col("trip_id"), col("service_id")),
        _ => (None, None, None),
    };
    let refs: Vec<(Option<usize>, &HashSet<String>)> = match keep {
        Keep::Transfers => vec![
            (col("from_stop_id"), stops),
            (col("to_stop_id"), stops),
            (col("from_route_id"), routes),
            (col("to_route_id"), routes),
            (col("from_trip_id"), trips),
            (col("to_trip_id"), trips),
        ],
        _ => Vec::new(),
    };
    let mut kept_trips = Vec::new();
    let mut kept_services = Vec::new();
    while t.next_row()? {
        let keep_row = match keep {
            Keep::All => true,
            Keep::By(_, set) | Keep::Trips(set) => key.is_some() && set.contains(t.get(key)?),
            Keep::Transfers => {
                let mut ok = true;
                for (c, set) in &refs {
                    let v = t.get(*c)?;
                    if !v.is_empty() && !set.contains(v) {
                        ok = false;
                        break;
                    }
                }
                ok
            }
        };
        if !keep_row {
            continue;
        }
        if let Keep::Trips(_) = keep {
            kept_trips.push(t.get(trip_col)?.to_string());
            kept_services.push(t.get(service_col)?.to_string());
        }
        out.write_byte_record(t.record()).map_err(csv_err)?;
    }
    let bytes = out
        .into_inner()
        .map_err(|e| pack_error(format!("writing {file}: {e}")))?;
    Ok((bytes, kept_trips, kept_services))
}
