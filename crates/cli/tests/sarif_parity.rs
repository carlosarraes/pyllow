//! Issue #8: "SARIF rule IDs and locations match JSON."
//!
//! A CI setup that gates on the JSON and renders the SARIF in a code-scanning
//! UI must see the same finding at the same place under the same name.

use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::tempdir;

fn pyllow_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pyllow"))
}

fn project() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("src/app.py"),
        "# comment\ndef build(items=[]):\n    try:\n        pass\n    except Exception:\n        pass\n    return items\n",
    )
    .unwrap();
    dir
}

fn render(root: &std::path::Path, format: &str) -> Value {
    let out = Command::new(pyllow_bin())
        .args(["smells", root.to_str().unwrap(), "--format", format])
        .output()
        .expect("failed to spawn pyllow");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("{format} output must parse:\n{stdout}\n{e}"))
}

/// (rule, path, startLine, endLine) for every finding, from each renderer.
fn json_locations(v: &Value) -> BTreeSet<(String, String, i64, i64)> {
    v["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .map(|d| {
            (
                d["rule"].as_str().unwrap().to_string(),
                d["path"].as_str().unwrap().to_string(),
                d["startLine"].as_i64().unwrap_or(-1),
                d["endLine"].as_i64().unwrap_or(-1),
            )
        })
        .collect()
}

fn sarif_locations(v: &Value) -> BTreeSet<(String, String, i64, i64)> {
    v["runs"][0]["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|r| {
            let region = &r["locations"][0]["physicalLocation"]["region"];
            (
                r["ruleId"].as_str().unwrap().to_string(),
                r["locations"][0]["physicalLocation"]["artifactLocation"]["uri"]
                    .as_str()
                    .unwrap()
                    .to_string(),
                region["startLine"].as_i64().unwrap_or(-1),
                region["endLine"].as_i64().unwrap_or(-1),
            )
        })
        .collect()
}

#[test]
fn sarif_and_json_agree_on_rules_and_locations() {
    let dir = project();
    let json = render(dir.path(), "json");
    let sarif = render(dir.path(), "sarif");

    let from_json = json_locations(&json);
    let from_sarif = sarif_locations(&sarif);

    assert!(!from_json.is_empty(), "fixture should produce findings");
    assert_eq!(
        from_json, from_sarif,
        "SARIF and JSON must report the same rule at the same location"
    );
}

#[test]
fn sarif_uris_are_repository_relative() {
    let dir = project();
    let sarif = render(dir.path(), "sarif");
    for r in sarif["runs"][0]["results"].as_array().unwrap() {
        let uri = r["locations"][0]["physicalLocation"]["artifactLocation"]["uri"]
            .as_str()
            .unwrap();
        assert!(
            !uri.starts_with('/'),
            "SARIF uri must be repository-relative, got {uri}"
        );
    }
}
