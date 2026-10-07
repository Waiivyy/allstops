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
        // Check the entry count the archive declares before the zip crate
        // parses its whole central directory, so a crafted archive with
        // millions of entries is rejected cheaply.
        if let Some(count) = declared_entry_count(bytes)
            && count > limits.max_entries as u64
        {
            return Err(Error::Limit(LimitKind::EntryCount {
                count: usize::try_from(count).unwrap_or(usize::MAX),
                limit: limits.max_entries,
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

fn u16_at(b: &[u8], i: usize) -> Option<u64> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?) as u64)
}

fn u32_at(b: &[u8], i: usize) -> Option<u64> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?) as u64)
}

fn u64_at(b: &[u8], i: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(i..i + 8)?.try_into().ok()?))
}

/// Total entry count from the end-of-central-directory record (and its
/// zip64 extension when present), per the PKWARE APPNOTE. `None` when no
/// record is found; the zip crate then reports the archive as unreadable.
pub(crate) fn declared_entry_count(b: &[u8]) -> Option<u64> {
    const EOCD: u32 = 0x0605_4b50;
    const LOCATOR: u32 = 0x0706_4b50;
    const EOCD64: u32 = 0x0606_4b50;
    // The record is 22 bytes plus a comment of at most 65,535 bytes.
    let lowest = b.len().saturating_sub(22 + 65_535);
    let at = (lowest..=b.len().checked_sub(22)?)
        .rev()
        .find(|&i| u32_at(b, i) == Some(EOCD as u64))?;
    let entries = u16_at(b, at + 10)?;
    if entries != 0xFFFF {
        return Some(entries);
    }
    // Zip64: the locator sits just before the record and points to the
    // zip64 record, which holds the 64-bit count at offset 32.
    let loc = at.checked_sub(20)?;
    if u32_at(b, loc) != Some(LOCATOR as u64) {
        return Some(entries);
    }
    let rec = usize::try_from(u64_at(b, loc + 8)?).ok()?;
    if u32_at(b, rec) != Some(EOCD64 as u64) {
        return Some(entries);
    }
    u64_at(b, rec + 32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::zip_files;

    #[test]
    fn reads_the_declared_entry_count() {
        let names: Vec<String> = (0..70).map(|i| format!("f{i}.txt")).collect();
        let files: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "x")).collect();
        let bytes = zip_files(&files);
        assert_eq!(declared_entry_count(&bytes), Some(70));
        let err = Archive::open(&bytes, &Limits::default())
            .err()
            .expect("too many entries");
        assert!(
            matches!(err, Error::Limit(LimitKind::EntryCount { count: 70, .. })),
            "{err}"
        );
    }

    #[test]
    fn reads_a_zip64_count() {
        // A minimal zip64 trailer: zip64 record, locator, then a classic
        // record whose 16-bit count is the 0xFFFF marker.
        let mut b = vec![0u8; 8];
        let rec = b.len();
        b.extend_from_slice(&0x0606_4b50u32.to_le_bytes());
        b.extend_from_slice(&44u64.to_le_bytes()); // size of the rest
        b.extend_from_slice(&[0u8; 4]); // versions
        b.extend_from_slice(&[0u8; 8]); // disk numbers
        b.extend_from_slice(&5_000_000u64.to_le_bytes()); // entries on this disk
        b.extend_from_slice(&5_000_000u64.to_le_bytes()); // entries in total
        b.extend_from_slice(&[0u8; 16]); // directory size and offset
        b.extend_from_slice(&0x0706_4b50u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&(rec as u64).to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        b.extend_from_slice(&[0u8; 4]);
        b.extend_from_slice(&0xFFFFu16.to_le_bytes());
        b.extend_from_slice(&0xFFFFu16.to_le_bytes());
        b.extend_from_slice(&[0xFFu8; 8]);
        b.extend_from_slice(&0u16.to_le_bytes());
        assert_eq!(declared_entry_count(&b), Some(5_000_000));
        assert!(matches!(
            Archive::open(&b, &Limits::default()).err(),
            Some(Error::Limit(LimitKind::EntryCount { .. }))
        ));
    }

    #[test]
    fn junk_has_no_declared_count() {
        assert_eq!(declared_entry_count(b"not a zip"), None);
        assert_eq!(declared_entry_count(b""), None);
    }
}
