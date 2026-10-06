//! Run rules, saved with every result so a run can be reproduced. See
//! docs/RULES.md for what each option means.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Every target station must be visited.
    Stops,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MinTransfer {
    /// Changing between two trips at the same station, in seconds.
    pub same_station: i32,
    /// Shortest a walk between two stations may take, in seconds.
    pub walk_link: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    pub mode: Mode,
    pub selection: String,
    /// Plan date, `YYYY-MM-DD`.
    pub date: String,
    /// `HH:MM`, on the plan date's service day; may exceed 24:00.
    pub earliest_start: String,
    pub latest_end: String,
    /// `"any"` or a station ID.
    pub start: String,
    pub end: String,
    pub allow_walking: bool,
    /// Assumption, not a measurement.
    pub walking_speed_kmh: f64,
    /// Assumption: real path length divided by straight-line distance.
    pub walk_detour_factor: f64,
    pub max_walk_m: f64,
    /// Other public modes allowed for getting between target stations.
    /// Riding them never counts as a visit.
    pub connector_modes: Vec<String>,
    pub min_transfer_s: MinTransfer,
    pub tight_transfer_s: i32,
    pub count_pass_through: bool,
    pub stay_aboard_through_terminus: bool,
}

impl Default for Rules {
    fn default() -> Self {
        Rules {
            mode: Mode::Stops,
            selection: "selection.toml".into(),
            date: "2026-11-14".into(),
            earliest_start: "04:30".into(),
            latest_end: "26:00".into(),
            start: "any".into(),
            end: "any".into(),
            allow_walking: true,
            walking_speed_kmh: 4.5,
            walk_detour_factor: 1.3,
            max_walk_m: 1200.0,
            connector_modes: vec!["tram".into(), "bus".into()],
            min_transfer_s: MinTransfer {
                same_station: 60,
                walk_link: 120,
            },
            tight_transfer_s: 120,
            count_pass_through: false,
            stay_aboard_through_terminus: false,
        }
    }
}

/// Parse `HH:MM` or `HH:MM:SS`; hours may exceed 23.
pub fn parse_clock(s: &str) -> Option<i32> {
    let mut it = s.trim().split(':');
    let h: i32 = it.next()?.parse().ok()?;
    let m: i32 = it.next()?.parse().ok()?;
    let sec: i32 = match it.next() {
        Some(x) => x.parse().ok()?,
        None => 0,
    };
    if it.next().is_some()
        || !(0..=99).contains(&h)
        || !(0..60).contains(&m)
        || !(0..60).contains(&sec)
    {
        return None;
    }
    Some(h * 3600 + m * 60 + sec)
}

/// GTFS `route_type` values a mode name covers, basic and extended.
pub fn route_types_for_mode(mode: &str) -> Option<Vec<std::ops::RangeInclusive<u16>>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_parsing() {
        assert_eq!(parse_clock("04:30"), Some(4 * 3600 + 1800));
        assert_eq!(parse_clock("26:00"), Some(26 * 3600));
        assert_eq!(parse_clock("05:00:30"), Some(5 * 3600 + 30));
        assert_eq!(parse_clock("4:61"), None);
        assert_eq!(parse_clock("x"), None);
        assert_eq!(parse_clock("-1:00"), None);
    }

    #[test]
    fn defaults_match_the_documented_rules() {
        let r = Rules::default();
        assert_eq!(r.walking_speed_kmh, 4.5);
        assert_eq!(r.walk_detour_factor, 1.3);
        assert_eq!(r.max_walk_m, 1200.0);
        assert_eq!(r.min_transfer_s.same_station, 60);
        assert!(!r.count_pass_through && !r.stay_aboard_through_terminus);
    }

    #[test]
    fn mode_names() {
        assert!(route_types_for_mode("tram").unwrap()[0].contains(&0));
        assert!(
            route_types_for_mode("bus")
                .unwrap()
                .iter()
                .any(|r| r.contains(&3))
        );
        assert!(route_types_for_mode("hovercraft").is_none());
    }
}
