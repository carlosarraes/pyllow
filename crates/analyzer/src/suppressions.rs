//! `# noqa` suppression scanner.
//!
//! Honors three existing Python conventions — no pyllow-native dialect:
//!
//! - **Inline** `# noqa` (bare → suppress all pyllow rules on that line),
//!   `# noqa: CODE,CODE` (suppress only mapped codes).
//! - **File-level** `# ruff: noqa` and `# flake8: noqa` (with optional code list).
//!
//! External codes (ruff/pyflakes/bugbear/bandit/pycodestyle/flake8-print) map to
//! pyllow rule keys via [`map_external_code`]. Codes pyllow doesn't recognize
//! are silently skipped — they're meant for other tools.
//!
//! Filtering runs in `postprocess::apply()` before baseline so suppressed
//! findings never get baselined and never surface in any output.

use pyllow_config::SuppressEntry;
use pyllow_types::Issue;
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::{Path, PathBuf};

/// Suppression scopes carry kebab-case pyllow rule keys.
/// `None` = nothing suppressed; `All` = bare-noqa style (suppress everything);
/// `Some(set)` = suppress only the listed pyllow rules.
#[derive(Debug, Clone, Default)]
pub enum SuppressionScope {
    #[default]
    None,
    All,
    Some(FxHashSet<&'static str>),
}

impl SuppressionScope {
    pub fn covers(&self, rule_key: &str) -> bool {
        match self {
            Self::None => false,
            Self::All => true,
            Self::Some(set) => set.contains(rule_key),
        }
    }

    fn merge_into(&mut self, other: SuppressionScope) {
        match (&mut *self, other) {
            (_, SuppressionScope::None) => {}
            (Self::All, _) => {}
            (slot, SuppressionScope::All) => *slot = SuppressionScope::All,
            (Self::None, SuppressionScope::Some(set)) => *self = SuppressionScope::Some(set),
            (Self::Some(existing), SuppressionScope::Some(new)) => existing.extend(new),
        }
    }
}

#[derive(Debug, Default)]
pub struct FileSuppressions {
    pub file_level: SuppressionScope,
    pub line_level: FxHashMap<u32, SuppressionScope>,
}

/// Parse a Python source file's noqa directives.
pub fn parse_file_suppressions(source: &str) -> FileSuppressions {
    let mut out = FileSuppressions::default();
    for (idx, raw_line) in source.lines().enumerate() {
        let line_num = (idx + 1) as u32;
        let Some(comment) = extract_comment(raw_line) else {
            continue;
        };

        // File-level: `# ruff: noqa` or `# flake8: noqa` (optionally with codes).
        if let Some(rest) =
            strip_prefix_ci(comment, "ruff:").or_else(|| strip_prefix_ci(comment, "flake8:"))
        {
            if let Some(scope) = parse_noqa_payload(rest.trim_start()) {
                out.file_level.merge_into(scope);
                continue;
            }
        }

        // Inline: `# noqa` or `# noqa: CODE,CODE`.
        if let Some(scope) = parse_noqa_payload(comment) {
            out.line_level
                .entry(line_num)
                .or_default()
                .merge_into(scope);
        }
    }
    out
}

/// Returns the comment body (everything after the first `#` that starts a
/// comment). Skips `#` characters inside the line's string literals using a
/// minimal state machine — adequate for noqa scanning, not for full lexing.
fn extract_comment(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut prev_backslash = false;
    while i < bytes.len() {
        let b = bytes[i];
        if !prev_backslash {
            match b {
                b'\'' if !in_double => in_single = !in_single,
                b'"' if !in_single => in_double = !in_double,
                b'#' if !in_single && !in_double => {
                    return Some(line[i + 1..].trim());
                }
                _ => {}
            }
        }
        prev_backslash = b == b'\\' && !prev_backslash;
        i += 1;
    }
    None
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    // Use `str::get` so we never slice across a UTF-8 char boundary — the
    // comment body can legitimately start with multi-byte chars (e.g. `─`).
    let head = s.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        s.get(prefix.len()..)
    } else {
        None
    }
}

