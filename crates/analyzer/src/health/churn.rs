//! Git-churn lookup for hotspot scoring.

use super::FileHealth;
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

pub(super) fn compute_churn(project_root: &Path, files: &[FileHealth]) -> FxHashMap<PathBuf, u32> {
    let git_root = find_git_root(project_root).unwrap_or_else(|| project_root.to_path_buf());
    let output = Command::new("git")
        .args(["log", "--name-only", "--pretty=format:"])
        .current_dir(&git_root)
        .output();
    let Ok(output) = output else {
        return FxHashMap::default();
    };
    if !output.status.success() {
        return FxHashMap::default();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut counts: FxHashMap<PathBuf, u32> = FxHashMap::default();
    let known: FxHashSet<PathBuf> = files
        .iter()
        .filter_map(|f| f.path.canonicalize().ok())
        .collect();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let abs = git_root.join(trimmed);
        if let Ok(canonical) = abs.canonicalize() {
            if known.contains(&canonical) {
                *counts.entry(canonical).or_insert(0) += 1;
            }
        }
    }
    counts
}

fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}
