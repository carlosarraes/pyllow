//! Issue #4: `pyllow audit --only <family> --rule <rule>`.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;

fn pyllow_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pyllow"))
}

/// One file tripping `mutable-default`, `broad-except`, and `unused-file`,
/// plus a diff that touches every line so nothing is scoped away.
fn project(config: &str) -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    let src = "def build(items=[]):\n    try:\n        return items\n    except Exception:\n        return None\n";
    fs::write(dir.path().join("src/app.py"), src).unwrap();
    let mut diff = String::from("--- /dev/null\n+++ b/src/app.py\n@@ -0,0 +1,5 @@\n");
    for line in src.lines() {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    fs::write(dir.path().join("pr.diff"), diff).unwrap();
    fs::write(dir.path().join("pyllow.toml"), config).unwrap();
    dir
}

fn audit(root: &Path, extra: &[&str]) -> (i32, Value, String) {
    let diff = root.join("pr.diff");
    let mut args = vec![
        "audit",
        root.to_str().unwrap(),
        "--diff-file",
        diff.to_str().unwrap(),
        "--format",
        "json",
    ];
    args.extend_from_slice(extra);
    let out = Command::new(pyllow_bin()).args(&args).output().unwrap();
    let json = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap_or(Value::Null);
    (
        out.status.code().unwrap(),
        json,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
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
fn no_selection_runs_every_family() {
    let dir = project("");
    let (_, json, _) = audit(dir.path(), &[]);
    let families = strs(&json["families"]["executed"]);
    for f in ["check", "dupes", "health", "smells"] {
        assert!(families.contains(&f.to_string()), "{families:?}");
    }
    assert!(strs(&json["families"]["requested"]).is_empty());
    let f = fired(&json);
    assert!(f.contains(&"unused-file".to_string()));
    assert!(f.contains(&"broad-except".to_string()));
}

#[test]
fn only_family_limits_findings_and_metadata() {
    let dir = project("");
    let (_, json, _) = audit(dir.path(), &["--only", "smells"]);
    assert_eq!(strs(&json["families"]["executed"]), vec!["smells"]);
    assert_eq!(strs(&json["families"]["requested"]), vec!["smells"]);
    let f = fired(&json);
    assert!(f.contains(&"broad-except".to_string()));
    assert!(
        !f.contains(&"unused-file".to_string()),
        "check family must not run"
    );
    let executed = strs(&json["rules"]["executed"]);
    assert!(!executed.contains(&"unused-file".to_string()));
    assert!(!executed.contains(&"duplicate".to_string()));
}

#[test]
fn rule_selection_narrows_within_the_family() {
    let dir = project("");
    let (code, json, _) = audit(dir.path(), &["--only", "smells", "--rule", "broad-except"]);
    assert_eq!(code, 1);
    assert_eq!(fired(&json), vec!["broad-except"]);
    assert_eq!(strs(&json["rules"]["executed"]), vec!["broad-except"]);
    assert_eq!(strs(&json["rules"]["requested"]), vec!["broad-except"]);
}

#[test]
fn multiple_families_and_rules_compose() {
    let dir = project("");
    let (_, json, _) = audit(
        dir.path(),
        &[
            "--only",
            "smells",
            "--only",
            "check",
            "--rule",
            "broad-except",
            "--rule",
            "unused-file",
        ],
    );
    let mut f = fired(&json);
    f.sort();
    assert_eq!(f, vec!["broad-except", "unused-file"]);
}

#[test]
fn rule_outside_selected_family_is_rejected_before_scanning() {
    let dir = project("");
    let (code, _, stderr) = audit(dir.path(), &["--only", "health", "--rule", "broad-except"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("broad-except") && stderr.contains("health"),
        "{stderr}"
    );
}

#[test]
fn unknown_rule_is_rejected() {
    let dir = project("");
    let (code, _, stderr) = audit(dir.path(), &["--only", "smells", "--rule", "nope"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("nope"), "{stderr}");
}

#[test]
fn unknown_family_is_rejected() {
    let dir = project("");
    let (code, _, _) = audit(dir.path(), &["--only", "linting"]);
    assert_eq!(code, 2);
}

#[test]
fn rule_without_family_is_rejected() {
    let dir = project("");
    let (code, _, _) = audit(dir.path(), &["--rule", "broad-except"]);
    assert_eq!(code, 2);
}

// A requested rule the config has turned off would silently find nothing
// and pass. Fail closed instead.
#[test]
fn requesting_a_disabled_rule_is_rejected() {
    let dir = project("");
    let (code, _, stderr) = audit(
        dir.path(),
        &["--only", "smells", "--rule", "no-explicit-any"],
    );
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("no-explicit-any") && stderr.contains("enabled"),
        "{stderr}"
    );
}

#[test]
fn configured_banned_api_id_is_a_selectable_smell_rule() {
    let dir = project(
        "[smells]\nenabled = [\"banned-api\"]\n[[smells.banned_api]]\nid = \"no-typing-cast\"\npath = \"typing.cast\"\nmessage = \"m\"\n",
    );
    let (code, json, stderr) = audit(
        dir.path(),
        &["--only", "smells", "--rule", "no-typing-cast"],
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(strs(&json["rules"]["executed"]), vec!["no-typing-cast"]);
}

// Incomplete analysis cannot return clean, whatever was selected.
#[test]
fn parse_errors_survive_family_selection() {
    let dir = project("");
    fs::write(dir.path().join("src/broken.py"), "def f(:\n").unwrap();
    let (code, json, _) = audit(dir.path(), &["--only", "dupes"]);
    assert_eq!(code, 1);
    assert!(fired(&json).contains(&"parse-error".to_string()));
}