/// Parses `noqa` / `noqa: CODE,CODE` from a comment body. Returns the scope
/// (All for bare noqa, Some(rules) for codes that map, None if no noqa or no
/// codes mapped to pyllow rules).
fn parse_noqa_payload(comment: &str) -> Option<SuppressionScope> {
    let comment = comment.trim_start();
    let rest = strip_prefix_ci(comment, "noqa")?;
    // Word-boundary check: untrimmed `rest` must be empty, start with whitespace,
    // or start with `:` — otherwise this was `noqaSomething`, not a directive.
    let after = rest.trim_start();
    if !after.starts_with(':') {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return Some(SuppressionScope::All);
        }
        return None;
    }
    let codes_section = &after[1..];
    let mut mapped: FxHashSet<&'static str> = FxHashSet::default();
    for token in codes_section.split([',', ' ', '\t']) {
        let code = token.trim();
        if code.is_empty() {
            continue;
        }
        // Stop at the first non-code token (e.g. trailing prose like
        // `# noqa: E711 - Beanie requires == None`).
        if !is_code_token(code) {
            break;
        }
        if let Some(rule) = map_external_code(code) {
            mapped.insert(rule);
        }
    }
    if mapped.is_empty() {
        None
    } else {
        Some(SuppressionScope::Some(mapped))
    }
}

fn is_code_token(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric())
}

/// Map external linter codes to pyllow rule keys. Codes not in the table are
/// ignored (they're for other tools).
pub fn map_external_code(code: &str) -> Option<&'static str> {
    Some(match code {
        // bugbear
        "B006" => "mutable-default",
        "BLE001" => "broad-except",
        // pycodestyle
        "E711" | "E712" => "sentinel-equality",
        "E722" => "broad-except",
        // bandit
        "S110" | "S112" => "broad-except",
        // flake8-print
        "T201" | "T203" => "stray-print",
        // pyflakes
        "F401" => "unused-import",
        _ => return None,
    })
}

/// Filter `issues` in place, dropping ones suppressed by noqa directives in
/// their source files. Returns the count dropped.
pub fn filter(issues: &mut Vec<Issue>, _project_root: &Path) -> usize {
    let mut cache: FxHashMap<PathBuf, FileSuppressions> = FxHashMap::default();
    let before = issues.len();
    issues.retain(|issue| !is_suppressed(issue, &mut cache));
    before - issues.len()
}

/// Filter `issues` in place against pyllow-native `[[suppress]]` config
/// entries. Returns the count dropped. ParseError issues bypass suppression
/// for the same reason noqa does — a blanket suppress shouldn't hide
/// unparseable files.
pub fn apply_config_suppress(
    issues: &mut Vec<Issue>,
    entries: &[SuppressEntry],
    project_root: &Path,
) -> usize {
    if entries.is_empty() {
        return 0;
    }
    let before = issues.len();
    issues.retain(|issue| !suppressed_by_config(issue, entries, project_root));
    before - issues.len()
}

fn suppressed_by_config(issue: &Issue, entries: &[SuppressEntry], project_root: &Path) -> bool {
    if matches!(issue, Issue::ParseError { .. }) {
        return false;
    }
    let abs = issue.path();
    // Match against project-root-relative path so `path = "src/foo.py"` in
    // pyllow.toml works against the canonical absolute path of the issue.
    let rel: &Path = abs.strip_prefix(project_root).unwrap_or(abs);
    let issue_line = issue.line();
    let rule_key = issue.rule_key();

    entries.iter().any(|entry| {
        if !entry.path_glob.is_match(rel) {
            return false;
        }
        // Empty rules list = suppress every pyllow rule at this path.
        if !entry.rules.is_empty() && !entry.rules.contains(rule_key) {
            return false;
        }
        // Entry without a line targets the whole file.
        match entry.line {
            Some(line) => issue_line == Some(line),
            None => true,
        }
    })
}

