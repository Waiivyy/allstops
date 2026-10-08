//! Station clustering: group GTFS stops into the stations a passenger would
//! name. Rules, applied in order:
//!
//! 1. `parent_station`: every stop joins the station of its topmost ancestor.
//! 2. DHID: stops whose IDs follow the German DHID scheme
//!    (`de:<area>:<station>[:<area>:<platform>]`) join by the station-level
//!    prefix `de:<area>:<station>`.
//! 3. Name and distance: remaining stops with the same normalised name
//!    within [`ClusterConfig::same_name_m`] of each other join.
//!
//! Every merge records its reason, and clusters that look ambiguous (same
//! name far apart, different names very close) are reported.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::feed::{Feed, LocationType, StopIdx};

#[derive(Debug, Clone)]
pub struct ClusterConfig {
    /// Stops with the same normalised name merge only within this distance.
    pub same_name_m: f64,
    /// Report same-named stations farther apart than this as ambiguous.
    pub ambiguous_same_name_m: f64,
    /// Report differently named stations closer than this as ambiguous.
    pub ambiguous_close_m: f64,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        ClusterConfig {
            same_name_m: 300.0,
            ambiguous_same_name_m: 1000.0,
            ambiguous_close_m: 50.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeReason {
    /// The station's own row, or a stop that is its own root.
    Root,
    ParentStation,
    DhidPrefix,
    NameDistance,
    /// Placed by stations.overrides.toml.
    Override,
}

#[derive(Debug, Clone, Serialize)]
pub struct Member {
    pub stop_id: String,
    pub name: String,
    pub reason: MergeReason,
    #[serde(skip)]
    pub stop: StopIdx,
}

#[derive(Debug, Clone, Serialize)]
pub struct Station {
    /// Stable identifier: the DHID station prefix, or the root stop's ID.
    pub id: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub members: Vec<Member>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Ambiguity {
    pub kind: &'static str,
    pub stations: [String; 2],
    pub names: [String; 2],
    pub distance_m: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Clustering {
    pub stations: Vec<Station>,
    /// Up to [`MAX_AMBIGUITY_SAMPLES`] ambiguous pairs, for review.
    pub ambiguities: Vec<Ambiguity>,
    /// All ambiguous pairs found, by kind, including those not kept as
    /// samples.
    pub ambiguity_counts: AmbiguityCounts,
    /// False when a work limit stopped a scan early; counts are then lower
    /// bounds and some merges by name may be missing.
    pub complete: bool,
    /// Station index for every stop of the feed. Every stop has one.
    #[serde(skip)]
    pub station_of_stop: Vec<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct AmbiguityCounts {
    pub same_name_far_apart: usize,
    pub different_names_close: usize,
}

/// Ambiguous pairs kept for display; the totals are still counted.
pub const MAX_AMBIGUITY_SAMPLES: usize = 1000;

/// Distance checks allowed per pairwise scan. Real feeds need far fewer;
/// the limit only stops crafted feeds (thousands of stops at one point)
/// from taking quadratic time.
pub const PAIR_CHECK_BUDGET: u64 = 20_000_000;

pub const EARTH_RADIUS_M: f64 = 6_371_008.8;

/// Great-circle distance in metres (haversine).
pub fn distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * a.sqrt().asin()
}

/// Lowercase, fold German umlauts and sharp s, keep letters and digits only.
pub fn normalise_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars().flat_map(char::to_lowercase) {
        match c {
            'ä' => out.push_str("ae"),
            'ö' => out.push_str("oe"),
            'ü' => out.push_str("ue"),
            'ß' => out.push_str("ss"),
            c if c.is_alphanumeric() => out.push(c),
            _ => {}
        }
    }
    out
}

/// `de:09162:6:41:41` and `de:09162:6` both give `de:09162:6`.
pub fn dhid_station_prefix(id: &str) -> Option<&str> {
    let mut parts = id.splitn(4, ':');
    let country = parts.next()?;
    let area = parts.next()?;
    let station = parts.next()?;
    let ok = country.len() == 2
        && country.bytes().all(|b| b.is_ascii_lowercase())
        && area.len() == 5
        && area.bytes().all(|b| b.is_ascii_digit())
        && !station.is_empty();
    ok.then(|| &id[..country.len() + 1 + area.len() + 1 + station.len()])
}

struct UnionFind(Vec<u32>);

impl UnionFind {
    fn new(n: usize) -> Self {
        UnionFind((0..n as u32).collect())
    }
    fn find(&mut self, mut x: u32) -> u32 {
        while self.0[x as usize] != x {
            let p = self.0[x as usize];
            self.0[x as usize] = self.0[p as usize];
            x = p;
        }
        x
    }
    fn union(&mut self, a: u32, b: u32) -> bool {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return false;
        }
        let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
        self.0[hi as usize] = lo;
        true
    }
}

/// Cluster the stops that matter for routing: platforms, stops and stations.
/// Entrances, generic nodes and boarding areas follow their parent but are
/// never on their own.
pub fn cluster(feed: &Feed, cfg: &ClusterConfig) -> Clustering {
    let n = feed.stops.len();
    let mut uf = UnionFind::new(n);
    let mut reason = vec![MergeReason::Root; n];

    // 1. parent_station.
    for i in 0..n as StopIdx {
        let root = feed.root_stop(i);
        if root != i {
            uf.union(i, root);
            reason[i as usize] = MergeReason::ParentStation;
        }
    }

    // 2. DHID station prefix, applied to roots only.
    let mut by_prefix: HashMap<&str, StopIdx> = HashMap::new();
    for i in 0..n as StopIdx {
        if feed.root_stop(i) != i {
            continue;
        }
        if let Some(prefix) = dhid_station_prefix(&feed.stops[i as usize].id) {
            match by_prefix.get(prefix) {
                Some(&first) => {
                    if uf.union(first, i) {
                        reason[i as usize] = MergeReason::DhidPrefix;
                    }
                }
                None => {
                    by_prefix.insert(prefix, i);
                }
            }
        }
    }

    // 3. Same normalised name within a distance, for roots that are still on
    // their own (no parent and no DHID merge).
    let mut by_name: HashMap<String, Vec<StopIdx>> = HashMap::new();
    for i in 0..n as StopIdx {
        let s = &feed.stops[i as usize];
        if feed.root_stop(i) == i && dhid_station_prefix(&s.id).is_none() && s.lat.is_finite() {
            by_name.entry(normalise_name(&s.name)).or_default().push(i);
        }
    }
    // Sweep each name group in latitude order, so only stops within the
    // merge distance north-south of each other are compared.
    let lat_window = cfg.same_name_m / 111_000.0;
    let mut checks: u64 = 0;
    let mut complete = true;
    let mut names: Vec<&String> = by_name.keys().collect();
    names.sort();
    'groups: for name in names {
        let mut group = by_name[name].clone();
        group.sort_by(|&a, &b| {
            let (la, lb) = (feed.stops[a as usize].lat, feed.stops[b as usize].lat);
            la.total_cmp(&lb).then(a.cmp(&b))
        });
        for (k, &a) in group.iter().enumerate() {
            let sa = &feed.stops[a as usize];
            for &b in &group[k + 1..] {
                let sb = &feed.stops[b as usize];
                if sb.lat - sa.lat > lat_window {
                    break;
                }
                checks += 1;
                if checks > PAIR_CHECK_BUDGET {
                    complete = false;
                    break 'groups;
                }
                if distance_m(sa.lat, sa.lon, sb.lat, sb.lon) <= cfg.same_name_m && uf.union(a, b) {
                    reason[b.max(a) as usize] = MergeReason::NameDistance;
                }
            }
        }
    }

    // Collect clusters, keyed by their union-find representative. Every stop
    // belongs to one, so nothing downstream ever meets a stop without a
    // station. Entrances, nodes and boarding areas normally join their
    // parent's cluster; one without a parent forms its own.
    let mut groups: BTreeMap<u32, Vec<StopIdx>> = BTreeMap::new();
    for i in 0..n as StopIdx {
        groups.entry(uf.find(i)).or_default().push(i);
    }
    let mut station_of_stop = vec![u32::MAX; n];
    let mut stations = Vec::with_capacity(groups.len());
    for members in groups.values() {
        let idx = stations.len() as u32;
        // Prefer a location_type=1 row for the name and position, then a
        // platform.
        let anchor = members
            .iter()
            .copied()
            .find(|&m| feed.stops[m as usize].location_type == LocationType::Station)
            .or_else(|| {
                members
                    .iter()
                    .copied()
                    .find(|&m| feed.stops[m as usize].location_type == LocationType::Platform)
            })
            .unwrap_or(members[0]);
        let a = &feed.stops[anchor as usize];
        let (lat, lon) = if a.lat.is_finite() && a.lon.is_finite() {
            (a.lat, a.lon)
        } else {
            // The mean of the members that have a position. A station whose
            // stops have none keeps NaN: it has no place on the map and no
            // walk links, rather than a made-up one.
            let pts: Vec<_> = members
                .iter()
                .map(|&m| &feed.stops[m as usize])
                .filter(|s| s.lat.is_finite() && s.lon.is_finite())
                .collect();
            if pts.is_empty() {
                (f64::NAN, f64::NAN)
            } else {
                let k = pts.len() as f64;
                (
                    pts.iter().map(|s| s.lat).sum::<f64>() / k,
                    pts.iter().map(|s| s.lon).sum::<f64>() / k,
                )
            }
        };
        let id = dhid_station_prefix(&a.id).unwrap_or(&a.id).to_string();
        for &m in members {
            station_of_stop[m as usize] = idx;
        }
        stations.push(Station {
            id,
            name: a.name.clone(),
            lat,
            lon,
            members: members
                .iter()
                .map(|&m| Member {
                    stop_id: feed.stops[m as usize].id.clone(),
                    name: feed.stops[m as usize].name.clone(),
                    reason: reason[m as usize],
                    stop: m,
                })
                .collect(),
        });
    }
    debug_assert!(station_of_stop.iter().all(|&x| x != u32::MAX));

    let (ambiguities, ambiguity_counts, scan_complete) = ambiguities(&stations, cfg);
    Clustering {
        ambiguities,
        ambiguity_counts,
        complete: complete && scan_complete,
        stations,
        station_of_stop,
    }
}

/// `stations.overrides.toml`: corrections applied after automatic
/// clustering, in a fixed order: every split, then every merge, then every
/// rename, each in file order. Later steps may use IDs created by earlier
/// ones.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StationOverrides {
    #[serde(default)]
    pub split: Vec<SplitOverride>,
    #[serde(default)]
    pub merge: Vec<MergeOverride>,
    #[serde(default)]
    pub rename: Vec<RenameOverride>,
}

