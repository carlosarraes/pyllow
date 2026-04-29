use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/fixtures/fastapi-minimal")
}

fn pyllow_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pyllow"))
}

#[test]
fn flags_only_orphan_in_fastapi_fixture() {
    let output = Command::new(pyllow_bin())
        .arg("check")
        .arg(fixture_root())
        .arg("--format")
        .arg("json")
        .output()
        .expect("failed to spawn pyllow");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("invalid json:\n{stdout}\nerror: {e}"));

    let issues = parsed["issues"].as_array().expect("issues array");
    let issue_paths: Vec<String> = issues
        .iter()
        .map(|i| i["path"].as_str().unwrap().to_string())
        .collect();

    assert_eq!(
        issue_paths.len(),
        1,
        "expected exactly one issue, got {issue_paths:?}"
    );
    assert!(
        issue_paths[0].ends_with("orphan.py"),
        "expected orphan.py to be flagged, got {}",
        issue_paths[0]
    );

    for handler in &["main.py", "users.py", "services.py"] {
        for path in &issue_paths {
            assert!(
                !path.ends_with(handler),
                "{handler} should not be flagged: it is a route handler or transitive dependency"
            );
        }
    }

    let stats = &parsed["stats"];
    assert!(
        stats["plugins_run"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "fastapi"),
        "fastapi plugin should have run"
    );
}
