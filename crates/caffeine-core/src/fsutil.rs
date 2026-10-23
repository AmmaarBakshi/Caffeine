//! Crash-safe file helpers.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::paths::Paths;

/// Atomically replace `path` with `contents`.
///
/// The data is written to a temporary file in the same directory, flushed to
/// disk, and renamed over the destination, so readers observe either the
/// old or the new file and never a truncated one. When `backup` is set and
/// the destination exists, its previous contents are copied to
/// `<name>.bak` first.
pub fn write_atomic(path: &Path, contents: &[u8], backup: bool) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Paths::ensure_dir(parent)?;
    let file_name = path
        .file_name()
        .ok_or_else(|| Error::failure(format!("invalid file path {}", path.display())))?
        .to_string_lossy()
        .into_owned();

    if backup && path.exists() {
        let bak = backup_path(path);
        fs::copy(path, &bak).map_err(|e| Error::io(format!("backing up to {}", bak.display()), e))?;
    }

    let tmp = parent.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    let result = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| Error::io(format!("creating {}", tmp.display()), e))?;
        f.write_all(contents)
            .map_err(|e| Error::io(format!("writing {}", tmp.display()), e))?;
        f.sync_all()
            .map_err(|e| Error::io(format!("flushing {}", tmp.display()), e))?;
        drop(f);
        // `fs::rename` replaces an existing destination on every supported
        // platform (MoveFileExW with MOVEFILE_REPLACE_EXISTING on Windows).
        fs::rename(&tmp, path)
            .map_err(|e| Error::io(format!("replacing {}", path.display()), e))?;
        #[cfg(unix)]
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// The `.bak` sibling used by [`write_atomic`].
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    path.with_file_name(name)
}

/// Read a file to a string, returning `Ok(None)` if it does not exist.
pub fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| Error::config(format!("{} is not valid UTF-8", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(format!("reading {}", path.display()), e)),
    }
}

/// Serialize `value` as pretty JSON and write it atomically.
pub fn write_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| Error::failure(format!("serializing {}: {e}", path.display())))?;
    bytes.push(b'\n');
    write_atomic(path, &bytes, false)
}

/// Read and parse a JSON file, returning `Ok(None)` if it does not exist.
pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match read_optional(path)? {
        None => Ok(None),
        Some(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| Error::failure(format!("{} is corrupt: {e}", path.display()))),
    }
}

/// Remove a file, treating "already gone" as success.
pub fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(format!("removing {}", path.display()), e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_and_backs_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("file.toml");
        write_atomic(&path, b"one", true).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "one");
        assert!(!backup_path(&path).exists());
        write_atomic(&path, b"two", true).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        assert_eq!(fs::read_to_string(backup_path(&path)).unwrap(), "one");
        // No temporary files are left behind.
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn optional_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.json");
        assert_eq!(read_optional(&path).unwrap(), None);
        assert_eq!(read_json::<serde_json::Value>(&path).unwrap(), None);
        write_json_atomic(&path, &serde_json::json!({"a": 1})).unwrap();
        assert_eq!(
            read_json::<serde_json::Value>(&path).unwrap(),
            Some(serde_json::json!({"a": 1}))
        );
        remove_if_exists(&path).unwrap();
        remove_if_exists(&path).unwrap();
    }
}
