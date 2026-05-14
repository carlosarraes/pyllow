//! Unified-diff index for line-level audit scoping.
//!
//! Parses a unified diff (output of `git diff`, `diff -u`, etc.) into a fast
//! lookup of which files and which line numbers in the **new** version are
//! touched by additions. Removed-only lines are not tracked at the line level
//! since they no longer exist in the new file — but the containing file is
//! still marked as touched, which lets line-less issue kinds (hotspot,
//! low-maintainability, circular dependency) fall back to file-scoping.

use rustc_hash::{FxHashMap, FxHashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone)]
pub struct DiffIndex {
    /// Files touched by the diff (any addition, deletion, or modification).
    /// Stored as canonical absolute paths when canonicalization succeeds;
    /// falls back to project-root-joined paths for files that don't exist
    /// (e.g., deleted in the diff).
    touched_files: FxHashSet<PathBuf>,
    /// Line numbers in the **new** version of each file that contain `+` lines
    /// (additions and modifications). Lookup keys mirror `touched_files`.
    added_lines: FxHashMap<PathBuf, FxHashSet<u32>>,
}

impl DiffIndex {
    pub fn from_unified_diff(diff: &str, project_root: &Path) -> Self {
        let mut idx = DiffIndex::default();
        let mut current_file: Option<PathBuf> = None;
        let mut current_new_line: u32 = 0;
        let mut in_hunk = false;

        for raw in diff.lines() {
            if let Some(path_str) = raw.strip_prefix("+++ ") {
                in_hunk = false;
                current_file = parse_diff_path(path_str, project_root);
                if let Some(path) = &current_file {
                    idx.touched_files.insert(path.clone());
                }
            } else if let Some(path_str) = raw.strip_prefix("--- ") {
                in_hunk = false;
                if let Some(path) = parse_diff_path(path_str, project_root) {
                    idx.touched_files.insert(path);
                }
            } else if raw.starts_with("@@") && !raw.starts_with("@@@") {
                in_hunk = parse_hunk_new_start(raw)
                    .map(|start| {
                        current_new_line = start;
                        true
                    })
                    .unwrap_or(false);
            } else if in_hunk {
                match raw.as_bytes().first() {
                    Some(b'+') => {
                        if let Some(path) = &current_file {
                            idx.added_lines
                                .entry(path.clone())
                                .or_default()
                                .insert(current_new_line);
                        }
                        current_new_line += 1;
                    }
                    Some(b' ') => current_new_line += 1,
                    Some(b'-') => {}
                    Some(b'\\') => {} // "\ No newline at end of file"
                    _ => in_hunk = false,
                }
            }
        }
        idx
    }

    pub fn touches_file(&self, path: &Path) -> bool {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.touched_files.contains(&canonical)
    }

    pub fn touches_line(&self, path: &Path, line: u32) -> bool {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.added_lines
            .get(&canonical)
            .map(|lines| lines.contains(&line))
            .unwrap_or(false)
    }

    pub fn is_empty(&self) -> bool {
        self.touched_files.is_empty()
    }
}

/// Parse a `+++`/`---` header path, stripping the `a/`/`b/` prefix that git
/// adds and skipping the `/dev/null` marker (used for file create/delete).
/// Returns None for unparseable or sentinel paths.
fn parse_diff_path(s: &str, project_root: &Path) -> Option<PathBuf> {
    // Git appends a tab + timestamp to header paths in some output modes.
    let s = s.split('\t').next().unwrap_or(s).trim();
    if s == "/dev/null" || s.is_empty() {
        return None;
    }
    let rel = s
        .strip_prefix("a/")
        .or_else(|| s.strip_prefix("b/"))
        .unwrap_or(s)
        .trim_matches('"');
    if rel.is_empty() {
        return None;
    }
    let abs = project_root.join(rel);
    Some(abs.canonicalize().unwrap_or(abs))
}

