//! Build-system detection.
//!
//! Detection looks only at the working directory itself (never parents or
//! subdirectories) and only for the systems below. Each detected system
//! maps to one fixed command; nothing is configured or generated.
//!
//! | Name     | Detected when the directory contains                 | Runs                        |
//! |----------|------------------------------------------------------|-----------------------------|
//! | `cargo`  | `Cargo.toml`                                         | `cargo build`               |
//! | `npm`    | `package.json` with a `build` script                 | `npm run build`             |
//! | `pnpm`   | as `npm`, plus `pnpm-lock.yaml`                      | `pnpm run build`            |
//! | `yarn`   | as `npm`, plus `yarn.lock`                           | `yarn run build`            |
//! | `make`   | `GNUmakefile`, `makefile` or `Makefile`              | `make`                      |
//! | `cmake`  | `CMakeLists.txt` and an existing `build/CMakeCache.txt` | `cmake --build build`    |
//! | `go`     | `go.mod`                                             | `go build ./...`            |
//! | `gradle` | `gradlew` (or `gradlew.bat` on Windows)              | `./gradlew build`           |
//! | `maven`  | `pom.xml`                                            | `mvn package`               |
//!
//! CMake is only detected once a build directory has been configured,
//! because configuring involves choices (generator, build type) Caffeine
//! must not make for you. Only one of npm/pnpm/yarn is reported, chosen by
//! lock file.

use std::path::Path;

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Detected {
    pub name: &'static str,
    /// The file that triggered detection.
    pub marker: String,
    pub program: String,
    pub args: Vec<String>,
}

fn found(name: &'static str, marker: &str, program: &str, args: &[&str]) -> Detected {
    Detected {
        name,
        marker: marker.to_string(),
        program: program.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
    }
}

/// Every supported system detected in `dir`, in table order.
pub fn detect(dir: &Path) -> Vec<Detected> {
    let has = |f: &str| dir.join(f).is_file();
    let mut out = Vec::new();
    if has("Cargo.toml") {
        out.push(found("cargo", "Cargo.toml", "cargo", &["build"]));
    }
    if has("package.json") && package_has_build_script(&dir.join("package.json")) {
        let (name, program) = if has("pnpm-lock.yaml") {
            ("pnpm", "pnpm")
        } else if has("yarn.lock") {
            ("yarn", "yarn")
        } else {
            ("npm", "npm")
        };
        out.push(found(name, "package.json", program, &["run", "build"]));
    }
    if let Some(m) = ["GNUmakefile", "makefile", "Makefile"]
        .into_iter()
        .find(|f| has(f))
    {
        out.push(found("make", m, "make", &[]));
    }
    if has("CMakeLists.txt") && dir.join("build").join("CMakeCache.txt").is_file() {
        out.push(found(
            "cmake",
            "CMakeLists.txt",
            "cmake",
            &["--build", "build"],
        ));
    }
    if has("go.mod") {
        out.push(found("go", "go.mod", "go", &["build", "./..."]));
    }
    let wrapper = if cfg!(windows) {
        "gradlew.bat"
    } else {
        "gradlew"
    };
    if has(wrapper) {
        out.push(found(
            "gradle",
            wrapper,
            &format!("./{wrapper}"),
            &["build"],
        ));
    }
    if has("pom.xml") {
        out.push(found("maven", "pom.xml", "mvn", &["package"]));
    }
    out
}

fn package_has_build_script(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .is_some_and(|v| v.pointer("/scripts/build").is_some_and(|s| s.is_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(dir: &Path) -> Vec<&'static str> {
        detect(dir).into_iter().map(|d| d.name).collect()
    }

    #[test]
    fn detects_only_defined_markers() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        assert!(names(d).is_empty());
        std::fs::write(d.join("Cargo.toml"), "").unwrap();
        std::fs::write(d.join("Makefile"), "").unwrap();
        std::fs::write(d.join("CMakeLists.txt"), "").unwrap();
        assert_eq!(names(d), ["cargo", "make"]);
        std::fs::create_dir(d.join("build")).unwrap();
        std::fs::write(d.join("build/CMakeCache.txt"), "").unwrap();
        assert_eq!(names(d), ["cargo", "make", "cmake"]);
    }

    #[test]
    fn javascript_requires_a_build_script_and_picks_by_lock_file() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        std::fs::write(d.join("package.json"), r#"{"scripts":{"test":"x"}}"#).unwrap();
        assert!(names(d).is_empty());
        std::fs::write(d.join("package.json"), r#"{"scripts":{"build":"tsc"}}"#).unwrap();
        assert_eq!(names(d), ["npm"]);
        std::fs::write(d.join("yarn.lock"), "").unwrap();
        assert_eq!(detect(d)[0].program, "yarn");
    }
}
