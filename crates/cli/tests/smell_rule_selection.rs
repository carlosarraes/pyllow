//! Issue #1: opt-in strict smell rules.
//!
//! The configuration fixture matrix: defaults, opt-in, explicit disablement,
//! and unknown rule names.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;

fn pyllow_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pyllow"))
}

/// Source that trips both `mutable-default` and `broad-except`.
fn project(config: Option<&str>) -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("src/app.py"),
        "def build(items=[]):\n    try:\n        return items\n    except Exception:\n        return None\n",
    )
    .unwrap();
    if let Some(body) = config {
        fs::write(dir.path().join("pyllow.toml"), body).unwrap();
    }
    dir
}

fn run(root: &Path) -> (i32, Value, String) {
    let out = Command::new(pyllow_bin())
        .args(["smells", root.to_str().unwrap(), "--format", "json"])
        .output()
        .expect("failed to spawn pyllow");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let json = serde_json::from_str(&stdout).unwrap_or(Value::Null);
    (
        out.status.code().unwrap(),
        json,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn executed(json: &Value) -> Vec<String> {
    json["rules"]["executed"]
        .as_array()
        .expect("rules.executed")
        .iter()
        .map(|r| r.as_str().unwrap().to_string())
        .collect()
}

fn fired(json: &Value) -> Vec<String> {
    json["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["rule"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn defaults_run_every_shipped_rule() {
    let dir = project(None);
    let (_, json, _) = run(dir.path());
    let executed = executed(&json);
    assert!(executed.contains(&"mutable-default".to_string()));
    assert!(executed.contains(&"broad-except".to_string()));
    assert!(fired(&json).contains(&"broad-except".to_string()));
}

#[test]
fn explicit_disablement_removes_the_rule_from_output_and_findings() {
    let dir = project(Some("[smells]\ndisabled = [\"broad-except\"]\n"));
    let (_, json, _) = run(dir.path());
    assert!(
        !executed(&json).contains(&"broad-except".to_string()),
        "a disabled rule must not be reported as executed"
    );
    assert!(
        !fired(&json).contains(&"broad-except".to_string()),
        "a disabled rule must produce no findings"
    );
    // Unrelated rules keep working.
    assert!(fired(&json).contains(&"mutable-default".to_string()));
}

#[test]
fn opting_in_a_default_on_rule_is_a_harmless_no_op() {
    let dir = project(Some("[smells]\nenabled = [\"broad-except\"]\n"));
    let (_, json, _) = run(dir.path());
    assert!(executed(&json).contains(&"broad-except".to_string()));
    assert!(fired(&json).contains(&"broad-except".to_string()));
}

#[test]
fn disabled_beats_enabled_for_the_same_rule() {
    let dir = project(Some(
        "[smells]\nenabled = [\"broad-except\"]\ndisabled = [\"broad-except\"]\n",
    ));
    let (_, json, _) = run(dir.path());
    assert!(
        !executed(&json).contains(&"broad-except".to_string()),
        "turning a rule off must win over turning it on"
    );
}

#[test]
fn unknown_rule_in_disabled_fails_before_analysis() {
    let dir = project(Some("[smells]\ndisabled = [\"no-such-rule\"]\n"));
    let (code, _, stderr) = run(dir.path());
    assert_eq!(code, 2, "config errors are operational failures: {stderr}");
    assert!(
        stderr.contains("no-such-rule"),
        "error must name the offending rule: {stderr}"
    );
}

#[test]
fn unknown_rule_in_enabled_fails_before_analysis() {
    let dir = project(Some("[smells]\nenabled = [\"typo-rule\"]\n"));
    let (code, _, stderr) = run(dir.path());
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("typo-rule"), "{stderr}");
}

/// The error should help, not just reject.
#[test]
fn unknown_rule_error_lists_the_valid_names() {
    let dir = project(Some("[smells]\ndisabled = [\"no-such-rule\"]\n"));
    let (_, _, stderr) = run(dir.path());
    assert!(
        stderr.contains("mutable-default"),
        "error should list valid rule names: {stderr}"
    );
}

// #3: no-explicit-any ships disabled and turns on by name.
#[test]
fn no_explicit_any_is_off_by_default_and_opt_in() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("src/app.py"),
        "from typing import Any\n\ndef f(x: Any) -> int:\n    return 1\n",
    )
    .unwrap();
    let (code, json, _) = run(dir.path());
    assert_eq!(code, 0, "off by default");
    assert!(!executed(&json).contains(&"no-explicit-any".to_string()));

    fs::write(
        dir.path().join("pyllow.toml"),
        "[smells]\nenabled = [\"no-explicit-any\"]\n",
    )
    .unwrap();
    let (code, json, _) = run(dir.path());
    assert_eq!(code, 1);
    let d = &json["diagnostics"][0];
    assert_eq!(d["rule"], "no-explicit-any");
    assert_eq!(d["startLine"], 3);
}
