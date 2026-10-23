//! Restore journal for settings Caffeine changes outside its own process.
//!
//! Before a backend modifies a persistent OS setting it records the original
//! value here (in the durable data directory, so the record survives
//! crashes and reboots). Restoring removes the entry. `caffeine doctor`
//! reports leftover entries and `caffeine lid restore` replays them.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use caffeine_core::session::store::new_session_id;
use caffeine_core::{Result, fsutil, time::unix_now};

pub const JOURNAL_FILE: &str = "restore-journal.json";

/// What kind of modification an entry describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum EntryKind {
    /// `caffeine lid set`: kept until `caffeine lid restore`.
    PersistentLidPolicy,
    /// A session-scoped lid override; restored when the session ends.
    SessionLidOverride { pid: u32 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub id: String,
    #[serde(flatten)]
    pub kind: EntryKind,
    pub created_at: u64,
    pub description: String,
    /// Backend-specific data needed to restore (original values etc.).
    pub payload: serde_json::Value,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct JournalFile {
    format: u32,
    entries: Vec<JournalEntry>,
}

/// Access to the journal file.
#[derive(Debug, Clone)]
pub struct Journal {
    path: PathBuf,
}

impl Journal {
    pub fn in_dir(data_dir: &Path) -> Journal {
        Journal {
            path: data_dir.join(JOURNAL_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn entries(&self) -> Result<Vec<JournalEntry>> {
        Ok(fsutil::read_json::<JournalFile>(&self.path)?
            .map(|f| f.entries)
            .unwrap_or_default())
    }

    fn save(&self, entries: Vec<JournalEntry>) -> Result<()> {
        if entries.is_empty() {
            return fsutil::remove_if_exists(&self.path);
        }
        fsutil::write_json_atomic(&self.path, &JournalFile { format: 1, entries })
    }

    /// Record a new entry and return its id.
    pub fn add(
        &self,
        kind: EntryKind,
        description: &str,
        payload: serde_json::Value,
    ) -> Result<String> {
        let mut entries = self.entries()?;
        let id = new_session_id();
        entries.push(JournalEntry {
            id: id.clone(),
            kind,
            created_at: unix_now(),
            description: description.to_string(),
            payload,
        });
        self.save(entries)?;
        Ok(id)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let mut entries = self.entries()?;
        entries.retain(|e| e.id != id);
        self.save(entries)
    }

    /// The first entry of the given kind, if any.
    pub fn find(&self, kind: &EntryKind) -> Result<Option<JournalEntry>> {
        Ok(self.entries()?.into_iter().find(|e| &e.kind == kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_find_remove() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::in_dir(dir.path());
        assert!(j.entries().unwrap().is_empty());
        let id = j
            .add(
                EntryKind::PersistentLidPolicy,
                "lid",
                serde_json::json!({"ac": 1}),
            )
            .unwrap();
        let sid = j
            .add(
                EntryKind::SessionLidOverride { pid: 7 },
                "session",
                serde_json::json!({}),
            )
            .unwrap();
        assert_eq!(j.entries().unwrap().len(), 2);
        assert_eq!(
            j.find(&EntryKind::PersistentLidPolicy).unwrap().unwrap().id,
            id
        );
        j.remove(&id).unwrap();
        j.remove(&sid).unwrap();
        assert!(!j.path().exists(), "empty journal is deleted");
    }
}
