use crate::score::{HealthScore, ScoreBreakdown};
use pyllow_types::Issue;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("io error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("json error in {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub generated_at: String,
    pub score: HealthScore,
    pub breakdown: ScoreBreakdown,
}

impl Snapshot {
    pub fn from_issues(issues: &[Issue]) -> Self {
        let breakdown = ScoreBreakdown::from_issues(issues);
        let score = crate::score::compute(issues);
        Self {
            version: 1,
            generated_at: now_iso(),
            score,
            breakdown,
        }
    }
}

pub fn save(path: &Path, snapshot: &Snapshot) -> Result<(), SnapshotError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|source| SnapshotError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }
    let json = serde_json::to_string_pretty(snapshot).map_err(|source| SnapshotError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    fs::write(path, json).map_err(|source| SnapshotError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

pub fn load(path: &Path) -> Result<Snapshot, SnapshotError> {
    let raw = fs::read_to_string(path).map_err(|source| SnapshotError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&raw).map_err(|source| SnapshotError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

#[derive(Debug, Clone, Default)]
pub struct Diff {
    pub score_delta: i32,
    pub total_issues_delta: i32,
    pub unused_files_delta: i32,
    pub unused_imports_delta: i32,
    pub unused_deps_delta: i32,
    pub duplicates_delta: i32,
    pub complexity_delta: i32,
    pub low_maintainability_delta: i32,
    pub hotspots_delta: i32,
    pub smells_delta: i32,
    pub circular_deps_delta: i32,
    pub refactor_targets_delta: i32,
    pub feature_flags_delta: i32,
}

pub fn compare(previous: &Snapshot, current: &Snapshot) -> Diff {
    let p = &previous.breakdown;
    let c = &current.breakdown;
    Diff {
        score_delta: current.score.value as i32 - previous.score.value as i32,
        total_issues_delta: c.total_issues as i32 - p.total_issues as i32,
        unused_files_delta: c.unused_files as i32 - p.unused_files as i32,
        unused_imports_delta: c.unused_imports as i32 - p.unused_imports as i32,
        unused_deps_delta: c.unused_deps as i32 - p.unused_deps as i32,
        duplicates_delta: c.duplicates as i32 - p.duplicates as i32,
        complexity_delta: c.complexity as i32 - p.complexity as i32,
        low_maintainability_delta: c.low_maintainability as i32 - p.low_maintainability as i32,
        hotspots_delta: c.hotspots as i32 - p.hotspots as i32,
        smells_delta: c.smells as i32 - p.smells as i32,
        circular_deps_delta: c.circular_deps as i32 - p.circular_deps as i32,
        refactor_targets_delta: c.refactor_targets as i32 - p.refactor_targets as i32,
        feature_flags_delta: c.feature_flags as i32 - p.feature_flags as i32,
    }
}

fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("epoch+{}", secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("snap.json");
        let issues = vec![Issue::UnusedFile {
            path: PathBuf::from("/x/o.py"),
        }];
        let snap = Snapshot::from_issues(&issues);
        save(&path, &snap).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.score, snap.score);
        assert_eq!(loaded.breakdown.total_issues, 1);
    }

    #[test]
    fn diff_detects_score_change() {
        let prev = Snapshot::from_issues(&[]);
        let cur_issues: Vec<Issue> = (0..5)
            .map(|i| Issue::UnusedFile {
                path: PathBuf::from(format!("/x/{i}.py")),
            })
            .collect();
        let cur = Snapshot::from_issues(&cur_issues);
        let diff = compare(&prev, &cur);
        assert!(diff.score_delta < 0);
        assert_eq!(diff.unused_files_delta, 5);
    }
}
