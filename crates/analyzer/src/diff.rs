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
use thiserror::Error;

/// A diff we cannot scope from. Audit must fail closed on these rather than
/// silently producing an under-scoped index that lets findings through.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DiffParseError {
    #[error("combined diffs (merge commits, `diff --cc`) are not supported for line scoping")]
    CombinedDiff,
    #[error("malformed hunk header: {0}")]
    MalformedHunkHeader(String),
}

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
    /// Files containing at least one `-` line. Used as a soundness fallback
    /// for line-scoped audit: an issue on an unchanged line may have been
    /// introduced by a deletion elsewhere (e.g., removing the last usage of
    /// an import surfaces `UnusedImport` on the unchanged `import` line).
    files_with_deletions: FxHashSet<PathBuf>,
}

impl DiffIndex {
    pub fn from_unified_diff(diff: &str, project_root: &Path) -> Result<Self, DiffParseError> {
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
            } else if raw.starts_with("--- ") {
                // Pre-image path. Deliberately *not* registered: scoping uses
                // post-image paths only, so a rename doesn't mark the old path
                // as touched and a deleted file drops out entirely.
                in_hunk = false;
            } else if raw.starts_with("@@@") {
                return Err(DiffParseError::CombinedDiff);
            } else if raw.starts_with("@@") {
                current_new_line = parse_hunk_new_start(raw)
                    .ok_or_else(|| DiffParseError::MalformedHunkHeader(raw.to_string()))?;
                in_hunk = true;
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
                    Some(b'-') => {
                        if let Some(path) = &current_file {
                            idx.files_with_deletions.insert(path.clone());
                        }
                    }
                    Some(b'\\') => {} // "\ No newline at end of file"
                    _ => in_hunk = false,
                }
            }
        }
        Ok(idx)
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

    /// Whether any added line falls inside the inclusive 1-indexed range
    /// `start..=end`. Iterates the added-line set rather than the range, so
    /// cost tracks diff size instead of function length.
    pub fn touches_range(&self, path: &Path, start: u32, end: u32) -> bool {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.added_lines
            .get(&canonical)
            .map(|lines| lines.iter().any(|l| *l >= start && *l <= end))
            .unwrap_or(false)
    }

    pub fn is_empty(&self) -> bool {
        self.touched_files.is_empty()
    }

    /// Whether the given file contains at least one `-` line in the diff.
    /// Callers use this as a soundness fallback when an issue's reported line
    /// is unchanged but its meaning may have been altered by deletions
    /// elsewhere in the same file (canonical case: `UnusedImport` after a PR
    /// removes the last usage of a symbol).
    pub fn file_has_deletions(&self, path: &Path) -> bool {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.files_with_deletions.contains(&canonical)
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
    // Git quotes paths containing spaces, quotes, or non-ASCII bytes, with the
    // `a/`/`b/` prefix *inside* the quotes.
    let decoded = if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        unescape_git_path(&s[1..s.len() - 1])?
    } else {
        s.to_string()
    };
    if decoded == "/dev/null" {
        return None;
    }
    let rel = decoded
        .strip_prefix("a/")
        .or_else(|| decoded.strip_prefix("b/"))
        .unwrap_or(&decoded);
    if rel.is_empty() {
        return None;
    }
    let abs = project_root.join(rel);
    Some(abs.canonicalize().unwrap_or(abs))
}

