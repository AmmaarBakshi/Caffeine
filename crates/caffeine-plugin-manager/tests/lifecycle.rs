//! End-to-end tests against the real `caffeine-hello` example plugin.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use serde_json::Value;

use caffeine_core::ErrorKind;
use caffeine_plugin_api::{Host, LogLevel, MANIFEST_FILE};
use caffeine_plugin_manager::{Manager, ManagerOptions, Origin, RunRequest, TrustStatus};

/// Build the example plugin once and return its executable.
fn hello_binary() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let out = Command::new(cargo)
            .args([
                "build",
                "-q",
                "-p",
                "caffeine-plugin-hello",
                "--message-format=json",
            ])
            .output()
            .expect("running cargo");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| {
                v.get("executable")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
            })
            .next_back()
            .expect("cargo reported the plugin executable")
    })
}

/// A plugin source directory laid out like the template project.
fn plugin_source(root: &Path) -> PathBuf {
    let src = root.join("hello-src");
    let manifest = include_str!("../../caffeine-plugin-hello/caffeine-plugin.toml");
    let exe_rel = if cfg!(windows) {
        "target/release/caffeine-hello.exe"
    } else {
        "target/release/caffeine-hello"
    };
    std::fs::create_dir_all(src.join("target/release")).unwrap();
    std::fs::write(src.join(MANIFEST_FILE), manifest).unwrap();
    std::fs::copy(hello_binary(), src.join(exe_rel)).unwrap();
    std::fs::write(src.join("README.md"), "hello").unwrap();
    src
}

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    src: PathBuf,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let src = plugin_source(&root);
    Env {
        _tmp: tmp,
        root,
        src,
    }
}

fn options(env: &Env, search: bool, enabled: &[&str]) -> ManagerOptions {
    ManagerOptions {
        plugin_dir: env.root.join("data/plugins"),
        data_dir: env.root.join("data/plugin-data"),
        search_paths: if search {
            vec![env.src.clone()]
        } else {
            Vec::new()
        },
        enabled: enabled.iter().map(|s| s.to_string()).collect(),
        settings: BTreeMap::new(),
        host_version: semver::Version::new(0, 1, 0),
        reserved_commands: vec!["start".into(), "stop".into()],
        builtins: Vec::new(),
    }
}

#[derive(Default)]
struct TestHost {
    printed: Vec<String>,
    data: Vec<Value>,
    logs: Vec<(LogLevel, String)>,
}

impl Host for TestHost {
    fn log(&mut self, level: LogLevel, message: &str) {
        self.logs.push((level, message.to_string()));
    }
    fn print(&mut self, text: &str) {
        self.printed.push(text.to_string());
    }
    fn data(&mut self, value: Value) {
        self.data.push(value);
    }
}

fn request(command: &str, args: &[&str]) -> RunRequest {
    RunRequest {
        command: command.into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        cwd: std::env::temp_dir(),
        json: false,
    }
}

#[test]
fn external_plugin_requires_enable_then_runs() {
    let env = env();
    let mut m = Manager::discover(options(&env, true, &[])).unwrap();
    let entry = m.get("hello").expect("discovered");
    assert_eq!(entry.origin, Origin::External);
    assert_eq!(entry.trust, TrustStatus::NotTrusted);
    assert!(!entry.enabled);
    assert!(!entry.has_errors(), "{:?}", entry.problems);

    let err = m
        .run("hello", &request("hello", &[]), &mut TestHost::default())
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Plugin);
    assert!(err.message().contains("not enabled"));

    let enabled = m.enable("hello").unwrap();
    assert_eq!(enabled, ["hello"]);
    assert_eq!(m.get("hello").unwrap().trust, TrustStatus::Trusted);

    let mut host = TestHost::default();
    let code = m
        .run("hello", &request("hello", &["Ada", "Linus"]), &mut host)
        .unwrap();
    assert_eq!(code, 0);
    assert_eq!(host.printed, ["Hi, Ada!", "Hi, Linus!"]);
    assert_eq!(host.data[0]["names"][1], "Linus");

    // A fresh manager (next caffeine invocation) sees the trust record.
    let m2 = Manager::discover(options(&env, true, &["hello"])).unwrap();
    assert_eq!(m2.get("hello").unwrap().state(), "enabled");
    let checks = m2.diagnose("hello").unwrap();
    assert_eq!(checks[0].id, "hello.greeting");
}

