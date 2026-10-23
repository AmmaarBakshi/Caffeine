//! Validating, installing and removing third-party plugins.
//!
//! Installing copies exactly the manifest, the executable for the current
//! platform and any top-level `README*`/`LICENSE*` files into
//! `<data>/plugins/<name>/`, keeping their relative paths. Nothing is
//! executed and the plugin is not enabled.

use std::path::{Path, PathBuf};

use serde::Serialize;

use caffeine_core::{Error, Paths, Result};
use caffeine_plugin_api::{MANIFEST_FILE, Manifest, Problem};

use crate::{Manager, Origin, compatibility_problems, read_manifest, resolve_executable};

/// Result of `caffeine plugin validate`.
#[derive(Debug, Clone, Serialize)]
pub struct ValidationReport {
    pub dir: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<Manifest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable: Option<PathBuf>,
    pub problems: Vec<Problem>,
}

impl ValidationReport {
    pub fn is_valid(&self) -> bool {
        self.manifest.is_some() && !self.problems.iter().any(Problem::is_error)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct InstallReport {
    pub name: String,
    pub version: semver::Version,
    pub dir: PathBuf,
    /// Version that was replaced by an upgrade.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaced: Option<semver::Version>,
    /// The plugin was enabled before and must be re-enabled (re-trusted).
    pub needs_reenable: bool,
}

/// Validate the plugin at `path` (a directory or its manifest file).
pub fn validate(path: &Path, host_version: &semver::Version) -> ValidationReport {
    let dir = if path.file_name().is_some_and(|n| n == MANIFEST_FILE) {
        path.parent().unwrap_or(Path::new(".")).to_path_buf()
    } else {
        path.to_path_buf()
    };
    let manifest = match read_manifest(&dir) {
        Ok(m) => m,
        Err(e) => {
            return ValidationReport {
                dir,
                manifest: None,
                executable: None,
                problems: vec![Problem::error(e)],
            };
        }
    };
    let mut problems = manifest.validate();
    problems.extend(compatibility_problems(&manifest, host_version));
    let (executable, exe_problems) = resolve_executable(&manifest, &dir);
    problems.extend(exe_problems);
    ValidationReport {
        dir,
        manifest: Some(manifest),
        executable,
        problems,
    }
}

impl Manager {
    /// Validate a plugin.
    pub fn validate(&self, path: &Path) -> ValidationReport {
        validate(path, self.host_version())
    }

    /// Install the plugin at `src`. With `upgrade`, an installed plugin of
    /// the same name is replaced.
    pub fn install(&mut self, src: &Path, upgrade: bool) -> Result<InstallReport> {
        let report = self.validate(src);
        let Some(manifest) = report.manifest.clone() else {
            return Err(Error::plugin(format!(
                "{} is not a valid plugin: {}",
                src.display(),
                report
                    .problems
                    .first()
                    .map(|p| p.message.as_str())
                    .unwrap_or("unknown error")
            )));
        };
        if !report.is_valid() {
            let errors: Vec<_> = report
                .problems
                .iter()
                .filter(|p| p.is_error())
                .map(|p| p.message.clone())
                .collect();
            return Err(Error::plugin(format!(
                "refusing to install {}:\n  - {}",
                manifest.name(),
                errors.join("\n  - ")
            ))
            .with_hint(format!("run `caffeine plugin validate {}`", src.display())));
        }
        let name = manifest.name().to_string();
        let existing = self.get(&name).cloned();
        let mut replaced = None;
        if let Some(existing) = &existing {
            match existing.origin {
                Origin::BuiltIn => {
                    return Err(Error::conflict(format!(
                        "\"{name}\" is the name of a built-in plugin"
                    )));
                }
                Origin::External => {
                    return Err(Error::conflict(format!(
                        "a plugin named \"{name}\" is already available from {}",
                        existing
                            .dir
                            .as_ref()
                            .map(|d| d.display().to_string())
                            .unwrap_or_default()
                    ))
                    .with_hint("remove that directory from plugins.search_paths first"));
                }
                Origin::Installed if !upgrade => {
                    return Err(Error::conflict(format!(
                        "plugin \"{name}\" {} is already installed",
                        existing.manifest.plugin.version
                    ))
                    .with_hint("pass --upgrade to replace it"));
                }
                Origin::Installed => replaced = Some(existing.manifest.plugin.version.clone()),
            }
        }

        let src_dir = report
            .dir
            .canonicalize()
            .map_err(|e| Error::io(format!("resolving {}", report.dir.display()), e))?;
        let mut files = vec![PathBuf::from(MANIFEST_FILE)];
        let exe = report
            .executable
            .as_ref()
            .expect("valid plugins have executables");
        files.push(
            exe.strip_prefix(&src_dir)
                .map_err(|_| Error::plugin("executable is outside the plugin directory"))?
                .to_path_buf(),
        );
        if let Ok(read) = std::fs::read_dir(&src_dir) {
            for entry in read.flatten() {
                let file_name = entry.file_name().to_string_lossy().to_ascii_uppercase();
                if entry.path().is_file()
                    && (file_name.starts_with("README") || file_name.starts_with("LICENSE"))
                {
                    files.push(PathBuf::from(entry.file_name()));
                }
            }
        }

        let plugin_dir = self.options.plugin_dir.clone();
        Paths::ensure_dir(&plugin_dir)?;
        let staging = plugin_dir.join(format!(".install-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&staging);
        let copied = (|| -> Result<()> {
            for rel in &files {
                let from = src_dir.join(rel);
                let to = staging.join(rel);
                if let Some(parent) = to.parent() {
                    Paths::ensure_dir(parent)?;
                }
                std::fs::copy(&from, &to)
                    .map_err(|e| Error::io(format!("copying {}", from.display()), e))?;
            }
            Ok(())
        })();
        if let Err(e) = copied {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }

        let dest = plugin_dir.join(&name);
        if dest.exists() {
            let old = plugin_dir.join(format!(".old-{name}-{}", std::process::id()));
            std::fs::rename(&dest, &old)
                .map_err(|e| Error::io(format!("moving aside {}", dest.display()), e))?;
            if let Err(e) = std::fs::rename(&staging, &dest) {
                let _ = std::fs::rename(&old, &dest);
                let _ = std::fs::remove_dir_all(&staging);
                return Err(Error::io(format!("installing into {}", dest.display()), e));
            }
            let _ = std::fs::remove_dir_all(&old);
        } else {
            std::fs::rename(&staging, &dest)
                .map_err(|e| Error::io(format!("installing into {}", dest.display()), e))?;
        }
        self.scan();
        let needs_reenable = self
            .get(&name)
            .is_some_and(|e| e.enabled && e.trust != crate::TrustStatus::Trusted);
        Ok(InstallReport {
            name,
            version: manifest.plugin.version,
            dir: dest,
            replaced,
            needs_reenable,
        })
    }

    /// Delete an installed plugin and its trust record. Returns the removed
    /// directory. The plugin's data directory is kept. The caller removes
    /// the name from `plugins.enabled`.
    pub fn remove(&mut self, name: &str) -> Result<PathBuf> {
        let entry = self.require(name)?.clone();
        if entry.origin != Origin::Installed {
            return Err(Error::usage(format!(
                "plugin \"{name}\" is {} and cannot be removed",
                entry.origin.as_str()
            ))
            .with_hint(match entry.origin {
                Origin::BuiltIn => "disable it with `caffeine plugin disable` instead".to_string(),
                _ => "remove its directory from plugins.search_paths".to_string(),
            }));
        }
        let dir = entry.dir.expect("installed plugins have directories");
        // Only ever delete directories directly inside the plugin directory.
        let root = self.options.plugin_dir.canonicalize().ok();
        let real = dir.canonicalize().ok();
        if root.is_none() || real.as_ref().and_then(|r| r.parent()) != root.as_deref() {
            return Err(Error::failure(format!(
                "refusing to delete {}: not inside the plugin directory",
                dir.display()
            )));
        }
        std::fs::remove_dir_all(&dir)
            .map_err(|e| Error::io(format!("removing {}", dir.display()), e))?;
        self.trust.remove(name)?;
        self.options.enabled.retain(|n| n != name);
        self.scan();
        Ok(dir)
    }
}
