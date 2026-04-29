use globset::Glob;
use pyllow_types::Issue;
use rustc_hash::FxHashMap;
use std::fs;
use std::path::{Path, PathBuf};

const CODEOWNERS_LOCATIONS: &[&str] =
    &["CODEOWNERS", ".github/CODEOWNERS", "docs/CODEOWNERS"];

#[derive(Debug, Clone)]
pub struct OwnerRule {
    pub pattern: String,
    pub owners: Vec<String>,
    matcher: globset::GlobMatcher,
}

#[derive(Debug, Clone, Default)]
pub struct Codeowners {
    pub rules: Vec<OwnerRule>,
}

impl Codeowners {
    pub fn load(project_root: &Path) -> Option<Self> {
        for candidate in CODEOWNERS_LOCATIONS {
            let path = project_root.join(candidate);
            if path.exists() {
                if let Ok(raw) = fs::read_to_string(&path) {
                    return Some(Self::parse(&raw));
                }
            }
        }
        None
    }

    pub fn parse(text: &str) -> Self {
        let mut rules = Vec::new();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let mut parts = trimmed.split_whitespace();
            let Some(pattern) = parts.next() else {
                continue;
            };
            let owners: Vec<String> = parts.map(String::from).collect();
            if owners.is_empty() {
                continue;
            }
            let glob_pattern = normalize_codeowner_pattern(pattern);
            let Ok(glob) = Glob::new(&glob_pattern) else {
                continue;
            };
            rules.push(OwnerRule {
                pattern: pattern.to_string(),
                owners,
                matcher: glob.compile_matcher(),
            });
        }
        Self { rules }
    }

    pub fn owners_for(&self, relative_path: &Path) -> Option<&[String]> {
        let path_str = relative_path.to_string_lossy();
        for rule in self.rules.iter().rev() {
            if rule.matcher.is_match(path_str.as_ref()) {
                return Some(&rule.owners);
            }
        }
        None
    }
}

fn normalize_codeowner_pattern(pattern: &str) -> String {
    let trimmed = pattern.trim_start_matches('/');
    if trimmed.ends_with('/') {
        format!("{}**", trimmed)
    } else if !pattern.contains('*') && !pattern.contains('?') && !pattern.contains('.') {
        format!("**/{}/**", trimmed)
    } else if pattern.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("**/{}", trimmed)
    }
}

pub fn group_by_owner<'a>(
    issues: &'a [Issue],
    project_root: &Path,
    owners: &Codeowners,
) -> FxHashMap<String, Vec<&'a Issue>> {
    let mut buckets: FxHashMap<String, Vec<&'a Issue>> = FxHashMap::default();
    for issue in issues {
        let path = issue.path();
        let relative: PathBuf = path
            .strip_prefix(project_root)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| path.to_path_buf());
        let label = owners
            .owners_for(&relative)
            .filter(|o| !o.is_empty())
            .map(|o| o.join(" "))
            .unwrap_or_else(|| "(unowned)".to_string());
        buckets.entry(label).or_default().push(issue);
    }
    buckets
}

pub fn group_by_top_level_dir<'a>(
    issues: &'a [Issue],
    project_root: &Path,
) -> FxHashMap<String, Vec<&'a Issue>> {
    let mut buckets: FxHashMap<String, Vec<&'a Issue>> = FxHashMap::default();
    for issue in issues {
        let path = issue.path();
        let relative: PathBuf = path
            .strip_prefix(project_root)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| path.to_path_buf());
        let label = relative
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .unwrap_or_else(|| "(root)".to_string());
        buckets.entry(label).or_default().push(issue);
    }
    buckets
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parses_simple_codeowners() {
        let text = "# CODEOWNERS\n* @global\nsrc/auth/ @auth-team\n*.py @python-folks\n";
        let owners = Codeowners::parse(text);
        assert_eq!(owners.rules.len(), 3);
    }

    #[test]
    fn last_match_wins() {
        let text = "* @global\nsrc/auth/ @auth-team\n";
        let owners = Codeowners::parse(text);
        assert_eq!(
            owners.owners_for(Path::new("src/auth/login.py")).unwrap(),
            &["@auth-team".to_string()]
        );
        assert_eq!(
            owners.owners_for(Path::new("src/main.py")).unwrap(),
            &["@global".to_string()]
        );
    }

    #[test]
    fn unowned_when_no_match() {
        let owners = Codeowners::parse("src/auth/ @auth-team\n");
        assert!(owners.owners_for(Path::new("docs/readme.md")).is_none());
    }

    #[test]
    fn group_by_owner_buckets_issues() {
        let owners = Codeowners::parse("* @global\nsrc/auth/ @auth-team\n");
        let issues = vec![
            Issue::UnusedFile {
                path: PathBuf::from("/proj/src/main.py"),
            },
            Issue::UnusedFile {
                path: PathBuf::from("/proj/src/auth/login.py"),
            },
        ];
        let buckets = group_by_owner(&issues, Path::new("/proj"), &owners);
        assert!(buckets.contains_key("@global"));
        assert!(buckets.contains_key("@auth-team"));
    }
}
