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
    /// Optional `stations.overrides.toml`, relative to the rules file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub station_overrides: Option<String>,
    /// Optional `walks.toml` with measured or forbidden walks, relative to
    /// the rules file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub walks: Option<String>,
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
            station_overrides: None,
            walks: None,
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

/// Longest plan window accepted, in seconds: 48 hours.
pub const MAX_WINDOW_S: i32 = 48 * 3600;

/// Longest transfer or walk minimum accepted, in seconds: one day.
pub const MAX_TRANSFER_S: i32 = 24 * 3600;

impl Rules {
    /// Check every value before any work starts. Returns all problems found,
    /// one per line.
    pub fn validate(&self) -> Result<(), String> {
        let mut bad: Vec<String> = Vec::new();
        let date_ok = self.date.len() == 10
            && self.date.as_bytes()[4] == b'-'
            && self.date.as_bytes()[7] == b'-'
            && self
                .date
                .bytes()
                .enumerate()
                .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit());
        if !date_ok {
            bad.push(format!("date {:?} is not YYYY-MM-DD", self.date));
        }
        match (
            parse_clock(&self.earliest_start),
            parse_clock(&self.latest_end),
        ) {
            (Some(s), Some(e)) if e <= s => {
                bad.push("latest_end must be after earliest_start".into())
            }
            (Some(s), Some(e)) if e - s > MAX_WINDOW_S => bad.push(format!(
                "the window from earliest_start to latest_end is longer than {} hours",
                MAX_WINDOW_S / 3600
            )),
            (None, _) => bad.push(format!(
                "earliest_start {:?} is not HH:MM",
                self.earliest_start
            )),
            (_, None) => bad.push(format!("latest_end {:?} is not HH:MM", self.latest_end)),
            _ => {}
        }
        if self.start.is_empty() || self.end.is_empty() {
            bad.push("start and end must be \"any\" or a station ID".into());
        }
        if !(self.walking_speed_kmh.is_finite()
            && self.walking_speed_kmh > 0.0
            && self.walking_speed_kmh <= 30.0)
        {
            bad.push("walking_speed_kmh must be above 0 and at most 30".into());
        }
        if !(self.walk_detour_factor.is_finite()
            && self.walk_detour_factor >= 1.0
            && self.walk_detour_factor <= 5.0)
        {
            bad.push("walk_detour_factor must be between 1 and 5".into());
        }
        if !(self.max_walk_m.is_finite() && self.max_walk_m >= 0.0 && self.max_walk_m <= 10_000.0) {
            bad.push("max_walk_m must be between 0 and 10000".into());
        }
        // At least one second: a change or walk taking no time would let
        // connections at the same instant chain, which the scans and the
        // lower bounds do not model.
        for (name, v) in [
            (
                "min_transfer_s.same_station",
                self.min_transfer_s.same_station,
            ),
            ("min_transfer_s.walk_link", self.min_transfer_s.walk_link),
        ] {
            if !(1..=MAX_TRANSFER_S).contains(&v) {
                bad.push(format!(
                    "{name} must be between 1 and {MAX_TRANSFER_S} seconds"
                ));
            }
        }
        if !(0..=MAX_TRANSFER_S).contains(&self.tight_transfer_s) {
            bad.push(format!(
                "tight_transfer_s must be between 0 and {MAX_TRANSFER_S} seconds"
            ));
        }
        for m in &self.connector_modes {
            if route_types_for_mode(m).is_none() {
                bad.push(format!("unknown connector mode {m:?}"));
            }
        }
        if bad.is_empty() {
            Ok(())
        } else {
            Err(bad.join("\n"))
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
    fn validation_rejects_nonsense_and_zero_transfer_times() {
        assert_eq!(Rules::default().validate(), Ok(()));
        let bad = |f: &dyn Fn(&mut Rules)| {
            let mut r = Rules::default();
            f(&mut r);
            r.validate().unwrap_err()
        };
        assert!(bad(&|r| r.min_transfer_s.same_station = 0).contains("same_station"));
        assert!(bad(&|r| r.min_transfer_s.walk_link = 0).contains("walk_link"));
        assert!(bad(&|r| r.min_transfer_s.same_station = i32::MAX).contains("same_station"));
        assert!(bad(&|r| r.walking_speed_kmh = 0.0).contains("walking_speed_kmh"));
        assert!(bad(&|r| r.walking_speed_kmh = f64::NAN).contains("walking_speed_kmh"));
        assert!(bad(&|r| r.walk_detour_factor = 0.5).contains("walk_detour_factor"));
        assert!(bad(&|r| r.max_walk_m = -1.0).contains("max_walk_m"));
        assert!(bad(&|r| r.max_walk_m = f64::INFINITY).contains("max_walk_m"));
        assert!(bad(&|r| r.latest_end = "04:00".into()).contains("after earliest_start"));
        assert!(bad(&|r| r.latest_end = "99:00".into()).contains("longer than"));
        assert!(bad(&|r| r.date = "14.11.2026".into()).contains("YYYY-MM-DD"));
        assert!(bad(&|r| r.connector_modes = vec!["hovercraft".into()]).contains("hovercraft"));
        // Every problem is reported, not just the first.
        let many = bad(&|r| {
            r.walking_speed_kmh = -1.0;
            r.min_transfer_s.walk_link = 0;
        });
        assert_eq!(many.lines().count(), 2);
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
