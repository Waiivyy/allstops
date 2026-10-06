//! `data/feeds.toml`: the list of known feeds with licence, attribution and a
//! pinned SHA-256 of the exact file the results were built from.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct FeedEntry {
    pub id: String,
    pub name: String,
    pub url: String,
    pub licence: String,
    #[allow(dead_code, reason = "shown by exports and the web app")]
    pub licence_url: String,
    pub attribution: String,
    #[allow(dead_code, reason = "shown by exports and the web app")]
    pub homepage: String,
    /// Lowercase hex SHA-256 of the pinned download.
    pub sha256: String,
    /// Date the pinned file was downloaded (YYYY-MM-DD).
    pub retrieved: String,
    #[serde(default)]
    #[allow(dead_code, reason = "checked when bundling packs for the web app")]
    pub redistribution: String,
}

impl FeedEntry {
    pub fn render_attribution(&self, feed_version: &str) -> String {
        let version = if feed_version.is_empty() {
            "unknown"
        } else {
            feed_version
        };
        self.attribution
            .replace("{retrieved}", &self.retrieved)
            .replace("{feed_version}", version)
    }
}

#[derive(Debug, Deserialize)]
struct RegistryFile {
    feed: Vec<FeedEntry>,
}

pub struct Registry {
    pub path: PathBuf,
    pub feeds: Vec<FeedEntry>,
}

impl Registry {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading feed registry {}", path.display()))?;
        let parsed: RegistryFile =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let mut ids = std::collections::HashSet::new();
        for f in &parsed.feed {
            if !ids.insert(f.id.as_str()) {
                bail!("{}: feed id {:?} appears twice", path.display(), f.id);
            }
            if !f.url.starts_with("https://") {
                bail!(
                    "{}: feed {:?} must use an https:// URL",
                    path.display(),
                    f.id
                );
            }
        }
        Ok(Registry {
            path: path.to_path_buf(),
            feeds: parsed.feed,
        })
    }

    pub fn get(&self, id: &str) -> Result<&FeedEntry> {
        self.feeds.iter().find(|f| f.id == id).with_context(|| {
            let known: Vec<&str> = self.feeds.iter().map(|f| f.id.as_str()).collect();
            format!("unknown feed {id:?}; known feeds: {}", known.join(", "))
        })
    }

    /// Rewrite the pinned hash and retrieval date of one feed, keeping the
    /// rest of the file (comments, order, formatting) unchanged.
    pub fn update_pin(&self, id: &str, sha256: &str, retrieved: &str) -> Result<()> {
        let text = std::fs::read_to_string(&self.path)?;
        let mut doc: toml_edit::DocumentMut = text.parse()?;
        let feeds = doc["feed"]
            .as_array_of_tables_mut()
            .context("feeds.toml: expected [[feed]] tables")?;
        let entry = feeds
            .iter_mut()
            .find(|t| t.get("id").and_then(|v| v.as_str()) == Some(id))
            .with_context(|| format!("feed {id:?} not found"))?;
        entry["sha256"] = toml_edit::value(sha256);
        entry["retrieved"] = toml_edit::value(retrieved);
        std::fs::write(&self.path, doc.to_string())?;
        Ok(())
    }
}
