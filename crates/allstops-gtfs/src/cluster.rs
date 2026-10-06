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

use serde::Serialize;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeReason {
    /// The station's own row, or a stop that is its own root.
    Root,
    ParentStation,
    DhidPrefix,
    NameDistance,
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
    pub ambiguities: Vec<Ambiguity>,
    /// Station index for every stop of the feed.
    #[serde(skip)]
    pub station_of_stop: Vec<u32>,
}

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
    for group in by_name.values() {
        for (k, &a) in group.iter().enumerate() {
            for &b in &group[k + 1..] {
                let (sa, sb) = (&feed.stops[a as usize], &feed.stops[b as usize]);
                if distance_m(sa.lat, sa.lon, sb.lat, sb.lon) <= cfg.same_name_m && uf.union(a, b) {
                    reason[b.max(a) as usize] = MergeReason::NameDistance;
                }
            }
        }
    }

    // Collect clusters, keyed by their union-find representative.
    let mut groups: BTreeMap<u32, Vec<StopIdx>> = BTreeMap::new();
    for i in 0..n as StopIdx {
        let lt = feed.stops[i as usize].location_type;
        if matches!(lt, LocationType::Platform | LocationType::Station) {
            groups.entry(uf.find(i)).or_default().push(i);
        }
    }
    let mut station_of_stop = vec![u32::MAX; n];
    let mut stations = Vec::with_capacity(groups.len());
    for members in groups.values() {
        let idx = stations.len() as u32;
        // Prefer a location_type=1 row for the name and position.
        let anchor = members
            .iter()
            .copied()
            .find(|&m| feed.stops[m as usize].location_type == LocationType::Station)
            .unwrap_or(members[0]);
        let a = &feed.stops[anchor as usize];
        let (lat, lon) = if a.lat.is_finite() {
            (a.lat, a.lon)
        } else {
            let pts: Vec<_> = members
                .iter()
                .map(|&m| &feed.stops[m as usize])
                .filter(|s| s.lat.is_finite())
                .collect();
            let k = pts.len().max(1) as f64;
            (
                pts.iter().map(|s| s.lat).sum::<f64>() / k,
                pts.iter().map(|s| s.lon).sum::<f64>() / k,
            )
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
    // Entrances and nodes inherit their root's station when it has one.
    for i in 0..n as StopIdx {
        if station_of_stop[i as usize] == u32::MAX {
            let r = uf.find(i);
            if let Some(&m) = groups.get(&r).and_then(|g| g.first()) {
                station_of_stop[i as usize] = station_of_stop[m as usize];
            }
        }
    }

    Clustering {
        ambiguities: ambiguities(&stations, cfg),
        stations,
        station_of_stop,
    }
}

fn ambiguities(stations: &[Station], cfg: &ClusterConfig) -> Vec<Ambiguity> {
    let mut out = Vec::new();
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, s) in stations.iter().enumerate() {
        by_name.entry(normalise_name(&s.name)).or_default().push(i);
    }
    for group in by_name.values() {
        for (k, &a) in group.iter().enumerate() {
            for &b in &group[k + 1..] {
                let d = distance_m(
                    stations[a].lat,
                    stations[a].lon,
                    stations[b].lat,
                    stations[b].lon,
                );
                if d > cfg.ambiguous_same_name_m {
                    out.push(ambiguity("same_name_far_apart", stations, a, b, d));
                }
            }
        }
    }
    // Different names very close: grid buckets of about the threshold size.
    let cell = cfg.ambiguous_close_m / 111_000.0;
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, s) in stations.iter().enumerate() {
        if s.lat.is_finite() {
            grid.entry(((s.lat / cell) as i64, (s.lon / cell) as i64))
                .or_default()
                .push(i);
        }
    }
    for (i, s) in stations.iter().enumerate() {
        if !s.lat.is_finite() {
            continue;
        }
        let (gy, gx) = ((s.lat / cell) as i64, (s.lon / cell) as i64);
        for dy in -1..=1 {
            for dx in -2..=2 {
                for &j in grid.get(&(gy + dy, gx + dx)).into_iter().flatten() {
                    if j <= i || normalise_name(&stations[j].name) == normalise_name(&s.name) {
                        continue;
                    }
                    let d = distance_m(s.lat, s.lon, stations[j].lat, stations[j].lon);
                    if d < cfg.ambiguous_close_m {
                        out.push(ambiguity("different_names_close", stations, i, j, d));
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.kind.cmp(b.kind).then(a.stations.cmp(&b.stations)));
    out
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
    fn distance_is_plausible() {
        // Marienplatz to Odeonsplatz is roughly 650 m.
        let d = distance_m(48.13737, 11.57538, 48.14263, 11.57741);
        assert!((550.0..700.0).contains(&d), "{d}");
    }
}
