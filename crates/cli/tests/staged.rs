//! Issue #5: `pyllow audit --staged` analyzes the exact Git index.

use serde_json::Value;
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
        // Hermetic: the developer's global config must not leak in. (Found
        // the hard way — a global gitignore for `pyllow.toml` silently kept
        // the staged-config fixture out of the index.)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Repo with one committed clean file.
fn repo() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/app.py"), "def f(x):\n    return x\n").unwrap();
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-q", "-m", "init"]);
    dir
}

fn audit_staged(root: &Path, extra: &[&str]) -> (i32, Value, String) {
    let mut args = vec![
        "audit",
        root.to_str().unwrap(),
        "--staged",
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

fn fired(json: &Value) -> Vec<(String, String, i64)> {
    json["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["rule"].as_str().unwrap().to_string(),
                d["path"].as_str().unwrap().to_string(),
                d["startLine"].as_i64().unwrap_or(-1),
            )
        })
        .collect()
}

/// Snapshot of everything `--staged` must not change.
fn state(root: &Path) -> (String, String, Vec<u8>) {
    (
        git(root, &["status", "--porcelain=v2", "-uall"]),
        git(root, &["rev-parse", "HEAD"]),
        fs::read(root.join(".git/index")).unwrap(),
    )
}

const SMELLY: &str = "def build(items=[]):\n    return items\n";

#[test]
fn analyzes_staged_content_not_the_worktree() {
    let dir = repo();
    // Stage a smelly version, then fix it in the worktree only.
    fs::write(dir.path().join("src/app.py"), SMELLY).unwrap();
    git(dir.path(), &["add", "src/app.py"]);
    fs::write(
        dir.path().join("src/app.py"),
        "def build(items=None):\n    return items\n",
    )
    .unwrap();

    let before = state(dir.path());
    let (code, json, stderr) = audit_staged(dir.path(), &["--only", "smells"]);
    assert_eq!(code, 1, "the staged smell must be found: {stderr}");
    let f = fired(&json);
    assert_eq!(f, vec![("mutable-default".into(), "src/app.py".into(), 1)]);
    assert_eq!(
        state(dir.path()),
        before,
        "index/worktree must be untouched"
    );
}

#[test]
fn worktree_only_changes_are_invisible() {
    let dir = repo();
    // Smell exists only in the worktree; index still holds the clean version.
    fs::write(dir.path().join("src/app.py"), SMELLY).unwrap();
    let before = state(dir.path());
    let (code, json, stderr) = audit_staged(dir.path(), &["--only", "smells"]);
    assert_eq!(code, 0, "{stderr}\n{json}");
    assert_eq!(state(dir.path()), before);
}

#[test]
fn unstaged_insertions_do_not_shift_reported_lines() {
    let dir = repo();
    fs::write(dir.path().join("src/app.py"), SMELLY).unwrap();
    git(dir.path(), &["add", "src/app.py"]);
    // Unstaged: push the function down five lines.
    fs::write(
        dir.path().join("src/app.py"),
        format!("# a\n# b\n# c\n# d\n# e\n{SMELLY}"),
    )
    .unwrap();
    let (_, json, _) = audit_staged(dir.path(), &["--only", "smells"]);
    assert_eq!(
        fired(&json)[0].2,
        1,
        "line must come from the staged content"
    );
}

#[test]
fn renames_report_the_post_image_path() {
    let dir = repo();
    fs::write(dir.path().join("src/app.py"), SMELLY).unwrap();
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-q", "-m", "smelly"]);
    git(dir.path(), &["mv", "src/app.py", "src/renamed.py"]);

    let (_, json, _) = audit_staged(dir.path(), &["--only", "smells"]);
    let paths: Vec<String> = fired(&json).into_iter().map(|f| f.1).collect();
    assert!(!paths.is_empty(), "renamed file must be analyzed");
    assert!(
        paths.iter().all(|p| p == "src/renamed.py"),
        "expected post-image path, got {paths:?}"
    );
}

#[test]
fn staged_deletions_are_skipped_not_crashed() {
    let dir = repo();
    fs::write(dir.path().join("src/gone.py"), SMELLY).unwrap();
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-q", "-m", "add"]);
    git(dir.path(), &["rm", "-q", "src/gone.py"]);
    let before = state(dir.path());
    let (code, _, stderr) = audit_staged(dir.path(), &["--only", "smells"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(state(dir.path()), before);
}

#[test]
fn empty_staged_changes_pass_without_analysis() {
    let dir = repo();
    let before = state(dir.path());
    let (code, json, _) = audit_staged(dir.path(), &["--only", "smells"]);
    assert_eq!(code, 0);
    assert!(fired(&json).is_empty());
    assert_eq!(state(dir.path()), before);
}

#[test]
fn staged_config_governs_the_run() {
    let dir = repo();
    fs::write(dir.path().join("src/app.py"), SMELLY).unwrap();
    // Staged config disables the rule; worktree config re-enables it.
    fs::write(
        dir.path().join("pyllow.toml"),
        "[smells]\ndisabled = [\"mutable-default\"]\n",
    )
    .unwrap();
    git(dir.path(), &["add", "-A"]);
    fs::write(dir.path().join("pyllow.toml"), "").unwrap();
    let (code, json, stderr) = audit_staged(dir.path(), &["--only", "smells"]);
    assert_eq!(code, 0, "staged config must win: {stderr}\n{json}");
}

#[test]
fn outside_a_git_repo_fails_closed() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/app.py"), SMELLY).unwrap();
    let (code, _, stderr) = audit_staged(dir.path(), &[]);
    assert_eq!(code, 2, "{stderr}");
}

#[test]
fn staged_conflicts_with_diff_file() {
    let dir = repo();
    let (code, _, _) = audit_staged(dir.path(), &["--diff-file", "x.diff"]);
    assert_eq!(code, 2);
}

// The index must survive a failing run too.
#[test]
fn index_untouched_when_the_run_fails() {
    let dir = repo();
    fs::write(dir.path().join("src/broken.py"), "def f(:\n").unwrap();
    git(dir.path(), &["add", "-A"]);
    let before = state(dir.path());
    let (code, _, _) = audit_staged(dir.path(), &["--only", "smells"]);
    assert_eq!(code, 1, "parse error must fail the gate");
    assert_eq!(state(dir.path()), before);
}
