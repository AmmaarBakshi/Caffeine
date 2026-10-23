//! The built-in `build` plugin.
//!
//! `caffeine build [TASK] [ARGS...]` runs a build task while the host keeps
//! the system awake, and `caffeine build-list` shows what can be run.
//!
//! Tasks are explicit: each names a program and its arguments, which are
//! executed directly (never through a shell) in a stated working directory
//! with stated extra environment variables:
//!
//! ```toml
//! [plugins.settings.build]
//! default_task = "release"
//!
//! [plugins.settings.build.tasks.release]
//! program = "cargo"
//! args = ["build", "--release"]
//! cwd = "."                  # relative to where caffeine is run; optional
//! env = { RUSTFLAGS = "-D warnings" }
//! description = "Optimized build"
//! ```
//!
//! When no task is configured, the plugin can fall back to the build
//! systems listed in [`detect`], each with one fixed command. Extra
//! arguments after the task name are appended to its arguments.

pub mod detect;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::json;

use caffeine_plugin_api::plugin::Initialize;
use caffeine_plugin_api::{
    Check, CheckStatus, Host, Invocation, LogLevel, Manifest, Plugin, PluginError, is_valid_name,
};

const MANIFEST: &str = r#"
[plugin]
name = "build"
version = "@VERSION@"
description = "Run configured build tasks (or a detected build system) while keeping the system awake"
api = "^1.0"
permissions = ["process.spawn", "power.inhibit"]

[[commands]]
name = "build"
summary = "Run a build task while the system is kept awake"
usage = "build [TASK] [ARGS]..."
keep_awake = true

[[commands]]
name = "build-list"
summary = "List configured tasks and detected build systems"
usage = "build-list"

[[settings]]
key = "tasks"
type = "table"
description = "Named tasks: [plugins.settings.build.tasks.<name>] with program, args, cwd, env, description"

[[settings]]
key = "default_task"
type = "string"
default = ""
description = "Task run by a bare `caffeine build` (empty: the only task, or the only detected system)"

[[settings]]
key = "detect"
type = "bool"
default = true
description = "Offer detected build systems (cargo, npm, make, ...) as tasks"
"#;

/// The plugin's manifest.
pub fn manifest() -> Manifest {
    Manifest::from_toml(&MANIFEST.replace("@VERSION@", env!("CARGO_PKG_VERSION")))
        .expect("built-in manifest is valid")
}

/// Factory used to register the plugin with the plugin manager.
pub fn new() -> Box<dyn Plugin> {
    Box::new(BuildPlugin::default())
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskConfig {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Default)]
pub struct BuildPlugin {
    tasks: BTreeMap<String, TaskConfig>,
    default_task: String,
    detect: bool,
}

/// A task ready to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Resolved {
    name: String,
    source: &'static str,
    program: String,
    args: Vec<String>,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
}

impl BuildPlugin {
    fn configured(&self, name: &str, cwd: &Path, extra: &[String]) -> Option<Resolved> {
        self.tasks.get(name).map(|t| Resolved {
            name: name.to_string(),
            source: "configured",
            program: t.program.clone(),
            args: t.args.iter().chain(extra).cloned().collect(),
            cwd: t
                .cwd
                .as_ref()
                .map_or_else(|| cwd.to_path_buf(), |c| cwd.join(c)),
            env: t.env.clone(),
        })
    }

    fn detected(&self, cwd: &Path) -> Vec<detect::Detected> {
        if self.detect {
            detect::detect(cwd)
        } else {
            Vec::new()
        }
    }

    /// Pick the task for `caffeine build ARGS`.
    fn select(&self, args: &[String], cwd: &Path) -> Result<Resolved, PluginError> {
        let args: Vec<String> = match args.first().map(String::as_str) {
            Some("--") => args[1..].to_vec(),
            _ => args.to_vec(),
        };
        let detected = self.detected(cwd);
        let from_detected = |d: &detect::Detected, extra: &[String]| Resolved {
            name: d.name.to_string(),
            source: "detected",
            program: d.program.clone(),
            args: d.args.iter().chain(extra).cloned().collect(),
            cwd: cwd.to_path_buf(),
            env: BTreeMap::new(),
        };
        if let Some(first) = args.first().filter(|a| !a.starts_with('-')) {
            if let Some(task) = self.configured(first, cwd, &args[1..]) {
                return Ok(task);
            }
            if let Some(d) = detected.iter().find(|d| d.name == first) {
                return Ok(from_detected(d, &args[1..]));
            }
            return Err(PluginError::new(format!(
                "no task or detected build system named \"{first}\""
            ))
            .with_hint(self.choices_hint(&detected)));
        }
        if !self.default_task.is_empty() {
            return self
                .configured(&self.default_task, cwd, &args)
                .ok_or_else(|| {
                    PluginError::new(format!(
                        "default_task \"{}\" is not a configured task",
                        self.default_task
                    ))
                    .with_hint("fix plugins.settings.build.default_task")
                });
        }
        match (self.tasks.len(), detected.len()) {
            (1, _) => {
                let name = self.tasks.keys().next().expect("one task");
                Ok(self.configured(name, cwd, &args).expect("task exists"))
            }
            (0, 1) => Ok(from_detected(&detected[0], &args)),
            (0, 0) => Err(PluginError::new(format!(
                "nothing to build in {}: no task is configured and no supported build system was found",
                cwd.display()
            ))
            .with_hint("configure one under [plugins.settings.build.tasks.<name>]; see `caffeine plugin info build`")),
            _ => Err(PluginError::new("several tasks are available; choose one")
                .with_hint(self.choices_hint(&detected))),
        }
    }