/// Extract the new-file starting line from a hunk header like
/// `@@ -10,3 +12,4 @@ optional context` → `12`.
///
/// Strict by design: a header we cannot fully parse means we cannot know
/// which lines changed, and guessing produces a silently under-scoped audit.
/// Returns `None` for anything malformed so the caller can fail closed.
fn parse_hunk_new_start(header: &str) -> Option<u32> {
    let body = header.strip_prefix("@@")?;
    let spec = &body[..body.find("@@")?];
    let mut parts = spec.split_whitespace();

    let old = parts.next()?;
    if !old.starts_with('-') {
        return None;
    }
    let new = parts.next()?;
    let digits = new.strip_prefix('+')?;

    let mut fields = digits.split(',');
    let start = fields.next()?;
    // A count, when present, must be numeric too — `+12,` and `+12,x` are
    // truncated headers, not zero-count hunks.
    if let Some(count) = fields.next() {
        if count.is_empty() || !count.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    if fields.next().is_some() {
        return None;
    }
    if start.is_empty() || !start.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    start.parse().ok()
}

/// Decode a git-quoted path body: C-style escapes plus three-digit octal
/// bytes, which git uses for non-ASCII and special characters.
fn unescape_git_path(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        match *bytes.get(i)? {
            b'n' => {
                out.push(b'\n');
                i += 1;
            }
            b't' => {
                out.push(b'\t');
                i += 1;
            }
            b'r' => {
                out.push(b'\r');
                i += 1;
            }
            c @ (b'"' | b'\\') => {
                out.push(c);
                i += 1;
            }
            b'0'..=b'7' => {
                let mut value = 0u32;
                let mut digits = 0;
                while digits < 3 && matches!(bytes.get(i), Some(b'0'..=b'7')) {
                    value = value * 8 + u32::from(bytes[i] - b'0');
                    i += 1;
                    digits += 1;
                }
                out.push(u8::try_from(value).ok()?);
            }
            _ => return None,
        }
    }
    String::from_utf8(out).ok()
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

    // #6: a combined diff (merge commit / `diff --cc`) uses `@@@` headers with
    // two pre-image columns. Silently skipping them under-scopes the audit into
    // a false PASS, so scoping must refuse the input instead.
    #[test]
    fn combined_diff_is_rejected() {
        let dir = tempdir().unwrap();
        let diff = "--- a/foo.py\n+++ b/foo.py\n@@@ -1,2 -1,2 +1,3 @@@\n  ctx\n++added\n";
        assert!(matches!(
            DiffIndex::from_unified_diff(diff, dir.path()),
            Err(DiffParseError::CombinedDiff)
        ));
    }

    #[test]
    fn malformed_hunk_header_is_rejected() {
        let dir = tempdir().unwrap();
        let diff = "--- a/foo.py\n+++ b/foo.py\n@@ -1,2 +notanumber @@\n+added\n";
        assert!(matches!(
            DiffIndex::from_unified_diff(diff, dir.path()),
            Err(DiffParseError::MalformedHunkHeader(_))
        ));
    }

    #[test]
    fn truncated_hunk_header_is_rejected() {
        let dir = tempdir().unwrap();
        let diff = "--- a/foo.py\n+++ b/foo.py\n@@ -1,2 @@\n+added\n";
        assert!(matches!(
            DiffIndex::from_unified_diff(diff, dir.path()),
            Err(DiffParseError::MalformedHunkHeader(_))
        ));
    }

    // #6 "use post-image paths": a rename must not mark the pre-image path as
    // touched, or audit scopes findings against a file that no longer exists.
    #[test]
    fn rename_registers_only_the_post_image_path() {
        let dir = tempdir().unwrap();
        let old = dir.path().join("old.py");
        let new = dir.path().join("new.py");
        touch(&old);
        touch(&new);
        let diff = "--- a/old.py\n+++ b/new.py\n@@ -1,2 +1,3 @@\n ctx\n+added\n";
        let idx = DiffIndex::from_unified_diff(diff, dir.path()).unwrap();
        assert!(idx.touches_file(&new), "post-image path is in scope");
        assert!(!idx.touches_file(&old), "pre-image path must not be in scope");
    }

    // #6: git quotes paths containing spaces or non-ASCII bytes, with the
    // `a/`/`b/` prefix inside the quotes and non-ASCII encoded as octal.
    #[test]
    fn quoted_path_with_spaces_is_decoded() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("my dir");
        std::fs::create_dir_all(&sub).unwrap();
        let foo = sub.join("foo.py");
        touch(&foo);
        let diff = "--- \"a/my dir/foo.py\"\n+++ \"b/my dir/foo.py\"\n@@ -1,1 +1,2 @@\n ctx\n+added\n";
        let idx = DiffIndex::from_unified_diff(diff, dir.path()).unwrap();
        assert!(idx.touches_file(&foo), "quoted path with a space");
    }

    #[test]
    fn octal_escaped_path_is_decoded() {
        let dir = tempdir().unwrap();
        let foo = dir.path().join("café.py");
        touch(&foo);
        // Git renders the é (U+00E9, UTF-8 0xC3 0xA9) as \303\251.
        let diff = "--- \"a/caf\\303\\251.py\"\n+++ \"b/caf\\303\\251.py\"\n@@ -1,1 +1,2 @@\n ctx\n+added\n";
        let idx = DiffIndex::from_unified_diff(diff, dir.path()).unwrap();
        assert!(idx.touches_file(&foo), "octal-escaped non-ASCII path");
    }

    // Coverage guards for the input shapes #6 enumerates. These lock in
    // behavior the changes above already provide; they are regression fences,
    // not test-first drivers.
    #[test]
    fn zero_count_hunk_registers_pure_insertion() {
        let dir = tempdir().unwrap();
        let foo = dir.path().join("foo.py");
        touch(&foo);
        // `-5,0` is a pure insertion: nothing consumed from the pre-image.
        let diff = "--- a/foo.py\n+++ b/foo.py\n@@ -5,0 +6,2 @@\n+first\n+second\n";
        let idx = DiffIndex::from_unified_diff(diff, dir.path()).unwrap();
        assert!(idx.touches_line(&foo, 6));
        assert!(idx.touches_line(&foo, 7));
        assert!(!idx.touches_line(&foo, 8));
    }

    #[test]
    fn deleted_file_is_not_registered_as_touched() {
        let dir = tempdir().unwrap();
        let gone = dir.path().join("gone.py");
        let diff = "--- a/gone.py\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-was_here\n-and_here\n";
        let idx = DiffIndex::from_unified_diff(diff, dir.path()).unwrap();
        assert!(
            !idx.touches_file(&gone),
            "a deleted file has no post-image to scope findings against"
        );
    }

    #[test]
    fn empty_diff_touches_nothing() {
        let dir = tempdir().unwrap();
        let foo = dir.path().join("foo.py");
        touch(&foo);
        let idx = DiffIndex::from_unified_diff("", dir.path()).unwrap();
        assert!(!idx.touches_file(&foo));
        assert!(!idx.touches_line(&foo, 1));
    }

    #[test]
    fn range_overlapping_an_added_line_is_touched() {
        let dir = tempdir().unwrap();
        let foo = dir.path().join("foo.py");
        touch(&foo);
        let diff = "--- a/foo.py\n+++ b/foo.py\n@@ -10,3 +10,4 @@\n unchanged1\n+added\n unchanged2\n";
        let idx = DiffIndex::from_unified_diff(diff, dir.path()).unwrap();
        // The addition lands on line 11.
        assert!(idx.touches_range(&foo, 5, 20), "range spanning the addition");
        assert!(idx.touches_range(&foo, 11, 11), "range exactly on the addition");
        assert!(!idx.touches_range(&foo, 12, 40), "range starting after it");
        assert!(!idx.touches_range(&foo, 1, 10), "range ending before it");
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
        let idx = DiffIndex::from_unified_diff(diff, dir.path()).unwrap();
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
        let idx = DiffIndex::from_unified_diff(diff, dir.path()).unwrap();
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
        let idx = DiffIndex::from_unified_diff(diff, dir.path()).unwrap();
        assert!(idx.touches_file(&foo));
        assert!(!idx.touches_line(&foo, 10));
        assert!(!idx.touches_line(&foo, 11));
        // Deletions are tracked separately so callers can fall back to
        // file-scope when issue meaning may have been altered.
        assert!(idx.file_has_deletions(&foo));
    }

    #[test]
    fn addition_only_hunk_does_not_register_deletions() {
        let dir = tempdir().unwrap();
        let foo = dir.path().join("foo.py");
        touch(&foo);
        let diff = "--- a/foo.py\n+++ b/foo.py\n@@ -10,1 +10,2 @@\n unchanged\n+added\n";
        let idx = DiffIndex::from_unified_diff(diff, dir.path()).unwrap();
        assert!(!idx.file_has_deletions(&foo));
    }

    #[test]
    fn paths_resolved_against_project_root() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("src").join("foo.py");
        touch(&nested);
        // `a/`/`b/` prefixes must be stripped; the relative path is joined
        // against project_root and canonicalized.
        let diff = "--- a/src/foo.py\n+++ b/src/foo.py\n@@ -0,0 +1,1 @@\n+new\n";
        let idx = DiffIndex::from_unified_diff(diff, dir.path()).unwrap();
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
        let idx = DiffIndex::from_unified_diff(diff, dir.path()).unwrap();
        assert!(idx.touches_file(&foo));
        assert!(idx.touches_line(&foo, 1));
        assert!(idx.touches_line(&foo, 2));
    }
}
