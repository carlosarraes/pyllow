//! Issue #8: the exit-code contract CI integrations depend on.
//!
//! 0 = clean analysis, 1 = analysis completed with blocking findings,
//! 2 = configuration, git, parsing, I/O, or internal failure.
//! An operational failure must never be reported as either 0 or 1 — a gate
//! that cannot tell "your code has problems" from "the tool broke" is unsafe.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;

fn pyllow_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pyllow"))
}

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(pyllow_bin())
        .args(args)
        .output()
        .expect("failed to spawn pyllow");
    (
        out.status.code().expect("process exited via signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

#[test]
fn clean_analysis_exits_zero() {
    let dir = tempdir().unwrap();
    let (code, _, _) = run(&["check", dir.path().to_str().unwrap()]);
    assert_eq!(code, 0, "a project with nothing to report must exit 0");
}

#[test]
fn blocking_findings_exit_one() {
    let dir = tempdir().unwrap();
    write(&dir.path().join("orphan.py"), "value = 1\n");
    let (code, _, _) = run(&["check", dir.path().to_str().unwrap()]);
    assert_eq!(code, 1, "an unreachable module is a blocking finding");
}

#[test]
fn unreadable_diff_file_exits_two() {
    let dir = tempdir().unwrap();
    let missing = dir.path().join("nope.diff");
    let (code, _, stderr) = run(&[
        "audit",
        dir.path().to_str().unwrap(),
        "--diff-file",
        missing.to_str().unwrap(),
    ]);
    assert_eq!(
        code, 2,
        "an unreadable --diff-file is an operational failure, not a finding (stderr: {stderr})"
    );
}

#[test]
fn malformed_diff_file_exits_two() {
    let dir = tempdir().unwrap();
    let diff = dir.path().join("bad.diff");
    write(
        &diff,
        "--- a/foo.py\n+++ b/foo.py\n@@ -1,2 +bogus @@\n+added\n",
    );
    let (code, _, stderr) = run(&[
        "audit",
        dir.path().to_str().unwrap(),
        "--diff-file",
        diff.to_str().unwrap(),
    ]);
    assert_eq!(
        code, 2,
        "a diff we cannot parse is an operational failure (stderr: {stderr})"
    );
}

#[test]
fn invalid_config_exits_two() {
    let dir = tempdir().unwrap();
    write(
        &dir.path().join("pyllow.toml"),
        "this is not = valid = toml\n",
    );
    let (code, _, stderr) = run(&["check", dir.path().to_str().unwrap()]);
    assert_eq!(
        code, 2,
        "an unparseable config is an operational failure (stderr: {stderr})"
    );
}
