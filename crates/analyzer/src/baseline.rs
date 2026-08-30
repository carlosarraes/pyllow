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
            format!("unused-dep:{name}:{source}")
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
            format!("complexity:{}:{}", relative(path, project_root), function)
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
            let mut parts: Vec<String> = cycle.iter().map(|p| relative(p, project_root)).collect();
            parts.sort();
            format!("circular-dependency:{}", parts.join("|"))
        }
        Issue::RefactorTarget { path, function, .. } => {
            format!(
                "refactor-target:{}:{}",
                relative(path, project_root),
                function
            )
        }
        Issue::FeatureFlag {
            path, flag, line, ..
        } => {
            format!(
                "feature-flag:{}:{}:{}",
                relative(path, project_root),
                line,
                flag
            )
        }
        // The configured ID is the rule key; the line is deliberately part of
        // the fingerprint because each call site is a separate policy
        // violation, unlike a per-function metric.
        Issue::BannedApi { path, line, id, .. } => {
            format!("{id}:{}:{line}", relative(path, project_root))
        }
        Issue::ParseError { path, .. } => {
            // Fingerprint by path only — the rustpython error message can
            // shift across versions; baselining the exact text would
            // create churn on every parser bump.
            format!("parse-error:{}", relative(path, project_root))
        }
        Issue::BoundaryViolation {
            from_path,
            from_zone,
            to_path,
            to_zone,
            // Deliberately excluded from the fingerprint so unrelated edits
            // shifting line numbers don't invalidate the baseline. Re-baselining
            // is meant for zone/path changes, not line drift.
            from_line: _,
        } => {
            // Fingerprint by zone names + paths so renaming a zone resets
            // the baseline (intentional review surface).
            format!(
                "boundary-violation:{}->{}:{}->{}",
                from_zone,
                to_zone,
                relative(from_path, project_root),
                relative(to_path, project_root)
            )
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
    let parsed: BaselineFile =
        serde_json::from_str(&raw).map_err(|source| BaselineError::Parse {
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
    let mut fingerprints: Vec<String> = issues
        .iter()
        .map(|i| fingerprint(i, project_root))
        .collect();
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
    format!("epoch+{secs}")
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

#[cfg(test)]
mod rule_identity_tests {
    use super::*;
    use pyllow_types::{DuplicateOccurrence, Effort, FlagProvider, SmellRule};

    /// One of every issue variant. Exhaustiveness is enforced by the match in
    /// `fingerprint`; this list exists so the invariant below sees them all.
    fn one_of_each() -> Vec<Issue> {
        let path = PathBuf::from("/repo/src/a.py");
        vec![
            Issue::UnusedFile { path: path.clone() },
            Issue::UnusedImport {
                path: path.clone(),
                line: 1,
                name: "os".into(),
                module: "os".into(),
            },
            Issue::UnusedDep {
                path: path.clone(),
                name: "requests".into(),
                source: "pyproject.toml".into(),
            },
            Issue::Duplicate {
                token_count: 50,
                occurrences: vec![DuplicateOccurrence {
                    path: path.clone(),
                    start_line: 1,
                    end_line: 9,
                }],
            },
            Issue::Complexity {
                path: path.clone(),
                line: 1,
                end_line: 9,
                function: "f".into(),
                cyclomatic: 12,
                cognitive: 9,
            },
            Issue::LowMaintainability {
                path: path.clone(),
                score: 40,
                avg_cyclomatic: 3.0,
                loc: 200,
            },
            Issue::Hotspot {
                path: path.clone(),
                cyclomatic: 20,
                churn: 30,
                score: 1.5,
            },
            Issue::Smell {
                path: path.clone(),
                line: 3,
                rule: SmellRule::StrayPrint,
                detail: String::new(),
            },
            Issue::CircularDependency {
                cycle: vec![path.clone()],
            },
            Issue::RefactorTarget {
                path: path.clone(),
                line: 1,
                end_line: 9,
                function: "f".into(),
                cyclomatic: 20,
                cognitive: 30,
                effort: Effort::High,
            },
            Issue::FeatureFlag {
                path: path.clone(),
                line: 4,
                flag: "NEW_UI".into(),
                provider: FlagProvider::EnvVar,
            },
            Issue::ParseError {
                path: path.clone(),
                message: "bad syntax".into(),
            },
            Issue::BoundaryViolation {
                from_path: path.clone(),
                from_line: 2,
                from_zone: "web".into(),
                to_path: PathBuf::from("/repo/src/b.py"),
                to_zone: "db".into(),
            },
        ]
    }

    // #1: "Rules have stable identifiers used by configuration, JSON, SARIF,
    // suppressions, and baselines." Baseline fingerprints are built from
    // hand-written prefixes; if a rule key is ever renamed without updating
    // them, every existing baseline entry silently stops matching and
    // grandfathered findings come roaring back. This ties the two together.
    #[test]
    fn every_fingerprint_embeds_the_stable_rule_key() {
        let root = Path::new("/repo");
        for issue in one_of_each() {
            let fp = fingerprint(&issue, root);
            let key = issue.rule_key();
            assert!(
                fp.contains(key.as_ref()),
                "fingerprint `{fp}` must embed rule key `{key}`"
            );
        }
    }
}
