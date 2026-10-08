//! `walks.toml`: measured walk times and forbidden walks between stations,
//! by station ID. The network builder applies them to its generated walk
//! links; the verifier reads the same file to check walks.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

fn yes() -> bool {
    true
}

/// `walks.toml`: measured walk times and forbidden walks between stations.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WalkOverrides {
    #[serde(default)]
    pub walk: Vec<WalkOverride>,
}

/// One walk link, by station ID. Set either `seconds` (a measured time that
/// replaces the estimate; the walk-link minimum still applies) or `forbid`.
/// An override never creates a walk longer than `max_walk_m`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WalkOverride {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub seconds: Option<i32>,
    #[serde(default)]
    pub forbid: bool,
    /// Apply in both directions (default) or only from `from` to `to`.
    #[serde(default = "yes")]
    pub both_ways: bool,
    #[serde(default)]
    pub note: String,
}

impl WalkOverrides {
    pub fn validate(&self) -> std::result::Result<(), String> {
        for w in &self.walk {
            if w.from == w.to {
                return Err(format!("a walk from {:?} to itself", w.from));
            }
            match (w.seconds, w.forbid) {
                (Some(s), false) if !(1..=86_400).contains(&s) => {
                    return Err(format!(
                        "{} to {}: seconds must be between 1 and 86400",
                        w.from, w.to
                    ));
                }
                (Some(_), false) | (None, true) => {}
                _ => {
                    return Err(format!(
                        "{} to {}: set seconds or forbid, not both or neither",
                        w.from, w.to
                    ));
                }
            }
        }
        Ok(())
    }

    /// Directed rules, `(from, to)` to the measured seconds or `None` for a
    /// forbidden walk. Later entries replace earlier ones.
    pub fn directed(&self) -> BTreeMap<(String, String), Option<i32>> {
        let mut out = BTreeMap::new();
        for w in &self.walk {
            let v = if w.forbid { None } else { w.seconds };
            out.insert((w.from.clone(), w.to.clone()), v);
            if w.both_ways {
                out.insert((w.to.clone(), w.from.clone()), v);
            }
        }
        out
    }
}
