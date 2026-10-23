//! Trust records for third-party plugins.
//!
//! Enabling a third-party plugin records the SHA-256 digest of its manifest
//! and executable together with the permissions the user reviewed. Before
//! every run the digest is recomputed; if the files changed (an upgrade, or
//! tampering) the plugin is refused until the user enables it again.
//!
//! The digest covers the manifest and the executable for the current
//! platform only. Files the executable loads at run time are not covered.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use caffeine_core::{Error, Result, fsutil};
use caffeine_plugin_api::Permission;

const FORMAT: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustRecord {
    pub version: semver::Version,
    /// Hex SHA-256 over the manifest and the executable.
    pub digest: String,
    /// Permissions the user reviewed when enabling the plugin.
    pub permissions: Vec<Permission>,
    pub trusted_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TrustFile {
    format: u32,
    plugins: BTreeMap<String, TrustRecord>,
}

/// The trust store, `<data>/plugins/trust.json`.
#[derive(Debug, Clone)]
pub struct TrustStore {
    path: PathBuf,
    file: TrustFile,
}

impl TrustStore {
    pub fn load(path: &Path) -> Result<TrustStore> {
        let file = match fsutil::read_json::<TrustFile>(path)? {
            Some(f) if f.format == FORMAT => f,
            Some(f) => {
                return Err(Error::plugin(format!(
                    "{} has format {}, expected {FORMAT}",
                    path.display(),
                    f.format
                )));
            }
            None => TrustFile {
                format: FORMAT,
                plugins: BTreeMap::new(),
            },
        };
        Ok(TrustStore {
            path: path.to_path_buf(),
            file,
        })
    }

    pub fn get(&self, name: &str) -> Option<&TrustRecord> {
        self.file.plugins.get(name)
    }

    pub fn insert(&mut self, name: &str, record: TrustRecord) -> Result<()> {
        self.file.plugins.insert(name.to_string(), record);
        self.save()
    }

    pub fn remove(&mut self, name: &str) -> Result<bool> {
        let removed = self.file.plugins.remove(name).is_some();
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    fn save(&self) -> Result<()> {
        fsutil::write_json_atomic(&self.path, &self.file)
    }
}

/// Digest of a plugin's manifest and executable.
pub fn digest(manifest: &Path, executable: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    for (label, path) in [("manifest", manifest), ("executable", executable)] {
        let mut f = std::fs::File::open(path)
            .map_err(|e| Error::io(format!("reading plugin {label} {}", path.display()), e))?;
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        // Length-prefix each part so that moving bytes between the two
        // files cannot produce the same digest.
        hasher.update(label.as_bytes());
        hasher.update(len.to_le_bytes());
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = f
                .read(&mut buf)
                .map_err(|e| Error::io(format!("reading {}", path.display()), e))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_detects_changes_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let m = dir.path().join("m");
        let e = dir.path().join("e");
        std::fs::write(&m, "ab").unwrap();
        std::fs::write(&e, "c").unwrap();
        let first = digest(&m, &e).unwrap();
        assert_eq!(first.len(), 64);
        std::fs::write(&m, "a").unwrap();
        std::fs::write(&e, "bc").unwrap();
        assert_ne!(digest(&m, &e).unwrap(), first);

        let path = dir.path().join("trust.json");
        let mut store = TrustStore::load(&path).unwrap();
        assert!(store.get("x").is_none());
        let record = TrustRecord {
            version: semver::Version::new(1, 0, 0),
            digest: first,
            permissions: vec![Permission::ProcessSpawn],
            trusted_at: 1,
        };
        store.insert("x", record.clone()).unwrap();
        assert_eq!(TrustStore::load(&path).unwrap().get("x"), Some(&record));
        assert!(store.remove("x").unwrap());
        assert!(TrustStore::load(&path).unwrap().get("x").is_none());
    }
}
