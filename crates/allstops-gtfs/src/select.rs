//! Target selection: which stations a run must visit. Saved as
//! `selection.toml` so a run can be reproduced.
//!
//! A station is selected when any `include` rule matches it and no
//! `exclude_stations` entry names it. A rule with trip filters matches the
//! stations where a matching trip has a scheduled stop (pickup or drop-off
//! allowed); all filters given in one rule must hold together.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::cluster::Clustering;
use crate::feed::Feed;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub name: String,
    #[serde(default)]
    pub include: Vec<Rule>,
    /// Station IDs to remove after all includes.
    #[serde(default)]
    pub exclude_stations: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// GTFS route_type values, for example 1 for subway/metro.
    #[serde(default)]
    pub route_types: Vec<u16>,
    #[serde(default)]
    pub agencies: Vec<String>,
    /// Glob patterns on route_short_name: `*` any run, `?` one character.
    #[serde(default)]
    pub route_short_names: Vec<String>,
    /// `[min_lon, min_lat, max_lon, max_lat]`, applied to station positions.
    #[serde(default)]
    pub bbox: Option<[f64; 4]>,
    /// Explicit station IDs. When given with trip filters, both must hold.
    #[serde(default)]
    pub stations: Vec<String>,
}

impl Rule {
    fn has_trip_filter(&self) -> bool {
        !self.route_types.is_empty()
            || !self.agencies.is_empty()
            || !self.route_short_names.is_empty()
    }
}

/// Minimal glob: `*` matches any run of characters, `?` exactly one.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SelectError {
    #[error("selection {0:?} has no include rules")]
    Empty(String),
    #[error("unknown station id {0:?} in selection")]
    UnknownStation(String),
    #[error("selection {0:?} matches no stations")]
    NoMatch(String),
    #[error(
        "selection {0:?} names no transit mode: add route_types, agencies or route_short_names to an include rule so it is clear which trips count as visits"
    )]
    NoVisitModes(String),
}

impl Rule {
    /// Whether a route passes this rule's route filters.
    fn matches_route(&self, route: &crate::feed::Route) -> bool {
        (self.route_types.is_empty() || self.route_types.contains(&route.route_type))
            && (self.agencies.is_empty() || self.agencies.contains(&route.agency_id))
            && (self.route_short_names.is_empty()
                || self
                    .route_short_names
                    .iter()
                    .any(|p| glob_match(p, &route.short_name)))
    }
}

/// The route types whose trips count as visits under this selection: the
/// route types of every route matched by an include rule's route filters.
/// Riding anything else never counts. A selection made only of station
/// lists or areas names no mode, which is an error rather than a silent
/// "every mode counts".
pub fn visit_route_types(feed: &Feed, sel: &Selection) -> Result<Vec<u16>, SelectError> {
    let mut types = BTreeSet::new();
    for rule in sel.include.iter().filter(|r| r.has_trip_filter()) {
        for route in &feed.routes {
            if rule.matches_route(route) {
                types.insert(route.route_type);
            }
        }
        // Explicit route types count even when no route has them today.
        types.extend(rule.route_types.iter().copied());
    }
    if types.is_empty() {
        return Err(SelectError::NoVisitModes(sel.name.clone()));
    }
    Ok(types.into_iter().collect())
}