    fn choices_hint(&self, detected: &[detect::Detected]) -> String {
        let mut names: Vec<String> = self.tasks.keys().cloned().collect();
        names.extend(
            detected
                .iter()
                .filter(|d| !self.tasks.contains_key(d.name))
                .map(|d| d.name.to_string()),
        );
        if names.is_empty() {
            "run `caffeine build-list`".into()
        } else {
            format!(
                "available: {} (see `caffeine build-list`)",
                names.join(", ")
            )
        }
    }

    fn run(&self, inv: &Invocation, host: &mut dyn Host) -> Result<i32, PluginError> {
        let task = self.select(&inv.args, &inv.cwd)?;
        let program = resolve_program(&task.program, &task.cwd)?;
        let line = command_line(&task.program, &task.args);
        host.print(&format!("Running `{line}` in {}", task.cwd.display()));
        host.log(
            LogLevel::Info,
            &format!(
                "build task {} ({}): {}",
                task.name,
                task.source,
                program.display()
            ),
        );
        let started = Instant::now();
        let mut cmd = Command::new(&program);
        cmd.args(&task.args)
            .current_dir(&task.cwd)
            .envs(&task.env)
            .stdin(Stdio::inherit())
            .stderr(Stdio::inherit());
        // Keep machine-readable output on stdout clean.
        cmd.stdout(if inv.json {
            Stdio::from(std::io::stderr())
        } else {
            Stdio::inherit()
        });
        let status = cmd
            .status()
            .map_err(|e| PluginError::new(format!("could not start `{line}`: {e}")))?;
        let elapsed = started.elapsed().as_secs_f64();
        let code = status.code();
        host.data(json!({
            "task": task.name,
            "source": task.source,
            "program": task.program,
            "args": task.args,
            "cwd": task.cwd,
            "exit_code": code,
            "success": status.success(),
            "duration_secs": (elapsed * 10.0).round() / 10.0,
        }));
        if status.success() {
            host.print(&format!("Task {} finished in {elapsed:.1}s", task.name));
        } else {
            host.print(&format!(
                "Task {} failed ({status}) after {elapsed:.1}s",
                task.name
            ));
        }
        Ok(code.unwrap_or(1))
    }

    fn list(&self, inv: &Invocation, host: &mut dyn Host) -> Result<i32, PluginError> {
        let detected = self.detected(&inv.cwd);
        let configured: Vec<_> = self
            .tasks
            .iter()
            .map(|(name, t)| {
                json!({
                    "name": name,
                    "command": command_line(&t.program, &t.args),
                    "cwd": t.cwd,
                    "description": t.description,
                    "default": *name == self.default_task,
                })
            })
            .collect();
        host.data(json!({
            "cwd": inv.cwd,
            "configured": configured,
            "detected": detected,
            "detection_enabled": self.detect,
        }));
        if self.tasks.is_empty() {
            host.print("No tasks configured.");
        } else {
            host.print("Configured tasks:");
            for (name, t) in &self.tasks {
                let default = if *name == self.default_task {
                    " (default)"
                } else {
                    ""
                };
                host.print(&format!(
                    "  {name:<14} {}{default}",
                    command_line(&t.program, &t.args)
                ));
                if let Some(d) = &t.description {
                    host.print(&format!("  {:<14} {d}", ""));
                }
            }
        }
        if !self.detect {
            host.print("Build-system detection is disabled (plugins.settings.build.detect).");
        } else if detected.is_empty() {
            host.print(&format!(
                "No supported build system detected in {}.",
                inv.cwd.display()
            ));
        } else {
            host.print(&format!("Detected in {}:", inv.cwd.display()));
            for d in &detected {
                let shadow = if self.tasks.contains_key(d.name) {
                    " (shadowed by a configured task)"
                } else {
                    ""
                };
                host.print(&format!(
                    "  {:<14} {}  [{}]{shadow}",
                    d.name,
                    command_line(&d.program, &d.args),
                    d.marker
                ));
            }
        }
        Ok(0)
    }
}

