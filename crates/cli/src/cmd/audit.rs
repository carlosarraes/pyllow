use crate::postprocess::{
    apply, handle_snapshot, note_baseline_filter, render_ownership, render_score, PostFlags,
};
use crate::report::Format;
use anyhow::{Context, Result};
use colored::Colorize;
use pyllow_analyzer::diff::DiffIndex;
use pyllow_analyzer::dupes::{run_with_files as run_dupes, DupesOptions};
use pyllow_analyzer::health::{analyze as run_health, HealthOptions};
use pyllow_analyzer::smells::analyze as run_smells;
use pyllow_analyzer::{analyze_with_parsed, discover_python_files, resolve_package_roots};
use pyllow_types::{AnalysisResults, AnalysisStats, Issue, SmellRule};
use rustc_hash::FxHashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

/// How an audit run constrains its issue set. `File` keeps any issue whose
/// path appears in the changed-file set (existing `--base` behavior); `Line`
/// additionally requires the issue's line(s) to overlap with `+` lines in a
/// supplied unified diff, falling back to file-level matching for line-less
/// issue kinds (hotspot, low-maintainability, circular-dependency).
enum AuditScope {
    File(FxHashSet<PathBuf>),
    Line(DiffIndex),
}

impl AuditScope {
    fn contains(&self, issue: &Issue) -> bool {
        match self {
            AuditScope::File(changed) => issue_in_file_scope(issue, changed),
            AuditScope::Line(diff) => issue_in_diff_scope(issue, diff),
        }
    }

