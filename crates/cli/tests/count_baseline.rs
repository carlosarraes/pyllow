//! Issue #7 end-to-end: the strict count-baseline gate.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;

fn pyllow_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pyllow"))
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Project with exactly two broad-except findings.
fn project() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("src/app.py"),
        "def a():\n    try:\n        pass\n    except Exception:\n        pass\n\ndef b():\n    try:\n        pass\n    except Exception:\n        pass\n",
    )
    .unwrap();
    dir
}

fn smells(root: &Path, extra: &[&str]) -> (i32, String) {
    let mut args = vec!["smells", root.to_str().unwrap()];
    args.extend_from_slice(extra);
    let out = Command::new(pyllow_bin()).args(&args).output().unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn write_counts(root: &Path, n: u64) -> String {
    let p = root.join("counts.json");
    fs::write(
        &p,
        format!("{{\"schemaVersion\": 1, \"counts\": {{\"broad-except\": {n}}}}}\n"),
    )
    .unwrap();
    p.to_str().unwrap().to_string()
}

#[test]
fn equal_counts_pass() {
    let dir = project();
    let p = write_counts(dir.path(), 2);
    let (code, stderr) = smells(dir.path(), &["--count-baseline", &p]);
    assert_eq!(code, 1, "smell findings still exit 1: {stderr}");
    assert!(!stderr.contains("count-baseline"), "{stderr}");
}

#[test]
fn increase_is_a_regression() {
    let dir = project();
    let p = write_counts(dir.path(), 1);
    let (code, stderr) = smells(dir.path(), &["--count-baseline", &p]);
    assert_eq!(code, 1);
    assert!(stderr.contains("regression") && stderr.contains("broad-except"), "{stderr}");
}

#[test]
fn decrease_is_stale_and_prints_the_exact_lower_value() {
    let dir = project();
    let p = write_counts(dir.path(), 5);
    let (_, stderr) = smells(dir.path(), &["--count-baseline", &p]);
    assert!(stderr.contains("stale") && stderr.contains("exactly 2"), "{stderr}");
}

// The gate must fail the run even when the finding list alone would pass.
#[test]
fn stale_allowance_fails_even_with_zero_findings() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/app.py"), "def f(x):\n    return x\n").unwrap();
    let p = write_counts(dir.path(), 3);
    let (code, stderr) = smells(dir.path(), &["--count-baseline", &p]);
    assert_eq!(code, 1, "stale baseline must fail: {stderr}");
}

#[test]
fn save_writes_exact_current_counts() {
    let dir = project();
    let p = dir.path().join("counts.json");
    let (_, _) = smells(dir.path(), &["--save-count-baseline", p.to_str().unwrap()]);
    let saved: serde_json::Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(saved["schemaVersion"], 1);
    assert_eq!(saved["counts"]["broad-except"], 2);
    // A follow-up check against the fresh file passes.
    let (_, stderr) = smells(dir.path(), &["--count-baseline", p.to_str().unwrap()]);
    assert!(!stderr.contains("count-baseline"), "{stderr}");
}

#[test]
fn malformed_baseline_is_an_operational_error() {
    let dir = project();
    let p = dir.path().join("counts.json");
    fs::write(&p, "{\"schemaVersion\": 1, \"counts\": {\"broad-except\": true}}").unwrap();
    let (code, stderr) = smells(dir.path(), &["--count-baseline", p.to_str().unwrap()]);
    assert_eq!(code, 2, "{stderr}");
}

#[test]
fn inflated_branch_baseline_fails_against_merge_base() {
    let dir = project();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    let p = write_counts(dir.path(), 2);
    git(dir.path(), &["add", "-A", "-f"]);
    git(dir.path(), &["commit", "-q", "-m", "baseline 2"]);
    git(dir.path(), &["checkout", "-q", "-b", "feature"]);
    // Sneak the allowance up on the branch.
    write_counts(dir.path(), 10);
    git(dir.path(), &["add", "-A", "-f"]);
    git(dir.path(), &["commit", "-q", "-m", "inflate"]);
    let (code, stderr) = smells(dir.path(), &["--count-baseline", &p, "--count-base", "main"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("inflated") && stderr.contains("merge-base"), "{stderr}");
}

#[test]
fn honest_branch_baseline_passes_the_ratchet() {
    let dir = project();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    let p = write_counts(dir.path(), 2);
    git(dir.path(), &["add", "-A", "-f"]);
    git(dir.path(), &["commit", "-q", "-m", "baseline 2"]);
    git(dir.path(), &["checkout", "-q", "-b", "feature"]);
    let (code, stderr) = smells(dir.path(), &["--count-baseline", &p, "--count-base", "main"]);
    assert_eq!(code, 1, "findings exit, not gate: {stderr}");
    assert!(!stderr.contains("inflated"), "{stderr}");
}

#[test]
fn invalid_base_ref_is_an_operational_error() {
    let dir = project();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    let p = write_counts(dir.path(), 2);
    git(dir.path(), &["add", "-A", "-f"]);
    git(dir.path(), &["commit", "-q", "-m", "c"]);
    let (code, stderr) = smells(dir.path(), &["--count-baseline", &p, "--count-base", "no-such-ref"]);
    assert_eq!(code, 2, "{stderr}");
}

// "Interrupted analysis": an unparseable file means the counts are not
// trustworthy; the run must not report the gate as cleanly passed.
#[test]
fn unparseable_file_cannot_yield_a_clean_gate() {
    let dir = project();
    fs::write(dir.path().join("src/broken.py"), "def f(:\n").unwrap();
    let p = write_counts(dir.path(), 2);
    let (code, _) = smells(dir.path(), &["--count-baseline", &p]);
    assert_ne!(code, 0);
}
