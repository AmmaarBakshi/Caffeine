//! `caffeine plugin create`: generate a starter plugin project.
//!
//! The template is the `caffeine-plugin-hello` example from this
//! repository with its name substituted, so the generated code is exactly
//! what Caffeine's own test suite builds and runs.

use std::path::{Path, PathBuf};

use caffeine_core::{Error, Result, fsutil};
use caffeine_plugin_api::{MANIFEST_FILE, is_valid_name};

const MAIN_RS: &str = include_str!("../../caffeine-plugin-hello/src/main.rs");
const MANIFEST: &str = include_str!("../../caffeine-plugin-hello/caffeine-plugin.toml");

/// Where the generated project gets `caffeine-plugin-api` from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiSource {
    /// The Caffeine git repository (default).
    Git(String),
    /// A local checkout, for offline work or developing Caffeine itself.
    Path(PathBuf),
}

impl ApiSource {
    fn dependency(&self) -> String {
        match self {
            ApiSource::Git(url) => format!("{{ git = \"{url}\" }}"),
            ApiSource::Path(p) => {
                let p = crate::simplify_path(p.clone())
                    .display()
                    .to_string()
                    .replace('\\', "/");
                format!("{{ path = \"{p}\" }}")
            }
        }
    }
}

/// Files of a generated project, relative to its root.
pub fn render(name: &str, api: &ApiSource) -> Result<Vec<(PathBuf, String)>> {
    if !is_valid_name(name) {
        return Err(Error::usage(format!(
            "\"{name}\" is not a valid plugin name: use lowercase letters, digits and single \
             hyphens, starting with a letter"
        )));
    }
    let camel: String = name
        .split('-')
        .map(|part| {
            let mut c = part.chars();
            c.next()
                .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                .unwrap_or_default()
        })
        .collect();
    let subst = |text: &str| text.replace("hello", name).replace("Hello", &camel);
    let cargo_toml = format!(
        r#"[package]
name = "caffeine-plugin-{name}"
version = "0.1.0"
edition = "2024"
publish = false

[[bin]]
name = "caffeine-{name}"
path = "src/main.rs"

[dependencies]
caffeine-plugin-api = {api}
serde_json = "1"

[profile.release]
strip = "symbols"
"#,
        api = api.dependency()
    );
    let readme = format!(
        r#"# {name}

A [Caffeine](https://github.com/AMMAAR-IC/caffeine) plugin.

## Develop

```sh
cargo build --release          # the manifest points at target/release/caffeine-{name}
cargo test
caffeine plugin validate .     # manifest, executable and compatibility checks
```

To try it without installing, add this directory to `plugins.search_paths`
(`caffeine config set plugins.search_paths "$PWD"`), then
`caffeine plugin enable {name}` and `caffeine {name}`.

## Install

```sh
caffeine plugin install .      # copies the manifest and executable; runs nothing
caffeine plugin enable {name}  # review permissions and trust this build
caffeine {name} you
```

Rebuilding changes the executable, so Caffeine refuses to run it until you
enable it again.

## How it works

`caffeine-plugin.toml` declares the plugin's commands, settings and
permissions. Caffeine starts the executable for each command and speaks the
plugin protocol (JSON Lines) on its stdin and stdout; `src/main.rs`
implements the `Plugin` trait and lets `caffeine_plugin_api::sdk::serve` handle
the protocol. Never print to stdout: use `host.print` and `host.data`, and give
programs you start `sdk::child_stdout()`.

Commands are registered by adding a `[[commands]]` entry to the manifest and a
matching arm in `invoke`. Settings declared under `[[settings]]` are read from
`[plugins.settings.{name}]` in the Caffeine configuration, validated, and
passed to `initialize`.
"#
    );
    Ok(vec![
        (PathBuf::from("Cargo.toml"), cargo_toml),
        (PathBuf::from(MANIFEST_FILE), subst(MANIFEST)),
        (Path::new("src").join("main.rs"), subst(MAIN_RS)),
        (PathBuf::from("README.md"), readme),
        (PathBuf::from(".gitignore"), "/target\n".to_string()),
    ])
}

/// Write a new project into `dir`, which must not exist or be empty.
/// Returns the written files.
pub fn create(dir: &Path, name: &str, api: &ApiSource) -> Result<Vec<PathBuf>> {
    let files = render(name, api)?;
    if dir.exists()
        && std::fs::read_dir(dir)
            .map_err(|e| Error::io(format!("reading {}", dir.display()), e))?
            .next()
            .is_some()
    {
        return Err(Error::conflict(format!(
            "{} already exists and is not empty",
            dir.display()
        )));
    }
    let mut written = Vec::new();
    for (rel, contents) in files {
        let path = dir.join(&rel);
        fsutil::write_atomic(&path, contents.as_bytes(), false)?;
        written.push(path);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use caffeine_plugin_api::Manifest;

    #[test]
    fn renders_a_valid_manifest_for_the_new_name() {
        let files = render(
            "disk-report",
            &ApiSource::Git("https://example.invalid/x".into()),
        )
        .unwrap();
        let manifest = &files
            .iter()
            .find(|(p, _)| p == Path::new(MANIFEST_FILE))
            .unwrap()
            .1;
        let m = Manifest::from_toml(manifest).unwrap();
        assert_eq!(m.name(), "disk-report");
        assert!(m.command("disk-report-wait").is_some());
        assert_eq!(
            m.executable.as_ref().unwrap().path_for("linux"),
            "target/release/caffeine-disk-report"
        );
        assert!(m.validate().is_empty());
        let main = &files
            .iter()
            .find(|(p, _)| p == Path::new("src/main.rs"))
            .unwrap()
            .1;
        assert!(main.contains("struct DiskReport"));
        assert!(!main.contains("hello"));
        assert!(render("Bad", &ApiSource::Path(".".into())).is_err());
    }

    #[test]
    fn refuses_non_empty_directories() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("x"), "").unwrap();
        let err = create(tmp.path(), "abc", &ApiSource::Path(".".into())).unwrap_err();
        assert_eq!(err.kind(), caffeine_core::ErrorKind::Conflict);
    }
}