impl Plugin for BuildPlugin {
    fn manifest(&self) -> Manifest {
        manifest()
    }

    fn initialize(&mut self, init: &Initialize) -> Result<(), PluginError> {
        let s = &init.settings;
        self.detect = s.get("detect").and_then(|v| v.as_bool()).unwrap_or(true);
        self.default_task = s
            .get("default_task")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        self.tasks = match s.get("tasks") {
            None => BTreeMap::new(),
            Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
                PluginError::new(format!("invalid plugins.settings.build.tasks: {e}"))
                    .with_hint("each task needs `program`; optional: args, cwd, env, description")
            })?,
        };
        for (name, task) in &self.tasks {
            if !is_valid_name(name) {
                return Err(PluginError::new(format!(
                    "invalid task name \"{name}\": use lowercase letters, digits and hyphens"
                )));
            }
            if task.program.trim().is_empty() {
                return Err(PluginError::new(format!(
                    "task \"{name}\" has an empty program"
                )));
            }
        }
        Ok(())
    }

    fn invoke(&mut self, inv: &Invocation, host: &mut dyn Host) -> Result<i32, PluginError> {
        match inv.command.as_str() {
            "build" => self.run(inv, host),
            "build-list" => self.list(inv, host),
            other => Err(PluginError::new(format!("unknown command {other}"))),
        }
    }

    fn diagnose(&mut self) -> Vec<Check> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut checks = Vec::new();
        for (name, task) in &self.tasks {
            let dir = task
                .cwd
                .as_ref()
                .map_or_else(|| cwd.clone(), |c| cwd.join(c));
            checks.push(match resolve_program(&task.program, &dir) {
                Ok(p) => Check::new(
                    format!("build.task.{name}"),
                    CheckStatus::Ok,
                    format!("task {name}: {} found at {}", task.program, p.display()),
                ),
                Err(e) => Check::new(
                    format!("build.task.{name}"),
                    CheckStatus::Fail,
                    format!("task {name}: {e}"),
                )
                .with_hint(e.hint.unwrap_or_default()),
            });
        }
        if !self.default_task.is_empty() && !self.tasks.contains_key(&self.default_task) {
            checks.push(
                Check::new(
                    "build.default_task",
                    CheckStatus::Fail,
                    format!(
                        "default_task \"{}\" is not a configured task",
                        self.default_task
                    ),
                )
                .with_hint("fix plugins.settings.build.default_task"),
            );
        }
        if self.tasks.is_empty() {
            checks.push(Check::new(
                "build.tasks",
                CheckStatus::Info,
                "no build tasks configured; detected build systems will be used",
            ));
        }
        checks
    }
}