/// Station indices (into `clustering.stations`) selected, in ascending order.
pub fn select(
    feed: &Feed,
    clustering: &Clustering,
    sel: &Selection,
) -> Result<Vec<u32>, SelectError> {
    if sel.include.is_empty() {
        return Err(SelectError::Empty(sel.name.clone()));
    }
    let by_id = |id: &str| {
        clustering
            .stations
            .iter()
            .position(|s| s.id == id)
            .map(|i| i as u32)
            .ok_or_else(|| SelectError::UnknownStation(id.to_string()))
    };

    let mut chosen = BTreeSet::new();
    for rule in &sel.include {
        let explicit: Option<BTreeSet<u32>> = if rule.stations.is_empty() {
            None
        } else {
            Some(
                rule.stations
                    .iter()
                    .map(|s| by_id(s))
                    .collect::<Result<_, _>>()?,
            )
        };
        let mut matched: BTreeSet<u32> = BTreeSet::new();
        if rule.has_trip_filter() {
            for (ti, trip) in feed.trips.iter().enumerate() {
                if !rule.matches_route(&feed.routes[trip.route as usize]) {
                    continue;
                }
                for st in feed.trip_stop_times(ti as u32) {
                    if !st.is_pass_through() {
                        matched.insert(clustering.station_of_stop[st.stop as usize]);
                    }
                }
            }
            if let Some(ex) = &explicit {
                matched.retain(|s| ex.contains(s));
            }
        } else if let Some(ex) = explicit {
            matched = ex;
        } else {
            // Only a bbox: every station inside it.
            matched = (0..clustering.stations.len() as u32).collect();
        }
        if let Some([min_lon, min_lat, max_lon, max_lat]) = rule.bbox {
            matched.retain(|&s| {
                let st = &clustering.stations[s as usize];
                st.lon >= min_lon && st.lon <= max_lon && st.lat >= min_lat && st.lat <= max_lat
            });
        }
        chosen.extend(matched);
    }
    for id in &sel.exclude_stations {
        chosen.remove(&by_id(id)?);
    }
    if chosen.is_empty() {
        return Err(SelectError::NoMatch(sel.name.clone()));
    }
    Ok(chosen.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Limits;
    use crate::cluster::{ClusterConfig, cluster};
    use crate::fixture::minimal_with;

    #[test]
    fn glob() {
        assert!(glob_match("U*", "U6"));
        assert!(glob_match("U?", "U6"));
        assert!(!glob_match("U?", "U66"));
        assert!(glob_match("*", ""));
        assert!(glob_match("S*1", "S21"));
        assert!(!glob_match("S*1", "S2"));
        assert!(glob_match("U", "U"));
        assert!(!glob_match("U", "SEV U"));
    }

    fn feed() -> Feed {
        Feed::from_zip_bytes(
            &minimal_with(&[
                (
                    "stops.txt",
                    "stop_id,stop_name,stop_lat,stop_lon\nA,A,48.10,11.50\nB,B,48.11,11.51\nC,C,48.12,11.52\nD,D,48.50,11.90\n",
                ),
                (
                    "routes.txt",
                    "route_id,agency_id,route_short_name,route_type\nU,A,U1,1\nT,A,17,0\n",
                ),
                ("trips.txt", "route_id,service_id,trip_id\nU,WD,T1\nT,WD,T2\n"),
                (
                    "stop_times.txt",
                    "trip_id,arrival_time,departure_time,stop_id,stop_sequence,pickup_type,drop_off_type\n\
                     T1,08:00:00,08:00:00,A,1,0,0\nT1,08:02:00,08:02:00,D,2,1,1\nT1,08:05:00,08:05:00,B,3,0,0\n\
                     T2,09:00:00,09:00:00,B,1,0,0\nT2,09:05:00,09:05:00,C,2,0,0\n",
                ),
            ]),
            &Limits::default(),
        )
        .unwrap()
    }

    fn ids(f: &Feed, sel: &Selection) -> Vec<String> {
        let c = cluster(f, &ClusterConfig::default());
        select(f, &c, sel)
            .unwrap()
            .into_iter()
            .map(|i| c.stations[i as usize].id.clone())
            .collect()
    }

    #[test]
    fn route_type_selects_scheduled_stops_only() {
        let f = feed();
        let sel = Selection {
            name: "metro".into(),
            include: vec![Rule {
                route_types: vec![1],
                ..Rule::default()
            }],
            ..Selection::default()
        };
        // D is a pass-through on T1 and must not be selected.
        assert_eq!(ids(&f, &sel), vec!["A", "B"]);
    }

    #[test]
    fn short_name_glob_bbox_and_exclusions() {
        let f = feed();
        let sel = Selection {
            name: "x".into(),
            include: vec![Rule {
                route_short_names: vec!["1?".into()],
                bbox: Some([11.505, 48.0, 11.6, 48.2]),
                ..Rule::default()
            }],
            exclude_stations: vec![],
        };
        assert_eq!(ids(&f, &sel), vec!["B", "C"]);
        let sel = Selection {
            exclude_stations: vec!["C".into()],
            ..sel
        };
        assert_eq!(ids(&f, &sel), vec!["B"]);
    }

    #[test]
    fn explicit_list_and_errors() {
        let f = feed();
        let c = cluster(&f, &ClusterConfig::default());
        let sel = Selection {
            name: "list".into(),
            include: vec![Rule {
                stations: vec!["A".into(), "D".into()],
                ..Rule::default()
            }],
            ..Selection::default()
        };
        assert_eq!(ids(&f, &sel), vec!["A", "D"]);
        let bad = Selection {
            name: "bad".into(),
            include: vec![Rule {
                stations: vec!["nope".into()],
                ..Rule::default()
            }],
            ..Selection::default()
        };
        assert_eq!(
            select(&f, &c, &bad),
            Err(SelectError::UnknownStation("nope".into()))
        );
        assert_eq!(
            select(&f, &c, &Selection::default()),
            Err(SelectError::Empty(String::new()))
        );
    }

    #[test]
    fn visit_modes_come_from_the_matched_routes() {
        let f = feed();
        let by_type = Selection {
            name: "metro".into(),
            include: vec![Rule {
                route_types: vec![1],
                ..Rule::default()
            }],
            ..Selection::default()
        };
        assert_eq!(visit_route_types(&f, &by_type).unwrap(), vec![1]);
        let by_name = Selection {
            name: "trams by name".into(),
            include: vec![Rule {
                route_short_names: vec!["1?".into()],
                ..Rule::default()
            }],
            ..Selection::default()
        };
        assert_eq!(visit_route_types(&f, &by_name).unwrap(), vec![0]);
        let only_stations = Selection {
            name: "list".into(),
            include: vec![Rule {
                stations: vec!["A".into()],
                ..Rule::default()
            }],
            ..Selection::default()
        };
        assert!(matches!(
            visit_route_types(&f, &only_stations),
            Err(SelectError::NoVisitModes(_))
        ));
    }

    #[test]
    fn toml_roundtrip() {
        let text = r#"
name = "Munich U-Bahn"
exclude_stations = ["de:09162:1"]

[[include]]
route_types = [1]
route_short_names = ["U*"]
bbox = [11.3, 48.0, 11.8, 48.3]
"#;
        let sel: Selection = toml::from_str(text).unwrap();
        assert_eq!(sel.include[0].route_types, vec![1]);
        assert_eq!(sel.include[0].bbox, Some([11.3, 48.0, 11.8, 48.3]));
        let back: Selection = toml::from_str(&toml::to_string(&sel).unwrap()).unwrap();
        assert_eq!(back, sel);
        assert!(toml::from_str::<Selection>("name = \"x\"\nroute_types = [1]\n").is_err());
    }
}
