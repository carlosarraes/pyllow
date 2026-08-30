//! Issue #8: the documented, versioned machine-output contract.
//!
//! CI integrations must be able to depend on these fields rather than on
//! whatever `AnalysisResults` happens to serialize to.

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::tempdir;

fn pyllow_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pyllow"))
}

/// A project with one unreachable module that also has an unused import,
/// giving both a file-level and a line-level diagnostic.
fn probe_project() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/orphan.py"), "import os\n\nvalue = 1\n").unwrap();
    dir
}

/// A project with a line-level smell at a known line.
fn smell_project() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("src/app.py"),
        "# leading comment\ndef build(items=[]):\n    return items\n",
    )
    .unwrap();
    dir
}

fn run_json(args: &[&str]) -> (Value, String) {
    let out = Command::new(pyllow_bin())
        .args(args)
        .output()
        .expect("failed to spawn pyllow");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let parsed = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout must be pure JSON:\n{stdout}\nerror: {e}"));
    (parsed, String::from_utf8_lossy(&out.stderr).into_owned())
}

fn check_json(root: &std::path::Path) -> (Value, String) {
    let out = Command::new(pyllow_bin())
        .args(["check", root.to_str().unwrap(), "--format", "json"])
        .output()
        .expect("failed to spawn pyllow");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let parsed = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout must be pure JSON:\n{stdout}\nerror: {e}"));
    (parsed, String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn envelope_carries_schema_version_and_tool() {
    let dir = probe_project();
    let (json, _) = check_json(dir.path());
    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["tool"], "pyllow");
}

#[test]
fn diagnostic_paths_are_repository_relative_posix() {
    let dir = probe_project();
    let (json, _) = check_json(dir.path());
    let diags = json["diagnostics"].as_array().expect("diagnostics array");
    assert!(!diags.is_empty(), "expected at least one diagnostic");
    for d in diags {
        let path = d["path"].as_str().expect("diagnostic path");
        assert!(
            !path.starts_with('/') && !path.contains(':'),
            "path must be repository-relative, got {path}"
        );
        assert!(
            !path.contains('\\'),
            "path must use POSIX separators, got {path}"
        );
    }
    assert!(
        diags.iter().any(|d| d["path"] == "src/orphan.py"),
        "expected src/orphan.py among {diags:?}"
    );
}

#[test]
fn localized_diagnostics_carry_inclusive_one_based_ranges() {
    let dir = smell_project();
    let (json, _) = run_json(&["smells", dir.path().to_str().unwrap(), "--format", "json"]);
    let diags = json["diagnostics"].as_array().unwrap();
    let smell = diags
        .iter()
        .find(|d| d["rule"] == "mutable-default")
        .unwrap_or_else(|| panic!("expected a mutable-default diagnostic in {diags:?}"));
    // `def build(items=[])` sits on line 2, after the leading comment.
    assert_eq!(smell["startLine"], 2, "one-based, not zero-based");
    assert_eq!(smell["endLine"], 2, "inclusive");
}

// #8: "paths are repository-relative POSIX paths" applies to the whole
// document, not just the diagnostics view.
#[test]
fn issue_paths_are_relative_too() {
    let dir = probe_project();
    let (json, _) = check_json(dir.path());
    for issue in json["issues"].as_array().expect("issues array") {
        let path = issue["path"].as_str().expect("issue path");
        assert!(
            !path.starts_with('/'),
            "issues[].path must be repository-relative, got {path}"
        );
    }
}

#[test]
fn every_diagnostic_carries_a_rule_and_message() {
    let dir = probe_project();
    let (json, _) = check_json(dir.path());
    for d in json["diagnostics"].as_array().unwrap() {
        assert!(
            d["rule"].as_str().is_some_and(|r| !r.is_empty()),
            "missing rule in {d:?}"
        );
        assert!(
            d["message"].as_str().is_some_and(|m| !m.is_empty()),
            "missing message in {d:?}"
        );
    }
}

#[test]
fn file_level_diagnostics_have_null_range_rather_than_a_fake_one() {
    let dir = probe_project();
    let (json, _) = check_json(dir.path());
    let diags = json["diagnostics"].as_array().unwrap();
    let unused_file = diags
        .iter()
        .find(|d| d["rule"] == "unused-file")
        .expect("expected an unused-file diagnostic");
    assert!(
        unused_file["startLine"].is_null(),
        "a file-level finding must not invent line 1"
    );
}

// #8: "JSON uses stdout exclusively; progress and metadata use stderr."
// A progress line leaking into stdout corrupts the document for every
// consumer that pipes it into a parser.
#[test]
fn progress_goes_to_stderr_leaving_stdout_pure_json() {
    let dir = smell_project();
    let diff = dir.path().join("pr.diff");
    fs::write(
        &diff,
        "--- a/src/app.py\n+++ b/src/app.py\n@@ -1,2 +1,3 @@\n ctx\n+added\n",
    )
    .unwrap();
    let out = Command::new(pyllow_bin())
        .args([
            "audit",
            dir.path().to_str().unwrap(),
            "--diff-file",
            diff.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .expect("failed to spawn pyllow");

    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str::<Value>(&stdout)
        .unwrap_or_else(|e| panic!("stdout must parse as a single JSON document:\n{stdout}\n{e}"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("auditing"),
        "progress belongs on stderr, got: {stderr}"
    );
}

// #8: "Incomplete analysis cannot return clean." A file pyllow could not parse
// was excluded from every other check, so reporting success would be a lie.
#[test]
fn unparseable_source_does_not_report_clean() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/broken.py"), "def f(:\n    ???\n").unwrap();
    let out = Command::new(pyllow_bin())
        .args(["check", dir.path().to_str().unwrap(), "--format", "json"])
        .output()
        .expect("failed to spawn pyllow");
    let code = out.status.code().unwrap();
    assert_ne!(
        code, 0,
        "a file that could not be parsed must not yield a clean run"
    );
    let json: Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert!(
        json["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["rule"] == "parse-error"),
        "the unparseable file must surface as a parse-error diagnostic"
    );
}

// #8: executed-rule metadata. The distinction that matters is "executed" vs
// "observed": a rule that ran and found nothing must still be listed, or a
// consumer cannot tell "clean" from "that rule was disabled".
#[test]
fn envelope_lists_rules_that_ran_even_when_they_found_nothing() {
    let dir = smell_project();
    let (json, _) = run_json(&["smells", dir.path().to_str().unwrap(), "--format", "json"]);
    let executed: Vec<&str> = json["rules"]["executed"]
        .as_array()
        .expect("rules.executed array")
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    assert!(
        executed.contains(&"mutable-default"),
        "the rule that fired must be listed: {executed:?}"
    );
    assert!(
        executed.contains(&"broad-except"),
        "a rule that ran and found nothing must still be listed: {executed:?}"
    );
}

// #8: "Versioning and compatibility policy are documented and tested."
//
// These are the fields docs/machine-output.md promises. Removing or renaming
// any of them is a breaking change: this test fails, forcing a deliberate
// schemaVersion bump and a doc update rather than a silent break. Adding
// fields is allowed and deliberately does not fail here.
#[test]
fn documented_contract_fields_are_present() {
    let dir = smell_project();
    let (json, _) = run_json(&["smells", dir.path().to_str().unwrap(), "--format", "json"]);

    for key in ["schemaVersion", "tool", "rules", "diagnostics", "issues", "stats"] {
        assert!(
            !json[key].is_null(),
            "envelope must carry documented field `{key}`"
        );
    }
    assert!(!json["rules"]["executed"].is_null(), "rules.executed");

    let diag = &json["diagnostics"][0];
    for key in ["path", "startLine", "endLine", "rule", "message"] {
        assert!(
            diag.get(key).is_some(),
            "diagnostic must carry documented field `{key}`, got {diag:?}"
        );
    }
}

// Ties the code's schema version to the documented one, so a bump cannot land
// in one place and not the other.
#[test]
fn documented_schema_version_matches_emitted_one() {
    let dir = smell_project();
    let (json, _) = run_json(&["smells", dir.path().to_str().unwrap(), "--format", "json"]);
    let emitted = json["schemaVersion"].as_u64().expect("schemaVersion");

    let doc = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("docs/machine-output.md"),
    )
    .expect("docs/machine-output.md must exist");

    assert!(
        doc.contains(&format!("\"schemaVersion\": {emitted}")),
        "docs/machine-output.md must document schemaVersion {emitted}"
    );
}
