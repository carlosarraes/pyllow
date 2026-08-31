//! Issue #2 end-to-end: the configured rule ID flows through JSON, SARIF,
//! and rule-specific suppression; the family ships disabled.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;

fn pyllow_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pyllow"))
}

const BAN: &str = "[[smells.banned_api]]\nid = \"no-typing-cast\"\npath = \"typing.cast\"\nmessage = \"Prefer parsing.\"\n";

fn project(config: &str) -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("src/app.py"),
        "import typing\n\ndef f(x):\n    return typing.cast(int, x)\n",
    )
    .unwrap();
    fs::write(dir.path().join("pyllow.toml"), config).unwrap();
    dir
}

fn run(root: &Path, format: &str) -> (i32, Value) {
    let out = Command::new(pyllow_bin())
        .args(["smells", root.to_str().unwrap(), "--format", format])
        .output()
        .unwrap();
    let json = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap_or(Value::Null);
    (out.status.code().unwrap(), json)
}

fn rules_fired(json: &Value) -> Vec<String> {
    json["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["rule"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn entries_alone_do_nothing_until_the_family_is_enabled() {
    let dir = project(BAN);
    let (code, json) = run(dir.path(), "json");
    assert_eq!(code, 0);
    assert!(rules_fired(&json).is_empty(), "family ships disabled");
    let executed = json["rules"]["executed"].as_array().unwrap();
    assert!(!executed.iter().any(|r| r == "banned-api"));
}

#[test]
fn enabled_family_reports_the_configured_id_at_the_call_range() {
    let dir = project(&format!("[smells]\nenabled = [\"banned-api\"]\n{BAN}"));
    let (code, json) = run(dir.path(), "json");
    assert_eq!(code, 1);
    let d = &json["diagnostics"][0];
    assert_eq!(d["rule"], "no-typing-cast");
    assert_eq!(d["path"], "src/app.py");
    assert_eq!(d["startLine"], 4);
    assert_eq!(d["endLine"], 4);
    assert!(d["message"].as_str().unwrap().contains("Prefer parsing."));
}

#[test]
fn sarif_uses_the_configured_id_as_rule_id() {
    let dir = project(&format!("[smells]\nenabled = [\"banned-api\"]\n{BAN}"));
    let (_, sarif) = run(dir.path(), "sarif");
    let result = &sarif["runs"][0]["results"][0];
    assert_eq!(result["ruleId"], "no-typing-cast");
    let catalog = sarif["runs"][0]["tool"]["driver"]["rules"]
        .as_array()
        .unwrap();
    assert!(catalog.iter().any(|r| r["id"] == "no-typing-cast"));
}

#[test]
fn rule_specific_suppression_uses_the_configured_id() {
    let dir = project(&format!(
        "[smells]\nenabled = [\"banned-api\"]\n{BAN}\n[[suppress]]\npath = \"src/app.py\"\nrules = [\"no-typing-cast\"]\n"
    ));
    let (code, json) = run(dir.path(), "json");
    assert_eq!(code, 0, "{json}");
    assert!(rules_fired(&json).is_empty());
}