/// Extract the new-file starting line from a hunk header like
/// `@@ -10,3 +12,4 @@ optional context` → `12`.
fn parse_hunk_new_start(header: &str) -> Option<u32> {
    let after_plus = header.split('+').nth(1)?.trim_start();
    let digits: String = after_plus
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, "").unwrap();
    }

    #[test]
    fn empty_diff_touches_nothing() {
        let dir = tempdir().unwrap();
        let foo = dir.path().join("foo.py");
        touch(&foo);
        let idx = DiffIndex::from_unified_diff("", dir.path());
        assert!(!idx.touches_file(&foo));
        assert!(!idx.touches_line(&foo, 1));
    }

    #[test]
    fn added_line_is_touched_context_is_not() {
        let dir = tempdir().unwrap();
        let foo = dir.path().join("foo.py");
        touch(&foo);
        // New file layout (lines 10-13):
        //   10: unchanged1  (context — no `+`, not touched)
        //   11: added       (`+`, touched)
        //   12: unchanged2  (context, not touched)
        //   13: unchanged3  (context, not touched)
        let diff = "--- a/foo.py\n+++ b/foo.py\n@@ -10,3 +10,4 @@\n unchanged1\n+added\n unchanged2\n unchanged3\n";
        let idx = DiffIndex::from_unified_diff(diff, dir.path());
        assert!(idx.touches_file(&foo));
        assert!(!idx.touches_line(&foo, 10));
        assert!(idx.touches_line(&foo, 11));
        assert!(!idx.touches_line(&foo, 12));
        assert!(!idx.touches_line(&foo, 13));
    }

    #[test]
    fn multiple_hunks_in_same_file_tracked() {
        let dir = tempdir().unwrap();
        let foo = dir.path().join("foo.py");
        touch(&foo);
        // Hunk 1: new lines 1..2 — `+added1` is line 2
        // Hunk 2: new lines 11..12 — `+added2` is line 12
        let diff = "--- a/foo.py\n+++ b/foo.py\n@@ -1,1 +1,2 @@\n unchanged\n+added1\n@@ -10,1 +11,2 @@\n unchanged\n+added2\n";
        let idx = DiffIndex::from_unified_diff(diff, dir.path());
        assert!(idx.touches_line(&foo, 2));
        assert!(idx.touches_line(&foo, 12));
        assert!(!idx.touches_line(&foo, 1));
        assert!(!idx.touches_line(&foo, 11));
    }

    #[test]
    fn deletion_only_hunk_touches_file_but_no_lines() {
        let dir = tempdir().unwrap();
        let foo = dir.path().join("foo.py");
        touch(&foo);
        // Removed lines don't exist in the new version; no lines should be flagged.
        let diff =
            "--- a/foo.py\n+++ b/foo.py\n@@ -10,3 +10,2 @@\n unchanged1\n-removed\n unchanged2\n";
        let idx = DiffIndex::from_unified_diff(diff, dir.path());
        assert!(idx.touches_file(&foo));
        assert!(!idx.touches_line(&foo, 10));
        assert!(!idx.touches_line(&foo, 11));
    }

    #[test]
    fn paths_resolved_against_project_root() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("src").join("foo.py");
        touch(&nested);
        // `a/`/`b/` prefixes must be stripped; the relative path is joined
        // against project_root and canonicalized.
        let diff = "--- a/src/foo.py\n+++ b/src/foo.py\n@@ -0,0 +1,1 @@\n+new\n";
        let idx = DiffIndex::from_unified_diff(diff, dir.path());
        assert!(idx.touches_file(&nested));
        assert!(idx.touches_line(&nested, 1));
    }

    #[test]
    fn new_file_creation_is_tracked() {
        let dir = tempdir().unwrap();
        let foo = dir.path().join("foo.py");
        touch(&foo);
        // git diff for a brand-new file uses `--- /dev/null`.
        let diff = "--- /dev/null\n+++ b/foo.py\n@@ -0,0 +1,2 @@\n+line1\n+line2\n";
        let idx = DiffIndex::from_unified_diff(diff, dir.path());
        assert!(idx.touches_file(&foo));
        assert!(idx.touches_line(&foo, 1));
        assert!(idx.touches_line(&foo, 2));
    }
}
