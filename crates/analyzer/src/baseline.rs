use pyllow_types::Issue;
use rustc_hash::FxHashSet;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BaselineError {
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
pub struct BaselineFile {
    pub version: u32,
    pub generated_at: String,
    pub fingerprints: Vec<String>,
}

pub fn fingerprint(issue: &Issue, project_root: &Path) -> String {
    match issue {
        Issue::UnusedFile { path } => {
            format!("unused-file:{}", relative(path, project_root))
        }
        Issue::UnusedImport {
            path, line, name, ..
        } => {
            format!(
                "unused-import:{}:{}:{}",
                relative(path, project_root),
                line,
                name
            )
        }
        Issue::UnusedDep { name, source, .. } => {
            format!("unused-dep:{}:{}", name, source)
        }
        Issue::Duplicate {
            token_count,
            occurrences,
        } => {
            let mut parts: Vec<String> = occurrences
                .iter()
                .map(|o| {
                    format!(
                        "{}#{}-{}",
                        relative(&o.path, project_root),
                        o.start_line,
                        o.end_line
                    )
                })
                .collect();
            parts.sort();
            format!("duplicate:{}:{}", token_count, parts.join("|"))
        }
        Issue::Complexity { path, function, .. } => {
            format!(
                "complexity:{}:{}",
                relative(path, project_root),
                function
            )
        }
        Issue::LowMaintainability { path, .. } => {
            format!("low-maintainability:{}", relative(path, project_root))
        }
        Issue::Hotspot { path, .. } => {
            format!("hotspot:{}", relative(path, project_root))
        }
        Issue::Smell {
            path, line, rule, ..
        } => {
            format!(
                "smell:{}:{}:{}",
                rule.as_str(),
                relative(path, project_root),
                line
            )
        }
        Issue::CircularDependency { cycle } => {
            // Sort the cycle so rotated cycles ([a,b,c] vs [b,c,a]) hash to
            // the same fingerprint — they describe the same dependency loop.
            let mut parts: Vec<String> =
                cycle.iter().map(|p| relative(p, project_root)).collect();
            parts.sort();
            format!("circular-dependency:{}", parts.join("|"))
        }
    }
}

fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub fn load(path: &Path) -> Result<FxHashSet<String>, BaselineError> {
    let raw = fs::read_to_string(path).map_err(|source| BaselineError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let parsed: BaselineFile = serde_json::from_str(&raw).map_err(|source| BaselineError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(parsed.fingerprints.into_iter().collect())
}

pub fn save(path: &Path, issues: &[Issue], project_root: &Path) -> Result<(), BaselineError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|source| BaselineError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }
    let mut fingerprints: Vec<String> =
        issues.iter().map(|i| fingerprint(i, project_root)).collect();
    fingerprints.sort();
    fingerprints.dedup();
    let file = BaselineFile {
        version: 1,
        generated_at: now_iso(),
        fingerprints,
    };
    let json = serde_json::to_string_pretty(&file).map_err(|source| BaselineError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    fs::write(path, json).map_err(|source| BaselineError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

pub fn filter(issues: &mut Vec<Issue>, baseline: &FxHashSet<String>, project_root: &Path) -> usize {
    let before = issues.len();
    issues.retain(|i| !baseline.contains(&fingerprint(i, project_root)));
    before - issues.len()
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
    use pyllow_types::DuplicateOccurrence;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn root() -> PathBuf {
        PathBuf::from("/tmp/proj")
    }

    #[test]
    fn fingerprint_unused_file_relative() {
        let i = Issue::UnusedFile {
            path: PathBuf::from("/tmp/proj/src/foo.py"),
        };
        assert_eq!(fingerprint(&i, &root()), "unused-file:src/foo.py");
    }

    #[test]
    fn fingerprint_unused_import() {
        let i = Issue::UnusedImport {
            path: PathBuf::from("/tmp/proj/main.py"),
            line: 7,
            name: "os".into(),
            module: "os".into(),
        };
        assert_eq!(fingerprint(&i, &root()), "unused-import:main.py:7:os");
    }

    #[test]
    fn fingerprint_duplicate_stable() {
        let i = Issue::Duplicate {
            token_count: 50,
            occurrences: vec![
                DuplicateOccurrence {
                    path: PathBuf::from("/tmp/proj/b.py"),
                    start_line: 10,
                    end_line: 20,
                },
                DuplicateOccurrence {
                    path: PathBuf::from("/tmp/proj/a.py"),
                    start_line: 1,
                    end_line: 11,
                },
            ],
        };
        let fp = fingerprint(&i, &root());
        assert!(fp.contains("a.py#1-11"));
        assert!(fp.contains("b.py#10-20"));
        assert!(fp.starts_with("duplicate:50:"));
    }

    #[test]
    fn round_trip_baseline() {
        let dir = tempdir().unwrap();
        let baseline_path = dir.path().join("baseline.json");
        let issues = vec![
            Issue::UnusedFile {
                path: PathBuf::from("/tmp/proj/orphan.py"),
            },
            Issue::UnusedImport {
                path: PathBuf::from("/tmp/proj/main.py"),
                line: 3,
                name: "sys".into(),
                module: "sys".into(),
            },
        ];
        save(&baseline_path, &issues, &root()).unwrap();
        let loaded = load(&baseline_path).unwrap();
        assert!(loaded.contains("unused-file:orphan.py"));
        assert!(loaded.contains("unused-import:main.py:3:sys"));
    }

    #[test]
    fn filter_drops_baselined_issues() {
        let mut issues = vec![
            Issue::UnusedFile {
                path: PathBuf::from("/tmp/proj/old.py"),
            },
            Issue::UnusedFile {
                path: PathBuf::from("/tmp/proj/new.py"),
            },
        ];
        let mut baseline = FxHashSet::default();
        baseline.insert("unused-file:old.py".to_string());
        let dropped = filter(&mut issues, &baseline, &root());
        assert_eq!(dropped, 1);
        assert_eq!(issues.len(), 1);
    }
}