    fn is_empty_set(&self) -> bool {
        match self {
            AuditScope::File(set) => set.is_empty(),
            AuditScope::Line(diff) => diff.is_empty(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Pass,
    Warn,
    Fail,
}

impl Verdict {
    fn label(&self) -> colored::ColoredString {
        match self {
            Verdict::Pass => "PASS".green().bold(),
            Verdict::Warn => "WARN".yellow().bold(),
            Verdict::Fail => "FAIL".red().bold(),
        }
    }
    fn is_fail(&self) -> bool {
        matches!(self, Verdict::Fail)
    }
}

pub fn run(
    path: PathBuf,
    base: String,
    diff_file: Option<PathBuf>,
    max_issues: usize,
    format: Format,
    post: PostFlags,
) -> Result<bool> {
    let (config, project_root) = super::load_config(&path)?;
    let started = Instant::now();
    let scope = build_scope(&base, diff_file.as_deref(), &project_root)?;
    if scope.is_empty_set() {
        match &scope {
            AuditScope::File(_) => {
                eprintln!("warning: no files changed since {base} (audit will be empty)")
            }
            AuditScope::Line(_) => {
                eprintln!("warning: --diff-file is empty (audit will be empty)")
            }
        }
    }

    let (mut analysis, parsed) = analyze_with_parsed(&config).context("check analysis failed")?;
    let mut all_issues: Vec<Issue> = std::mem::take(&mut analysis.issues);

    let package_roots = resolve_package_roots(&config).context("resolving package roots")?;
    let files = discover_python_files(&project_root, &package_roots, &config);

    let health_opts = HealthOptions::default();
    all_issues.extend(run_dupes(&files, DupesOptions::default()));
    all_issues.extend(run_health(&parsed, &project_root, health_opts));
    let smells_opts = super::smells::options_from_config(&config, 5);
    all_issues.extend(run_smells(&parsed, &smells_opts));

    let total_before = all_issues.len();
    all_issues.retain(|i| scope.contains(i));

    let mut results_for_baseline = AnalysisResults {
        stats: AnalysisStats::default(),
        issues: std::mem::take(&mut all_issues),
        executed_rules: Vec::new(),
    };
    let suppressed = apply(&mut results_for_baseline, &project_root, &post)?;
    note_baseline_filter(suppressed, &post.baseline);
    all_issues = results_for_baseline.issues;
    let in_scope = all_issues.len();

    let verdict = if in_scope == 0 {
        Verdict::Pass
    } else if in_scope <= max_issues {
        Verdict::Warn
    } else {
        Verdict::Fail
    };

    let scope_label = match &scope {
        AuditScope::File(set) => format!(
            "{} changed file{} since {}",
            set.len(),
            if set.len() == 1 { "" } else { "s" },
            base
        ),
        AuditScope::Line(_) => format!(
            "lines from --diff-file {}",
            diff_file
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        ),
    };
    eprintln!("auditing {scope_label} ({in_scope} of {total_before} issues in scope)");

    let results = AnalysisResults {
        stats: AnalysisStats {
            files_scanned: files.len(),
            entry_points: 0,
            plugins_run: Vec::new(),
            elapsed_ms: started.elapsed().as_millis() as u64,
        },
        issues: all_issues,
        executed_rules: executed_rules(&health_opts, &smells_opts),
    };
    format.print(&results, &project_root)?;
    render_score(&results, &post, format);
    render_ownership(&results, &project_root, &post, format);
    handle_snapshot(&results, &post, format)?;
    eprintln!(
        "{} {} {} ({} ms)",
        "verdict:".dimmed(),
        verdict.label(),
        format!(
            "{} issue{} in PR scope",
            in_scope,
            if in_scope == 1 { "" } else { "s" }
        )
        .dimmed(),
        results.stats.elapsed_ms
    );

    Ok(verdict.is_fail())
}

fn build_scope(base: &str, diff_file: Option<&Path>, project_root: &Path) -> Result<AuditScope> {
    if let Some(diff_path) = diff_file {
        let raw = fs::read_to_string(diff_path)
            .with_context(|| format!("reading --diff-file {}", diff_path.display()))?;
        let index = DiffIndex::from_unified_diff(&raw, project_root)
            .with_context(|| format!("parsing --diff-file {}", diff_path.display()))?;
        Ok(AuditScope::Line(index))
    } else {
        Ok(AuditScope::File(changed_files_since(project_root, base)?))
    }
}

/// Audit composes four passes; its executed-rule metadata is their union.
fn executed_rules(
    health_opts: &HealthOptions,
    smells_opts: &pyllow_analyzer::smells::SmellsOptions,
) -> Vec<String> {
    let mut rules: Vec<String> = pyllow_analyzer::REACHABILITY_RULES
        .iter()
        .map(|r| r.to_string())
        .collect();
    rules.push("duplicate".to_string());
    rules.extend(pyllow_analyzer::health::executed_rules(health_opts));
    rules.extend(super::smells::executed_smell_rules(smells_opts));
    rules
}

fn issue_in_file_scope(issue: &Issue, changed: &FxHashSet<PathBuf>) -> bool {
    match issue {
        Issue::Duplicate { occurrences, .. } => occurrences
            .iter()
            .any(|o| canonical_in_set(&o.path, changed)),
        // Cycles span N files; `issue.path()` is the first sorted member, so
        // a PR that only edits another file in the same cycle would slip
        // past the gate. Match if any cycle member changed.
        Issue::CircularDependency { cycle } => cycle.iter().any(|p| canonical_in_set(p, changed)),
        _ => canonical_in_set(issue.path(), changed),
    }
}

fn issue_in_diff_scope(issue: &Issue, diff: &DiffIndex) -> bool {
    match issue {
        // Match any occurrence whose [start..=end] range intersects a touched
        // line; fallow parity (an N-file clone passes if even one copy moved).
        Issue::Duplicate { occurrences, .. } => occurrences
            .iter()
            .any(|o| diff.touches_range(&o.path, o.start_line, o.end_line)),
        // Cycles span N files with no line info — fall back to file-touched.
        Issue::CircularDependency { cycle } => cycle.iter().any(|p| diff.touches_file(p)),
        // Everything else matches when any added line overlaps the issue's
        // own range. Function-scoped issues carry their whole body, so a
        // branch added deep inside a function is caught without widening
        // the finding to the entire file.
        other => match other.range() {
            Some((start, end)) => {
                diff.touches_range(other.path(), start, end)
                    || (deletions_can_invalidate(other) && diff.file_has_deletions(other.path()))
            }
            None => diff.touches_file(other.path()),
        },
    }
}

/// Whether deletions elsewhere in the file can make this issue newly valid on
/// an unchanged line.
///
/// Two families qualify, both because their truth depends on lines other than
/// the one they are reported at:
///
/// - `UnusedImport` — removing the last usage of a symbol makes its `import`
///   line newly unused without touching the import statement itself.
/// - `Smell(HighTodoDensity)` — density is TODOs over LOC, so deleting real
///   code raises the ratio without touching any TODO comment.
///
/// Everything else is tied to a specific construct: an unrelated comment
/// deletion must not drag a pre-existing `broad-except` into scope. Function
/// metrics are excluded deliberately — deletions only ever *reduce*
/// complexity, so they cannot newly create a `Complexity`/`RefactorTarget`.
fn deletions_can_invalidate(issue: &Issue) -> bool {
    matches!(
        issue,
        Issue::UnusedImport { .. }
            | Issue::Smell {
                rule: SmellRule::HighTodoDensity,
                ..
            }
    )
}

fn canonical_in_set(path: &Path, set: &FxHashSet<PathBuf>) -> bool {
    path.canonicalize()
        .map(|c| set.contains(&c))
        .unwrap_or(false)
}

fn changed_files_since(project_root: &Path, base: &str) -> Result<FxHashSet<PathBuf>> {
    // `--relative` forces git to emit paths relative to the current working
    // directory (which we set to `project_root`). Without it, monorepos
    // where the project root is a subdirectory of the git repo (e.g.
    // `mondrio/backend/`) would receive paths like `backend/src/foo.py`
    // and the subsequent `project_root.join(...)` would produce a doubled
    // path that doesn't exist — silently dropping every "changed file"
    // and turning audit into a permanent PASS.
    let output = Command::new("git")
        .args([
            "diff",
            "--name-only",
            "--relative",
            &format!("{base}...HEAD"),
        ])
        .current_dir(project_root)
        .output()
        .context("running git diff")?;
    if !output.status.success() {
        anyhow::bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut set = FxHashSet::default();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let abs = project_root.join(line);
        if let Ok(canonical) = abs.canonicalize() {
            set.insert(canonical);
        }
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_types::{DuplicateOccurrence, SmellRule};
    use std::fs;
    use tempfile::tempdir;

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, "").unwrap();
    }

    fn diff_touching_lines_11(path_str: &str) -> String {
        format!("--- a/{path_str}\n+++ b/{path_str}\n@@ -10,1 +10,2 @@\n unchanged\n+added\n")
    }

    #[test]
    fn diff_scope_keeps_smell_on_touched_line() {
        let dir = tempdir().unwrap();
        let foo = dir.path().join("foo.py");
        touch(&foo);
        let scope = line_scope(
            &diff_touching_lines_11("foo.py"),
            dir.path(),
        );
        let issue = Issue::Smell {
            path: foo.clone(),
            line: 11,
            rule: SmellRule::MutableDefault,
            detail: String::new(),
        };
        assert!(scope.contains(&issue));
    }

    #[test]
    fn diff_scope_drops_smell_on_untouched_line() {
        let dir = tempdir().unwrap();
        let foo = dir.path().join("foo.py");
        touch(&foo);
        let scope = line_scope(
            &diff_touching_lines_11("foo.py"),
            dir.path(),
        );
        let issue = Issue::Smell {
            path: foo,
            line: 20,
            rule: SmellRule::MutableDefault,
            detail: String::new(),
        };
        assert!(!scope.contains(&issue));
    }

    #[test]
    fn diff_scope_keeps_circular_dep_when_any_cycle_member_touched() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a.py");
        let b = dir.path().join("b.py");
        let c = dir.path().join("c.py");
        touch(&a);
        touch(&b);
        touch(&c);
        let scope = line_scope(
            &diff_touching_lines_11("b.py"),
            dir.path(),
        );
        let issue = Issue::CircularDependency {
            cycle: vec![a, b, c],
        };
        assert!(scope.contains(&issue));
    }

    #[test]
    fn diff_scope_keeps_duplicate_when_occurrence_range_intersects_diff() {
        let dir = tempdir().unwrap();
        let foo = dir.path().join("foo.py");
        touch(&foo);
        // Diff touches line 11; duplicate spans lines 10..=15 → intersects.
        let scope = line_scope(
            &diff_touching_lines_11("foo.py"),
            dir.path(),
        );
        let issue = Issue::Duplicate {
            token_count: 30,
            occurrences: vec![DuplicateOccurrence {
                path: foo,
                start_line: 10,
                end_line: 15,
            }],
        };
        assert!(scope.contains(&issue));
    }

    #[test]
    fn diff_scope_drops_duplicate_when_no_occurrence_in_diff() {
        let dir = tempdir().unwrap();
        let foo = dir.path().join("foo.py");
        touch(&foo);
        // Diff touches only line 11; duplicate is at lines 50..=55 → no overlap.
        let scope = line_scope(
            &diff_touching_lines_11("foo.py"),
            dir.path(),
        );
        let issue = Issue::Duplicate {
            token_count: 30,
            occurrences: vec![DuplicateOccurrence {
                path: foo,
                start_line: 50,
                end_line: 55,
            }],
        };
        assert!(!scope.contains(&issue));
    }

    fn line_scope(diff: &str, root: &std::path::Path) -> AuditScope {
        AuditScope::Line(DiffIndex::from_unified_diff(diff, root).expect("valid test diff"))
    }

    fn complexity_at(path: std::path::PathBuf, line: u32, end_line: u32) -> Issue {
        Issue::Complexity {
            path,
            line,
            end_line,
            function: "process".into(),
            cyclomatic: 12,
            cognitive: 18,
        }
    }

    #[test]
    fn diff_scope_keeps_complexity_when_body_grows_without_touching_def() {
        // Adding a branch inside the body without touching `def` still
        // alters the function's complexity. The issue now carries the body
        // range, so this matches by range overlap rather than by falling
        // back to whole-file scoping.
        let dir = tempdir().unwrap();
        let foo = dir.path().join("foo.py");
        touch(&foo);
        // Pure addition deep in the file — no deletion, no edit at line 5.
        let addition_only = "--- a/foo.py\n+++ b/foo.py\n@@ -40,1 +40,3 @@\n existing\n+    if new_branch:\n+        do_thing()\n";
        let scope = line_scope(addition_only, dir.path());
        // `def` at 5, body runs through 45 — the addition at 41 is inside it.
        let issue = complexity_at(foo, 5, 45);
        assert!(
            scope.contains(&issue),
            "complexity must stay in scope when an added line falls inside its body range"
        );
    }

    // #6: without a body range, any edit anywhere in the file dragged every
    // complexity finding into scope. A finding whose body ends well before
    // the edit is not implicated by it.
    #[test]
    fn diff_scope_drops_complexity_when_addition_is_outside_the_body() {
        let dir = tempdir().unwrap();
        let foo = dir.path().join("foo.py");
        touch(&foo);
        let addition_only = "--- a/foo.py\n+++ b/foo.py\n@@ -40,1 +40,3 @@\n existing\n+    if new_branch:\n+        do_thing()\n";
        let scope = line_scope(addition_only, dir.path());
        // `def` at 5, body ends at 20 — the addition at 41 is a different function.
        let issue = complexity_at(foo, 5, 20);
        assert!(
            !scope.contains(&issue),
            "complexity must not be dragged in by an edit outside its body range"
        );
    }

    // #6 "deletion-only behavior is explicit per issue family": high-todo-density
    // is TODOs divided by LOC, so deleting code *raises* it. It is the only
    // line-reported smell whose truth depends on lines other than its own.
    #[test]
    fn diff_scope_keeps_todo_density_in_file_with_only_deletions() {
        let dir = tempdir().unwrap();
        let foo = dir.path().join("foo.py");
        touch(&foo);
        let deletion_only =
            "--- a/foo.py\n+++ b/foo.py\n@@ -42,3 +42,2 @@\n keep1\n-real_code()\n keep2\n";
        let scope = line_scope(deletion_only, dir.path());
        let smell = Issue::Smell {
            path: foo,
            line: 5,
            rule: SmellRule::HighTodoDensity,
            detail: String::new(),
        };
        assert!(
            scope.contains(&smell),
            "deleting code raises TODO density, so the finding is newly implicated"
        );
    }

    #[test]
    fn diff_scope_drops_localized_smell_in_file_with_unrelated_deletion() {
        // Pi P2: a comment deletion shouldn't drag every pre-existing
        // broad-except into scope. Smells are tied to a specific line of
        // code, not to file-wide state — the deletion fallback should
        // only apply to issues whose meaning can change because of remote
        // deletions (UnusedImport, Complexity, RefactorTarget).
        let dir = tempdir().unwrap();
        let foo = dir.path().join("foo.py");
        touch(&foo);
        let deletion_only =
            "--- a/foo.py\n+++ b/foo.py\n@@ -42,3 +42,2 @@\n keep1\n-# removed comment\n keep2\n";
        let scope = line_scope(deletion_only, dir.path());
        let smell = Issue::Smell {
            path: foo,
            line: 5, // unchanged line, far from the deletion
            rule: SmellRule::BroadExcept,
            detail: String::new(),
        };
        assert!(
            !scope.contains(&smell),
            "broad-except at an unchanged line should not be pulled into scope by an unrelated deletion"
        );
    }

    #[test]
    fn diff_scope_keeps_unused_import_when_deletion_removed_its_usage() {
        // Pi regression: a PR that deletes the last usage of an imported
        // symbol surfaces `UnusedImport` on the unchanged `import` line.
        // Pure line-based scoping would silently drop the issue and the
        // audit would falsely pass. The deletion fallback keeps it in scope.
        let dir = tempdir().unwrap();
        let foo = dir.path().join("foo.py");
        touch(&foo);
        let deletion_only =
            "--- a/foo.py\n+++ b/foo.py\n@@ -10,3 +10,2 @@\n keep1\n-removed_usage\n keep2\n";
        let scope = line_scope(deletion_only, dir.path());
        let issue = Issue::UnusedImport {
            path: foo,
            line: 1, // unchanged `import` line
            name: "os".into(),
            module: "os".into(),
        };
        assert!(
            scope.contains(&issue),
            "deletion fallback should keep an UnusedImport on an unchanged line"
        );
    }

    #[test]
    fn diff_scope_keeps_lineless_issue_on_touched_file() {
        let dir = tempdir().unwrap();
        let foo = dir.path().join("foo.py");
        touch(&foo);
        let scope = line_scope(
            &diff_touching_lines_11("foo.py"),
            dir.path(),
        );
        // Hotspot has no line; touched file is enough.
        let issue = Issue::Hotspot {
            path: foo,
            cyclomatic: 30,
            churn: 10,
            score: 7.5,
        };
        assert!(scope.contains(&issue));
    }
}