fn is_suppressed(issue: &Issue, cache: &mut FxHashMap<PathBuf, FileSuppressions>) -> bool {
    // Parse failures bypass noqa. A blanket `# ruff: noqa` would otherwise
    // hide an unparseable file, undoing the fail-fast guarantee that
    // ParseError exists for in the first place.
    if matches!(issue, Issue::ParseError { .. }) {
        return false;
    }
    let path = issue.path().to_path_buf();
    if path.as_os_str().is_empty() {
        return false;
    }
    let suppressions = cache.entry(path.clone()).or_insert_with(|| {
        std::fs::read_to_string(&path)
            .map(|src| parse_file_suppressions(&src))
            .unwrap_or_default()
    });
    let rule = issue.rule_key();
    if suppressions.file_level.covers(rule) {
        return true;
    }
    let Some(line) = issue.line() else {
        return false;
    };
    suppressions
        .line_level
        .get(&line)
        .map(|s| s.covers(rule))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_types::SmellRule;

    #[test]
    fn extracts_comment_skipping_strings() {
        assert_eq!(extract_comment("x = 1  # noqa"), Some("noqa"));
        assert_eq!(
            extract_comment("x = \"# not a comment\"  # noqa"),
            Some("noqa")
        );
        assert_eq!(extract_comment("'#inside'  # real"), Some("real"));
        assert_eq!(extract_comment("no comment here"), None);
    }

    #[test]
    fn bare_noqa_is_all() {
        assert!(matches!(
            parse_noqa_payload("noqa"),
            Some(SuppressionScope::All)
        ));
        assert!(matches!(
            parse_noqa_payload("noqa  trailing prose"),
            Some(SuppressionScope::All)
        ));
    }

    #[test]
    fn coded_noqa_maps_to_rules() {
        let scope = parse_noqa_payload("noqa: E711, E712").unwrap();
        match scope {
            SuppressionScope::Some(set) => {
                assert!(set.contains("sentinel-equality"));
                assert_eq!(set.len(), 1); // both E711+E712 collapse to same rule
            }
            _ => panic!("expected Some"),
        }
    }

    #[test]
    fn coded_noqa_with_trailing_prose() {
        let scope = parse_noqa_payload("noqa: E711 - Beanie requires == None").unwrap();
        match scope {
            SuppressionScope::Some(set) => assert!(set.contains("sentinel-equality")),
            _ => panic!("expected Some"),
        }
    }

    #[test]
    fn unknown_codes_yield_no_suppression() {
        assert!(parse_noqa_payload("noqa: PLW1234").is_none());
    }

    #[test]
    fn parses_file_level_ruff_directive() {
        let src = "# ruff: noqa: T201\n\nprint(\"x\")\n";
        let s = parse_file_suppressions(src);
        assert!(s.file_level.covers("stray-print"));
        assert!(!s.file_level.covers("broad-except"));
    }

    #[test]
    fn parses_file_level_flake8_directive() {
        let src = "# flake8: noqa\nfoo == None\n";
        let s = parse_file_suppressions(src);
        assert!(s.file_level.covers("anything"));
    }

    #[test]
    fn parses_inline_directive_per_line() {
        let src = "x = []\nif len(x) > 0:  # noqa: E711\n    print(x)  # noqa: T201\n";
        let s = parse_file_suppressions(src);
        // Line 2: E711 → sentinel-equality
        assert!(s.line_level[&2].covers("sentinel-equality"));
        assert!(!s.line_level[&2].covers("stray-print"));
        // Line 3: T201 → stray-print
        assert!(s.line_level[&3].covers("stray-print"));
    }

    #[test]
    fn filter_drops_line_suppressed_smell() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.py");
        std::fs::write(&path, "x = []\nif len(x) > 0: pass  # noqa: E711\n").unwrap();
        let mut issues = vec![Issue::Smell {
            path: path.clone(),
            line: 2,
            rule: SmellRule::SentinelEquality,
            detail: String::new(),
        }];
        let dropped = filter(&mut issues, dir.path());
        assert_eq!(dropped, 1);
        assert!(issues.is_empty());
    }

    #[test]
    fn filter_drops_file_suppressed_smell_on_any_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.py");
        std::fs::write(&path, "# ruff: noqa: T201\n\nprint(\"x\")\nprint(\"y\")\n").unwrap();
        let mut issues = vec![
            Issue::Smell {
                path: path.clone(),
                line: 3,
                rule: SmellRule::StrayPrint,
                detail: String::new(),
            },
            Issue::Smell {
                path: path.clone(),
                line: 4,
                rule: SmellRule::StrayPrint,
                detail: String::new(),
            },
        ];
        let dropped = filter(&mut issues, dir.path());
        assert_eq!(dropped, 2);
    }

    #[test]
    fn filter_does_not_suppress_parse_errors_via_file_level_noqa() {
        // A blanket file-level `# ruff: noqa` must NOT hide a parse
        // failure — if the file is unparseable, the noqa scanner can't be
        // trusted to have read intent correctly anyway, and CI needs to
        // know.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.py");
        std::fs::write(&path, "# ruff: noqa\ndef\n").unwrap();
        let mut issues = vec![Issue::ParseError {
            path: path.clone(),
            message: "syntax error".into(),
        }];
        let dropped = filter(&mut issues, dir.path());
        assert_eq!(dropped, 0);
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn filter_keeps_unmatched_rules() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.py");
        std::fs::write(&path, "x = []\nif len(x) > 0: pass  # noqa: E711\n").unwrap();
        // Smell on line 2 that ISN'T sentinel-equality (E711 doesn't map to it).
        let mut issues = vec![Issue::Smell {
            path: path.clone(),
            line: 2,
            rule: SmellRule::TruthyLengthCheck,
            detail: String::new(),
        }];
        let dropped = filter(&mut issues, dir.path());
        assert_eq!(dropped, 0);
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn bare_noqa_suppresses_anything_on_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.py");
        std::fs::write(&path, "import os  # noqa\n").unwrap();
        let mut issues = vec![Issue::UnusedImport {
            path: path.clone(),
            line: 1,
            name: "os".into(),
            module: "os".into(),
        }];
        let dropped = filter(&mut issues, dir.path());
        assert_eq!(dropped, 1);
    }

    #[test]
    fn f401_maps_to_unused_import() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.py");
        std::fs::write(&path, "import os  # noqa: F401\n").unwrap();
        let mut issues = vec![Issue::UnusedImport {
            path: path.clone(),
            line: 1,
            name: "os".into(),
            module: "os".into(),
        }];
        let dropped = filter(&mut issues, dir.path());
        assert_eq!(dropped, 1);
    }

    fn mk_entry(path: &str, rules: &[&str], line: Option<u32>) -> pyllow_config::SuppressEntry {
        pyllow_config::SuppressEntry {
            path_glob: globset::Glob::new(path).unwrap().compile_matcher(),
            rules: rules.iter().map(|s| s.to_string()).collect(),
            line,
            reason: None,
        }
    }

    fn mk_smell(path: PathBuf, line: u32, rule: SmellRule) -> Issue {
        Issue::Smell {
            path,
            line,
            rule,
            detail: String::new(),
        }
    }

    #[test]
    fn config_suppress_drops_matching_rule() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        let foo = project.join("src").join("foo.py");
        let mut issues = vec![mk_smell(foo.clone(), 10, SmellRule::BroadExcept)];
        let entries = vec![mk_entry("src/foo.py", &["broad-except"], None)];
        let dropped = apply_config_suppress(&mut issues, &entries, project);
        assert_eq!(dropped, 1);
        assert!(issues.is_empty());
    }

    #[test]
    fn config_suppress_keeps_when_rule_differs() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        let foo = project.join("src").join("foo.py");
        let mut issues = vec![mk_smell(foo, 10, SmellRule::BroadExcept)];
        let entries = vec![mk_entry("src/foo.py", &["mutable-default"], None)];
        let dropped = apply_config_suppress(&mut issues, &entries, project);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn config_suppress_keeps_when_path_differs() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        let other = project.join("src").join("other.py");
        let mut issues = vec![mk_smell(other, 10, SmellRule::BroadExcept)];
        let entries = vec![mk_entry("src/foo.py", &["broad-except"], None)];
        let dropped = apply_config_suppress(&mut issues, &entries, project);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn config_suppress_drops_only_matching_line() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        let foo = project.join("src").join("foo.py");
        let mut issues = vec![
            mk_smell(foo.clone(), 10, SmellRule::BroadExcept),
            mk_smell(foo, 20, SmellRule::BroadExcept),
        ];
        let entries = vec![mk_entry("src/foo.py", &["broad-except"], Some(10))];
        let dropped = apply_config_suppress(&mut issues, &entries, project);
        assert_eq!(dropped, 1);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].line(), Some(20));
    }

    #[test]
    fn config_suppress_with_empty_rules_drops_any_rule() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        let foo = project.join("src").join("foo.py");
        let mut issues = vec![
            mk_smell(foo.clone(), 10, SmellRule::BroadExcept),
            mk_smell(foo, 20, SmellRule::MutableDefault),
        ];
        let entries = vec![mk_entry("src/foo.py", &[], None)];
        let dropped = apply_config_suppress(&mut issues, &entries, project);
        assert_eq!(dropped, 2);
        assert!(issues.is_empty());
    }

    #[test]
    fn config_suppress_does_not_drop_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        let foo = project.join("src").join("foo.py");
        let mut issues = vec![Issue::ParseError {
            path: foo,
            message: "bad syntax".into(),
        }];
        // Even with an aggressive "match everything" entry, ParseError survives.
        let entries = vec![mk_entry("**/*.py", &[], None)];
        let dropped = apply_config_suppress(&mut issues, &entries, project);
        assert_eq!(dropped, 0);
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn config_suppress_supports_glob_wildcards() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        let foo = project.join("tests").join("unit").join("foo.py");
        let mut issues = vec![mk_smell(foo, 10, SmellRule::StrayPrint)];
        let entries = vec![mk_entry("tests/**/*.py", &["stray-print"], None)];
        let dropped = apply_config_suppress(&mut issues, &entries, project);
        assert_eq!(dropped, 1);
    }
}