/// Quote arguments for display only; execution never goes through a shell.
fn command_line(program: &str, args: &[String]) -> String {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(|a| {
            if a.is_empty() || a.contains(char::is_whitespace) || a.contains('"') {
                format!("\"{}\"", a.replace('"', "\\\""))
            } else {
                a.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Find `program`: a path (relative to `cwd`) if it contains a separator,
/// otherwise a `PATH` lookup that also tries `PATHEXT` extensions on
/// Windows, where tools such as `npm` are `.cmd` files.
fn resolve_program(program: &str, cwd: &Path) -> Result<PathBuf, PluginError> {
    if program.contains('/') || program.contains('\\') {
        let p = cwd.join(program);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(PluginError::new(format!("{} does not exist", p.display())))
        };
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let exts: Vec<OsString> = if cfg!(windows) && Path::new(program).extension().is_none() {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(OsString::from)
            .collect()
    } else {
        vec![OsString::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let mut name = OsString::from(program);
            name.push(ext);
            let candidate = dir.join(&name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(
        PluginError::new(format!("`{program}` was not found in PATH"))
            .with_hint("install it, or give the task an explicit program path"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use caffeine_plugin_api::plugin::HostInfo;
    use serde_json::{Map, Value};

    fn plugin(settings: Value) -> BuildPlugin {
        let mut p = BuildPlugin::default();
        let Value::Object(settings) = settings else {
            panic!()
        };
        p.initialize(&Initialize {
            host: HostInfo::current(semver::Version::new(0, 1, 0)),
            settings,
            permissions: vec![],
            data_dir: std::env::temp_dir(),
        })
        .unwrap();
        p
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn manifest_is_valid() {
        assert!(
            manifest().validate().is_empty(),
            "{:?}",
            manifest().validate()
        );
    }

    #[test]
    fn selection_rules() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let empty = plugin(json!({ "detect": true }));
        let err = empty.select(&[], cwd).unwrap_err();
        assert!(err.message.contains("nothing to build"));

        std::fs::write(cwd.join("Cargo.toml"), "").unwrap();
        let t = empty.select(&args(&["--", "--release"]), cwd).unwrap();
        assert_eq!(
            (t.program.as_str(), t.args.clone()),
            ("cargo", args(&["build", "--release"]))
        );
        assert_eq!(t.source, "detected");

        let configured = plugin(json!({
            "tasks": {
                "docs": { "program": "cargo", "args": ["doc"], "cwd": "sub" },
                "lint": { "program": "cargo", "args": ["clippy"] }
            }
        }));
        assert!(
            configured
                .select(&[], cwd)
                .unwrap_err()
                .message
                .contains("several")
        );
        let t = configured.select(&args(&["docs", "--open"]), cwd).unwrap();
        assert_eq!(t.args, args(&["doc", "--open"]));
        assert_eq!(t.cwd, cwd.join("sub"));
        assert_eq!(
            configured.select(&args(&["cargo"]), cwd).unwrap().source,
            "detected"
        );
        assert!(configured.select(&args(&["deploy"]), cwd).is_err());

        let no_detect = plugin(json!({ "detect": false }));
        assert!(no_detect.select(&args(&["cargo"]), cwd).is_err());

        let with_default = plugin(json!({
            "default_task": "lint",
            "tasks": { "lint": { "program": "cargo", "args": ["clippy"] }, "x": { "program": "y" } }
        }));
        assert_eq!(with_default.select(&[], cwd).unwrap().name, "lint");
    }

    #[test]
    fn invalid_tasks_fail_initialization() {
        let mut p = BuildPlugin::default();
        let init = |settings: Value| Initialize {
            host: HostInfo::current(semver::Version::new(0, 1, 0)),
            settings: match settings {
                Value::Object(m) => m,
                _ => Map::new(),
            },
            permissions: vec![],
            data_dir: std::env::temp_dir(),
        };
        assert!(
            p.initialize(&init(json!({ "tasks": { "a": { "args": [] } } })))
                .is_err()
        );
        assert!(
            p.initialize(&init(
                json!({ "tasks": { "a": { "program": "x", "shell": true } } })
            ))
            .is_err()
        );
        assert!(
            p.initialize(&init(
                json!({ "tasks": { "Bad Name": { "program": "x" } } })
            ))
            .is_err()
        );
    }

    #[test]
    fn program_resolution() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(resolve_program("definitely-not-a-real-program-xyz", tmp.path()).is_err());
        std::fs::write(tmp.path().join("tool"), "").unwrap();
        assert!(resolve_program("./tool", tmp.path()).is_ok());
        assert!(resolve_program("./missing", tmp.path()).is_err());
        let cargo = resolve_program("cargo", tmp.path()).expect("cargo is on PATH while testing");
        assert!(cargo.is_file());
    }

    #[test]
    fn display_quoting() {
        assert_eq!(command_line("make", &args(&["-j", "4"])), "make -j 4");
        assert_eq!(
            command_line("echo", &args(&["a b", ""])),
            "echo \"a b\" \"\""
        );
    }

    #[derive(Default)]
    struct Collect(Vec<String>, Vec<Value>);
    impl Host for Collect {
        fn log(&mut self, _: LogLevel, _: &str) {}
        fn print(&mut self, t: &str) {
            self.0.push(t.into());
        }
        fn data(&mut self, v: Value) {
            self.1.push(v);
        }
    }

    #[test]
    fn runs_a_task_and_reports_its_exit_code() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = plugin(json!({
            "tasks": {
                "ok": { "program": "cargo", "args": ["--version"] },
                "bad": { "program": "cargo", "args": ["no-such-subcommand-xyz"] }
            }
        }));
        let inv = |a: &[&str]| Invocation {
            command: "build".into(),
            args: args(a),
            cwd: tmp.path().to_path_buf(),
            json: true,
        };
        let mut host = Collect::default();
        assert_eq!(p.invoke(&inv(&["ok"]), &mut host).unwrap(), 0);
        assert_eq!(host.1[0]["success"], true);
        let mut host = Collect::default();
        assert_ne!(p.invoke(&inv(&["bad"]), &mut host).unwrap(), 0);
        assert_eq!(host.1[0]["success"], false);
    }
}
