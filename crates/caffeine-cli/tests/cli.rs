//! End-to-end tests of the `caffeine` binary in an isolated CAFFEINE_HOME.
//!
//! Session tests take real OS assertions. Where the platform cannot
//! prevent idle sleep (e.g. a CI container without logind), they check
//! that `start` fails with an honest error instead.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Home {
        Home {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn config_file(&self) -> PathBuf {
        self.path().join("config").join("config.toml")
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_caffeine"));
        c.args(args)
            .env("CAFFEINE_HOME", self.path())
            .env_remove("CAFFEINE_CONFIG")
            .env("NO_COLOR", "1")
            .current_dir(self.path());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().expect("running caffeine")
    }

    fn json(&self, args: &[&str]) -> (i32, Value) {
        let mut full = vec!["--json"];
        full.extend_from_slice(args);
        let out = self.run(&full);
        let text = String::from_utf8_lossy(&out.stdout);
        let value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{args:?}: stdout is not one JSON document ({e}):\n{text}"));
        (out.status.code().unwrap_or(-1), value)
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // Never leave a session host running after a test.
        let _ = self.run(&["stop", "--force", "--timeout", "5"]);
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn idle_sleep_usable(home: &Home) -> bool {
    let (_, caps) = home.json(&["capabilities"]);
    caps["capabilities"].as_array().unwrap().iter().any(|c| {
        c["capability"] == "idle-sleep"
            && matches!(c["support"].as_str(), Some("supported" | "limited"))
    })
}

#[test]
fn version_and_help() {
    let home = Home::new();
    let out = home.run(&["--version"]);
    assert!(out.status.success());
    assert!(stdout(&out).starts_with("caffeine "));
    let out = home.run(&["--help"]);
    assert!(stdout(&out).contains("Exit codes:"));
    let out = home.run(&["definitely-not-a-command"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("unrecognized command"));
}

#[test]
fn completions_for_every_shell() {
    let home = Home::new();
    for shell in ["bash", "elvish", "fish", "powershell", "zsh"] {
        let out = home.run(&["completions", shell]);
        assert!(out.status.success(), "{shell}: {}", stderr(&out));
        assert!(stdout(&out).contains("caffeine"), "{shell}");
    }
}

#[test]
fn completions_survive_a_closed_pipe() {
    // `caffeine completions bash | head -1` must not panic.
    let home = Home::new();
    let mut child = home
        .cmd(&["completions", "bash"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    assert!(!stderr(&out).contains("panicked"), "{}", stderr(&out));
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn capabilities_report_every_capability() {
    let home = Home::new();
    let (code, v) = home.json(&["capabilities"]);
    assert_eq!(code, 0);
    let caps = v["capabilities"].as_array().unwrap();
    assert_eq!(caps.len(), 8);
    for c in caps {
        assert!(c["detail"].as_str().is_some_and(|d| !d.is_empty()), "{c}");
    }
    assert_eq!(v["platform"]["os"], std::env::consts::OS);
}

#[test]
fn status_without_session_exits_3() {
    let home = Home::new();
    let (code, v) = home.json(&["status"]);
    assert_eq!(code, 3);
    assert_eq!(v["state"], "inactive");
    let out = home.run(&["stop"]);
    assert!(out.status.success(), "stop is idempotent");
}

#[test]
fn config_round_trip_preserves_comments() {
    let home = Home::new();
    let out = home.run(&["config", "path"]);
    assert_eq!(PathBuf::from(stdout(&out).trim()), home.config_file());

    assert!(home.run(&["config", "init"]).status.success());
    let mut text = std::fs::read_to_string(home.config_file()).unwrap();
    text.push_str("\n# a comment of my own\n");
    std::fs::write(home.config_file(), text).unwrap();

    assert!(
        home.run(&["config", "set", "session.default_duration", "45m"])
            .status
            .success()
    );
    let out = home.run(&["config", "get", "session.default_duration"]);
    assert_eq!(stdout(&out).trim(), "45m");
    assert!(
        std::fs::read_to_string(home.config_file())
            .unwrap()
            .contains("a comment of my own")
    );

    let out = home.run(&["config", "set", "session.default_duration", "soon"]);
    assert_eq!(out.status.code(), Some(2));
    let out = home.run(&["config", "set", "power.turbo", "true"]);
    assert_eq!(out.status.code(), Some(9));

    let (code, v) = home.json(&["config", "validate"]);
    assert_eq!((code, v["valid"].as_bool()), (0, Some(true)));
    assert!(
        home.run(&["config", "unset", "session.default_duration"])
            .status
            .success()
    );
    let out = home.run(&["config", "get", "session.default_duration"]);
    assert_eq!(stdout(&out).trim(), "indefinite");
}

#[test]
fn broken_config_is_reported_and_never_overwritten() {
    let home = Home::new();
    std::fs::create_dir_all(home.config_file().parent().unwrap()).unwrap();
    std::fs::write(home.config_file(), "this is = = not toml").unwrap();
    let (code, _) = home.json(&["config", "validate"]);
    assert_eq!(code, 7);
    let out = home.run(&["config", "set", "general.color", "never"]);
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(
        std::fs::read_to_string(home.config_file()).unwrap(),
        "this is = = not toml"
    );
    // Status still works on defaults, with a warning.
    let out = home.run(&["status"]);
    assert!(stderr(&out).contains("warning:"));
}

#[test]
fn json_errors_are_a_single_document() {
    let home = Home::new();
    let (code, v) = home.json(&["config", "get", "nope.nope"]);
    assert_eq!(code, 9);
    assert_eq!(v["error"]["kind"], "not-found");
}

#[test]
fn built_in_build_plugin() {
    let home = Home::new();
    let (_, list) = home.json(&["plugin", "list"]);
    let build = list["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "build")
        .unwrap()
        .clone();
    assert_eq!(build["state"], "disabled");

    let out = home.run(&["build-list"]);
    assert_eq!(out.status.code(), Some(8), "disabled plugins do not run");

    assert!(home.run(&["plugin", "enable", "build"]).status.success());
    std::fs::write(home.path().join("Makefile"), "all:\n").unwrap();
    let (code, v) = home.json(&["build-list"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["detected"][0]["name"], "make");

    let (_, info) = home.json(&["plugin", "info", "build"]);
    assert_eq!(info["state"], "enabled");
    assert!(home.run(&["plugin", "disable", "build"]).status.success());
}

#[test]
fn plugin_create_and_validate() {
    let home = Home::new();
    let dir = home.path().join("my-plugin");
    let api = Path::new(env!("CARGO_MANIFEST_DIR")).join("../caffeine-plugin-api");
    let out = home.run(&[
        "plugin",
        "create",
        "word-count",
        "--path",
        dir.to_str().unwrap(),
        "--api-path",
        api.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(dir.join("caffeine-plugin.toml").is_file());
    // Not built yet: the manifest is fine but the executable is missing.
    let (code, v) = home.json(&["plugin", "validate", dir.to_str().unwrap()]);
    assert_eq!(code, 8);
    assert!(
        v["report"]["problems"]
            .to_string()
            .contains("has the plugin been built")
    );
    let out = home.run(&[
        "plugin",
        "create",
        "word-count",
        "--path",
        dir.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(6), "refuses to overwrite");
}

#[test]
fn lid_reports_or_says_unsupported() {
    let home = Home::new();
    let (code, v) = home.json(&["lid"]);
    match code {
        0 => assert!(
            v.get("lid").is_some() || v.get("policy").is_some() || v.get("lid_error").is_some()
        ),
        4 => assert_eq!(v["error"]["kind"], "unsupported"),
        other => panic!("unexpected exit {other}: {v}"),
    }
    // Persistent changes are never made without confirmation.
    let out = home
        .cmd(&["lid", "set", "--ac", "sleep"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_ne!(out.status.code(), Some(0));
}

#[test]
fn doctor_produces_checks() {
    let home = Home::new();
    let (code, v) = home.json(&["doctor"]);
    assert!(code == 0 || code == 1, "{code}");
    assert!(!v["checks"].as_array().unwrap().is_empty());
    assert_eq!(v["ok"].as_bool(), Some(code == 0));
}

#[test]
fn session_lifecycle() {
    let home = Home::new();
    let usable = idle_sleep_usable(&home);
    let (code, v) = home.json(&["start", "--duration", "10m"]);
    if code != 0 {
        // Unsupported, or (e.g. a CI runner without an active seat) refused
        // by the OS: it must fail with an honest error and hold nothing.
        assert!(matches!(code, 4 | 5), "start must fail honestly: {v}");
        assert!(
            !usable || code == 5,
            "capabilities said idle-sleep is usable: {v}"
        );
        let (code, status) = home.json(&["status"]);
        assert_eq!((code, status["state"].as_str()), (3, Some("inactive")));
        return;
    }
    assert_eq!(v["result"], "started");
    let id = v["session"]["id"].as_str().unwrap().to_string();

    let (code, v) = home.json(&["start", "--duration", "10m"]);
    assert_eq!((code, v["result"].as_str()), (0, Some("already-active")));
    let (code, _) = home.json(&["start", "--duration", "20m"]);
    assert_eq!(code, 6, "different settings conflict by default");

    let (code, v) = home.json(&["status"]);
    assert_eq!(code, 0);
    assert_eq!(v["session"]["id"], id.as_str());
    assert!(v["remaining_secs"].as_u64().unwrap() <= 600);

    let (code, v) = home.json(&["stop"]);
    assert_eq!((code, v["stopped"].as_bool()), (0, Some(true)));
    let (code, v) = home.json(&["status"]);
    assert_eq!(code, 3);
    assert_eq!(v["last_session"]["end"]["reason"], "stopped");
}

#[test]
fn run_passes_through_the_exit_code() {
    let home = Home::new();
    let exe = std::env::current_exe().unwrap();
    // Re-run this test binary with a filter that matches nothing: exits 0.
    let args = [exe.to_str().unwrap(), "--exact", "no-such-test"];
    let mut full = vec!["-q", "run", "--"];
    full.extend_from_slice(&args);
    let out = home.run(&full);
    if matches!(out.status.code(), Some(4 | 5)) {
        // No assertion available here; the command must not have run as if
        // it were protected. (See session_lifecycle.)
        return;
    }
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let (_, v) = home.json(&["status"]);
    assert_eq!(v["last_session"]["end"]["reason"], "command-exited");

    let out = home.run(&["-q", "run", "--", "definitely-not-a-program-xyz"]);
    assert_ne!(out.status.code(), Some(0));
}