#[test]
fn plugin_errors_and_unknown_commands_are_reported() {
    let env = env();
    let mut m = Manager::discover(options(&env, true, &[])).unwrap();
    m.enable("hello").unwrap();
    let err = m
        .run(
            "hello",
            &request("hello-wait", &["soon"]),
            &mut TestHost::default(),
        )
        .unwrap_err();
    assert!(err.message().contains("whole number"), "{err}");
    let err = m
        .run("hello", &request("nope", &[]), &mut TestHost::default())
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage);
}

#[test]
fn modified_executable_is_refused() {
    let env = env();
    let mut m = Manager::discover(options(&env, true, &[])).unwrap();
    m.enable("hello").unwrap();
    let exe = m.get("hello").unwrap().executable.clone().unwrap();
    let mut bytes = std::fs::read(&exe).unwrap();
    bytes.extend_from_slice(b"tampered");
    std::fs::write(&exe, bytes).unwrap();

    let m = Manager::discover(options(&env, true, &["hello"])).unwrap();
    assert_eq!(m.get("hello").unwrap().trust, TrustStatus::Changed);
    let err = m
        .run("hello", &request("hello", &[]), &mut TestHost::default())
        .unwrap_err();
    assert!(
        err.message().contains("changed since you enabled it"),
        "{err}"
    );
}

#[test]
fn install_upgrade_and_remove() {
    let env = env();
    let mut m = Manager::discover(options(&env, false, &[])).unwrap();
    assert!(m.get("hello").is_none());

    let report = m.install(&env.src, false).unwrap();
    assert_eq!(report.name, "hello");
    assert!(report.dir.join(MANIFEST_FILE).is_file());
    assert!(report.dir.join("README.md").is_file());
    let entry = m.get("hello").unwrap();
    assert_eq!(entry.origin, Origin::Installed);
    // Installing never enables or trusts.
    assert!(!entry.enabled);
    assert_eq!(entry.trust, TrustStatus::NotTrusted);

    let err = m.install(&env.src, false).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict);

    m.enable("hello").unwrap();
    let upgraded = m.install(&env.src, true).unwrap();
    assert_eq!(upgraded.replaced, Some(semver::Version::new(0, 1, 0)));
    // Identical files: trust still holds.
    assert!(!upgraded.needs_reenable);

    let dir = m.remove("hello").unwrap();
    assert!(!dir.exists());
    assert!(m.get("hello").is_none());
}

#[test]
fn settings_are_validated_and_passed() {
    let env = env();
    let mut opts = options(&env, true, &[]);
    opts.settings.insert(
        "hello".into(),
        serde_json::json!({ "greeting": "Ahoy", "colour": "red" }),
    );
    let mut m = Manager::discover(opts).unwrap();
    let entry = m.get("hello").unwrap();
    assert!(!entry.has_errors());
    assert!(entry.problems.iter().any(|p| p.message.contains("colour")));
    m.enable("hello").unwrap();
    let mut host = TestHost::default();
    m.run("hello", &request("hello", &["you"]), &mut host)
        .unwrap();
    assert_eq!(host.printed, ["Ahoy, you!"]);

    let mut opts = options(&env, true, &[]);
    opts.settings
        .insert("hello".into(), serde_json::json!({ "greeting": 5 }));
    let mut m = Manager::discover(opts).unwrap();
    assert!(m.get("hello").unwrap().has_errors());
    assert_eq!(m.enable("hello").unwrap_err().kind(), ErrorKind::Plugin);
}

#[test]
fn broken_and_colliding_plugins_are_reported() {
    let env = env();
    let broken = env.root.join("plugins-extra/broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join(MANIFEST_FILE), "[plugin]\nname = 3\n").unwrap();

    let mut opts = options(&env, true, &[]);
    opts.search_paths.push(env.root.join("plugins-extra"));
    opts.reserved_commands.push("hello".into());
    let m = Manager::discover(opts).unwrap();
    assert_eq!(m.broken().len(), 1);
    let entry = m.get("hello").unwrap();
    assert!(
        entry
            .problems
            .iter()
            .any(|p| p.is_error() && p.message.contains("collides"))
    );
    assert_eq!(entry.state(), "unusable");
}

#[test]
fn validate_reports_missing_executable() {
    let env = env();
    let exe = if cfg!(windows) {
        "target/release/caffeine-hello.exe"
    } else {
        "target/release/caffeine-hello"
    };
    std::fs::remove_file(env.src.join(exe)).unwrap();
    let report = Manager::discover(options(&env, false, &[]))
        .unwrap()
        .validate(&env.src);
    assert!(!report.is_valid());
    assert!(
        report
            .problems
            .iter()
            .any(|p| p.message.contains("has the plugin been built"))
    );
}
