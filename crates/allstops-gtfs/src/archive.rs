//! In-memory access to a GTFS zip. Entries are only ever read into memory and
//! looked up by their base name; nothing is written to disk.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use crate::error::{Error, LimitKind, Result};
use crate::limits::{ByteBudget, LimitedReader, Limits};

pub struct Archive<'a> {
    zip: zip::ZipArchive<Cursor<&'a [u8]>>,
    /// Base file name (for example `stops.txt`) to entry index.
    files: BTreeMap<String, usize>,
    budget: ByteBudget,
    limits: Limits,
}

impl<'a> Archive<'a> {
    pub fn open(bytes: &'a [u8], limits: &Limits) -> Result<Self> {
        if bytes.len() as u64 > limits.max_compressed_bytes {
            return Err(Error::Limit(LimitKind::CompressedSize {
                limit: limits.max_compressed_bytes,
            }));
        }
        let zip =
            zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| Error::Zip(e.to_string()))?;
        if zip.len() > limits.max_entries {
            return Err(Error::Limit(LimitKind::EntryCount {
                count: zip.len(),
                limit: limits.max_entries,
            }));
        }

        // GTFS puts files at the archive root. Some publishers wrap them in a
        // single folder, so accept files at depth one when the root has none.
        let mut root = BTreeMap::new();
        let mut nested = BTreeMap::new();
        for i in 0..zip.len() {
            let Some(name) = zip.name_for_index(i) else {
                continue;
            };
            if name.ends_with('/') {
                continue;
            }
            let parts: Vec<&str> = name.split('/').collect();
            let target = match parts.len() {
                1 => &mut root,
                2 => &mut nested,
                _ => continue,
            };
            let base = parts[parts.len() - 1].to_string();
            if target.insert(base.clone(), i).is_some() {
                return Err(Error::Zip(format!(
                    "archive contains {base} more than once"
                )));
            }
        }
        let files = if root.keys().any(|k| k.ends_with(".txt")) {
            root
        } else {
            nested
        };

        Ok(Archive {
            zip,
            files,
            budget: ByteBudget::new(limits.max_uncompressed_bytes),
            limits: limits.clone(),
        })
    }

    pub fn file_names(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.files.contains_key(name)
    }

    /// Run `f` with a limited, streaming reader over one entry. Returns `None`
    /// when the archive has no such file.
    pub fn with_reader<T>(
        &mut self,
        name: &str,
        f: impl FnOnce(&mut dyn Read) -> Result<T>,
    ) -> Result<Option<T>> {
        let Some(&index) = self.files.get(name) else {
            return Ok(None);
        };
        let entry = self
            .zip
            .by_index(index)
            .map_err(|e| Error::Zip(format!("{name}: {e}")))?;
        let mut reader =
            LimitedReader::new(entry, self.budget.clone(), name, self.limits.max_line_bytes);
        f(&mut reader).map(Some)
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }
}