/// Move stops out of a station into a new station.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SplitOverride {
    pub station: String,
    pub stops: Vec<String>,
    /// ID of the new station; must not exist yet.
    pub id: String,
    /// Name of the new station; defaults to the first moved stop's name.
    #[serde(default)]
    pub name: Option<String>,
    /// Why, for the record.
    #[serde(default)]
    pub note: String,
}

/// Join stations into one. The first keeps its place and, unless `id` is
/// given, its ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeOverride {
    pub stations: Vec<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenameOverride {
    pub station: String,
    pub name: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum OverrideError {
    #[error("station overrides: unknown station {0:?}")]
    UnknownStation(String),
    #[error("station overrides: stop {stop:?} is not in station {station:?}")]
    StopNotInStation { stop: String, station: String },
    #[error("station overrides: a merge needs at least two different stations")]
    MergeTooFew,
    #[error("station overrides: station ID {0:?} is already taken")]
    IdTaken(String),
    #[error("station overrides: the split would leave station {0:?} empty")]
    WouldEmpty(String),
    #[error("station overrides: the split into {0:?} names no stops")]
    SplitNoStops(String),
}

/// Apply overrides to a clustering, with the default configuration for the
/// recomputed ambiguity report.
pub fn apply_overrides(
    feed: &Feed,
    clustering: Clustering,
    ov: &StationOverrides,
) -> Result<Clustering, OverrideError> {
    apply_overrides_with(feed, clustering, ov, &ClusterConfig::default())
}

pub fn apply_overrides_with(
    feed: &Feed,
    clustering: Clustering,
    ov: &StationOverrides,
    cfg: &ClusterConfig,
) -> Result<Clustering, OverrideError> {
    // Working list: every station with a sort key that keeps the original
    // order and puts a split-off station right after its source.
    let mut slots: Vec<Option<((usize, usize), Station)>> = clustering
        .stations
        .into_iter()
        .enumerate()
        .map(|(i, s)| Some(((i, 0), s)))
        .collect();
    let mut index: HashMap<String, usize> = slots
        .iter()
        .enumerate()
        .filter_map(|(i, s)| s.as_ref().map(|(_, st)| (st.id.clone(), i)))
        .collect();
    let mut children: HashMap<usize, usize> = HashMap::new();

    for sp in &ov.split {
        let src = *index
            .get(&sp.station)
            .ok_or_else(|| OverrideError::UnknownStation(sp.station.clone()))?;
        if index.contains_key(&sp.id) {
            return Err(OverrideError::IdTaken(sp.id.clone()));
        }
        if sp.stops.is_empty() {
            return Err(OverrideError::SplitNoStops(sp.id.clone()));
        }
        let ((orig, _), source) = slots[src].as_mut().expect("indexed slots are live");
        let mut moved = Vec::new();
        for stop in &sp.stops {
            let pos = source
                .members
                .iter()
                .position(|m| &m.stop_id == stop)
                .ok_or_else(|| OverrideError::StopNotInStation {
                    stop: stop.clone(),
                    station: sp.station.clone(),
                })?;
            let mut m = source.members.remove(pos);
            m.reason = MergeReason::Override;
            moved.push(m);
        }
        if source.members.is_empty() {
            return Err(OverrideError::WouldEmpty(sp.station.clone()));
        }
        let pts: Vec<&crate::feed::Stop> = moved
            .iter()
            .map(|m| &feed.stops[m.stop as usize])
            .filter(|s| s.lat.is_finite() && s.lon.is_finite())
            .collect();
        let (lat, lon) = if pts.is_empty() {
            (f64::NAN, f64::NAN)
        } else {
            let k = pts.len() as f64;
            (
                pts.iter().map(|s| s.lat).sum::<f64>() / k,
                pts.iter().map(|s| s.lon).sum::<f64>() / k,
            )
        };
        let orig = *orig;
        let seq = children.entry(orig).or_insert(0);
        *seq += 1;
        let station = Station {
            id: sp.id.clone(),
            name: sp.name.clone().unwrap_or_else(|| moved[0].name.clone()),
            lat,
            lon,
            members: moved,
        };
        index.insert(sp.id.clone(), slots.len());
        slots.push(Some(((orig, *seq), station)));
    }

    for mg in &ov.merge {
        let mut ids: Vec<usize> = Vec::new();
        for id in &mg.stations {
            let i = *index
                .get(id)
                .ok_or_else(|| OverrideError::UnknownStation(id.clone()))?;
            if !ids.contains(&i) {
                ids.push(i);
            }
        }
        if ids.len() < 2 {
            return Err(OverrideError::MergeTooFew);
        }
        if let Some(new_id) = &mg.id
            && let Some(&other) = index.get(new_id)
            && !ids.contains(&other)
        {
            return Err(OverrideError::IdTaken(new_id.clone()));
        }
        let first = ids[0];
        let mut absorbed = Vec::new();
        for &i in &ids[1..] {
            let (_, st) = slots[i].take().expect("indexed slots are live");
            index.remove(&st.id);
            absorbed.extend(st.members.into_iter().map(|mut m| {
                m.reason = MergeReason::Override;
                m
            }));
        }
        let (_, target) = slots[first].as_mut().expect("indexed slots are live");
        target.members.extend(absorbed);
        if let Some(name) = &mg.name {
            target.name = name.clone();
        }
        if let Some(new_id) = &mg.id {
            index.remove(&target.id);
            target.id = new_id.clone();
            index.insert(new_id.clone(), first);
        }
    }

    for rn in &ov.rename {
        let i = *index
            .get(&rn.station)
            .ok_or_else(|| OverrideError::UnknownStation(rn.station.clone()))?;
        slots[i].as_mut().expect("indexed slots are live").1.name = rn.name.clone();
    }

    let mut live: Vec<((usize, usize), Station)> = slots.into_iter().flatten().collect();
    live.sort_by_key(|(k, _)| *k);
    let stations: Vec<Station> = live.into_iter().map(|(_, s)| s).collect();
    let mut station_of_stop = clustering.station_of_stop;
    for (i, st) in stations.iter().enumerate() {
        for m in &st.members {
            station_of_stop[m.stop as usize] = i as u32;
        }
    }
    let (ambiguities, ambiguity_counts, scan_complete) = ambiguities(&stations, cfg);
    Ok(Clustering {
        ambiguities,
        ambiguity_counts,
        complete: clustering.complete && scan_complete,
        stations,
        station_of_stop,
    })
}

/// Ambiguous pairs: samples, counts by kind, and whether the scans finished.
fn ambiguities(
    stations: &[Station],
    cfg: &ClusterConfig,
) -> (Vec<Ambiguity>, AmbiguityCounts, bool) {
    let mut out = Vec::new();
    let mut counts = AmbiguityCounts::default();
    let mut checks: u64 = 0;
    let mut complete = true;
    let mut record = |a: Ambiguity, out: &mut Vec<Ambiguity>| {
        match a.kind {
            "same_name_far_apart" => counts.same_name_far_apart += 1,
            _ => counts.different_names_close += 1,
        }
        if out.len() < MAX_AMBIGUITY_SAMPLES {
            out.push(a);
        }
    };

    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, s) in stations.iter().enumerate() {
        if s.lat.is_finite() && s.lon.is_finite() {
            by_name.entry(normalise_name(&s.name)).or_default().push(i);
        }
    }
    let mut names: Vec<&String> = by_name.keys().collect();
    names.sort();
    'same: for name in names {
        let group = &by_name[name];
        for (k, &a) in group.iter().enumerate() {
            for &b in &group[k + 1..] {
                checks += 1;
                if checks > PAIR_CHECK_BUDGET {
                    complete = false;
                    break 'same;
                }
                let d = distance_m(
                    stations[a].lat,
                    stations[a].lon,
                    stations[b].lat,
                    stations[b].lon,
                );
                if d > cfg.ambiguous_same_name_m {
                    record(
                        ambiguity("same_name_far_apart", stations, a, b, d),
                        &mut out,
                    );
                }
            }
        }
    }

    // Different names very close: grid buckets of about the threshold size.
    let cell = cfg.ambiguous_close_m / 111_000.0;
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, s) in stations.iter().enumerate() {
        if s.lat.is_finite() && s.lon.is_finite() {
            grid.entry(((s.lat / cell) as i64, (s.lon / cell) as i64))
                .or_default()
                .push(i);
        }
    }
    let mut checks: u64 = 0;
    'close: for (i, s) in stations.iter().enumerate() {
        if !(s.lat.is_finite() && s.lon.is_finite()) {
            continue;
        }
        let name_i = normalise_name(&s.name);
        let (gy, gx) = ((s.lat / cell) as i64, (s.lon / cell) as i64);
        for dy in -1..=1 {
            for dx in -2..=2 {
                for &j in grid.get(&(gy + dy, gx + dx)).into_iter().flatten() {
                    if j <= i {
                        continue;
                    }
                    checks += 1;
                    if checks > PAIR_CHECK_BUDGET {
                        complete = false;
                        break 'close;
                    }
                    if normalise_name(&stations[j].name) == name_i {
                        continue;
                    }
                    let d = distance_m(s.lat, s.lon, stations[j].lat, stations[j].lon);
                    if d < cfg.ambiguous_close_m {
                        record(
                            ambiguity("different_names_close", stations, i, j, d),
                            &mut out,
                        );
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.kind.cmp(b.kind).then(a.stations.cmp(&b.stations)));
    (out, counts, complete)
}

fn ambiguity(kind: &'static str, s: &[Station], a: usize, b: usize, d: f64) -> Ambiguity {
    Ambiguity {
        kind,
        stations: [s[a].id.clone(), s[b].id.clone()],
        names: [s[a].name.clone(), s[b].name.clone()],
        distance_m: (d * 10.0).round() / 10.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Limits;
    use crate::fixture::minimal_with;

    fn load(stops: &str) -> Feed {
        Feed::from_zip_bytes(&minimal_with(&[("stops.txt", stops)]), &Limits::default()).unwrap()
    }

    #[test]
    fn dhid_prefix_parsing() {
        assert_eq!(dhid_station_prefix("de:09162:6:41:41"), Some("de:09162:6"));
        assert_eq!(dhid_station_prefix("de:09162:6"), Some("de:09162:6"));
        assert_eq!(dhid_station_prefix("at:45:50002:0:10"), None);
        assert_eq!(dhid_station_prefix("de:0916:6"), None);
        assert_eq!(dhid_station_prefix("de:09162:"), None);
        assert_eq!(dhid_station_prefix("x"), None);
        assert_eq!(dhid_station_prefix(""), None);
        assert_eq!(dhid_station_prefix("dé:09162:6"), None);
        assert_eq!(dhid_station_prefix("ü:12345:6"), None);
    }

    #[test]
    fn normalises_umlauts_and_punctuation() {
        assert_eq!(
            normalise_name("Münchner Freiheit"),
            normalise_name("Muenchner-Freiheit")
        );
        assert_eq!(normalise_name("Fröttmaning"), "froettmaning");
        assert_eq!(normalise_name("Implerstraße"), "implerstrasse");
    }

    #[test]
    fn platforms_with_parent_merge() {
        let f = load(
            "stop_id,stop_name,stop_lat,stop_lon,location_type,parent_station\n\
             P,Hbf,48.14,11.56,1,\nP1,Hbf Gleis 1,48.1401,11.5601,0,P\nP2,Hbf Gleis 2,48.1402,11.5602,0,P\n\
             S1a,One,48.1,11.5,0,\nS2a,Two,48.11,11.51,0,\n",
        );
        let c = cluster(&f, &ClusterConfig::default());
        let hbf = c.stations.iter().find(|s| s.id == "P").unwrap();
        assert_eq!(hbf.members.len(), 3);
        assert_eq!(hbf.name, "Hbf");
        assert!(
            hbf.members
                .iter()
                .any(|m| m.reason == MergeReason::ParentStation)
        );
    }

    #[test]
    fn dhid_ids_merge_at_station_level() {
        let f = load(
            "stop_id,stop_name,stop_lat,stop_lon\n\
             de:09162:6:1:1,Hauptbahnhof,48.140,11.560\nde:09162:6:2:2,Hauptbahnhof,48.141,11.561\n\
             de:09162:7:1:1,Other,48.150,11.570\nS1a,One,48.1,11.5\nS2a,Two,48.11,11.51\n",
        );
        let c = cluster(&f, &ClusterConfig::default());
        let hbf = c.stations.iter().find(|s| s.id == "de:09162:6").unwrap();
        assert_eq!(hbf.members.len(), 2);
        assert!(c.stations.iter().any(|s| s.id == "de:09162:7"));
    }

    #[test]
    fn same_name_near_merges_far_stays_separate() {
        let f = load(
            "stop_id,stop_name,stop_lat,stop_lon\n\
             A1,Bahnhofstraße,48.1000,11.5000\nA2,Bahnhofstrasse,48.1010,11.5000\n\
             B1,Bahnhofstraße,48.2000,11.5000\nS1a,One,48.1,11.6\nS2a,Two,48.11,11.61\n",
        );
        let c = cluster(&f, &ClusterConfig::default());
        let named: Vec<_> = c
            .stations
            .iter()
            .filter(|s| normalise_name(&s.name) == "bahnhofstrasse")
            .collect();
        assert_eq!(
            named.len(),
            2,
            "two near stops merge, the far one stays apart"
        );
        assert!(named.iter().any(|s| s.members.len() == 2));
        assert!(
            c.ambiguities
                .iter()
                .any(|a| a.kind == "same_name_far_apart")
        );
    }

    #[test]
    fn different_names_very_close_are_reported() {
        let f = load(
            "stop_id,stop_name,stop_lat,stop_lon\n\
             X,Rathaus,48.1000,11.5000\nY,Marienplatz,48.10002,11.50002\nS1a,One,48.2,11.6\nS2a,Two,48.21,11.61\n",
        );
        let c = cluster(&f, &ClusterConfig::default());
        assert_eq!(
            c.stations.len(),
            4,
            "close but differently named stops do not merge"
        );
        assert!(
            c.ambiguities
                .iter()
                .any(|a| a.kind == "different_names_close")
        );
    }

    #[test]
    fn every_platform_has_a_station() {
        let f = load(
            "stop_id,stop_name,stop_lat,stop_lon,location_type,parent_station\n\
             P,Hbf,48.14,11.56,1,\nP1,Hbf 1,48.14,11.56,0,P\nE,Hbf entrance,48.14,11.56,2,P\n\
             S1a,One,48.1,11.5,0,\nS2a,Two,48.11,11.51,0,\n",
        );
        let c = cluster(&f, &ClusterConfig::default());
        assert!(c.station_of_stop.iter().all(|&s| s != u32::MAX));
        assert_eq!(
            c.station_of_stop[f.stop_index["E"] as usize],
            c.station_of_stop[f.stop_index["P"] as usize]
        );
    }

    #[test]
    fn a_stop_without_station_still_gets_one() {
        // An entrance with no parent, used by a trip: invalid GTFS, but it
        // must not leave a stop without a station.
        let f = load(
            "stop_id,stop_name,stop_lat,stop_lon,location_type,parent_station\n\
             S1a,One,48.1,11.5,0,\nS2a,Two,48.11,11.51,0,\nE,Gate,48.12,11.52,2,\n",
        );
        let c = cluster(&f, &ClusterConfig::default());
        assert!(
            c.station_of_stop
                .iter()
                .all(|&s| (s as usize) < c.stations.len())
        );
    }

    #[test]
    fn a_station_without_coordinates_has_no_position() {
        let f = load(
            "stop_id,stop_name,stop_lat,stop_lon,location_type,parent_station\n\
             P,Nowhere,,,1,\nP1,Nowhere 1,,,0,P\nS1a,One,48.1,11.5,0,\nS2a,Two,48.11,11.51,0,\n",
        );
        let c = cluster(&f, &ClusterConfig::default());
        let p = c.stations.iter().find(|s| s.id == "P").unwrap();
        assert!(
            p.lat.is_nan() && p.lon.is_nan(),
            "got ({}, {})",
            p.lat,
            p.lon
        );
    }

    #[test]
    fn ambiguity_samples_are_capped_but_counted() {
        let mut stops = String::from(
            "stop_id,stop_name,stop_lat,stop_lon\nS1a,One,48.0,11.0\nS2a,Two,48.01,11.01\n",
        );
        for i in 0..60 {
            stops.push_str(&format!("X{i},Name {i},48.2,11.6\n"));
        }
        let f = load(&stops);
        let c = cluster(&f, &ClusterConfig::default());
        // 60 differently named stops at one point: 60 * 59 / 2 close pairs.
        assert_eq!(c.ambiguity_counts.different_names_close, 1770);
        assert_eq!(c.ambiguities.len(), MAX_AMBIGUITY_SAMPLES.min(1770));
        assert!(c.complete);
    }

    #[test]
    fn distance_is_plausible() {
        // Marienplatz to Odeonsplatz is roughly 650 m.
        let d = distance_m(48.13737, 11.57538, 48.14263, 11.57741);
        assert!((550.0..700.0).contains(&d), "{d}");
    }
}
