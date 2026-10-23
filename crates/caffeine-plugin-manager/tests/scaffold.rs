//! The generated starter project must compile and pass its own tests.

use std::path::{Path, PathBuf};
use std::process::Command;

use caffeine_plugin_manager::scaffold::{ApiSource, create};

#[test]
fn generated_project_builds_and_tests() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("caffeine-plugin-disk-report");
    let api = Path::new(env!("CARGO_MANIFEST_DIR")).join("../caffeine-plugin-api");
    let files = create(
        &project,
        "disk-report",
        &ApiSource::Path(api.canonicalize().unwrap()),
    )
    .unwrap();
    assert_eq!(files.len(), 5);

    // A target directory of its own inside the workspace one, kept between
    // runs so dependencies are compiled only once.
    let target: PathBuf = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args(["test", "-q", "--manifest-path"])
        .arg(project.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", target.join("scaffold-test"))
        .output()
        .expect("running cargo");
    assert!(
        out.status.success(),
        "generated project failed to build:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
