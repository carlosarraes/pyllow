use crate::postprocess::{
    apply, handle_snapshot, note_baseline_filter, render_ownership, render_score, PostFlags,
};
use crate::report::Format;
use anyhow::{Context, Result};
use colored::Colorize;
use pyllow_analyzer::diff::DiffIndex;
use pyllow_analyzer::dupes::{run_with_files as run_dupes, DupesOptions};
use pyllow_analyzer::health::{analyze as run_health, HealthOptions};
use pyllow_analyzer::smells::analyze_collect as run_smells;
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
        // A file pyllow could not parse was excluded from every other check.
        // That is a completeness failure of the whole run, not a finding on
        // particular lines, so scoping never hides it (#8: incomplete
        // analysis cannot return clean).
        if matches!(issue, Issue::ParseError { .. }) {
            return true;
        }
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

/// An analysis family `audit --only` can select.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, clap::ValueEnum)]
pub enum Family {
    /// Reachability: unused files/imports/deps, cycles, boundaries.
    Check,
    Dupes,
    Health,
    Smells,
}

impl Family {
    const ALL: [Family; 4] = [Family::Check, Family::Dupes, Family::Health, Family::Smells];

    fn as_str(self) -> &'static str {
        match self {
            Family::Check => "check",
            Family::Dupes => "dupes",
            Family::Health => "health",
            Family::Smells => "smells",
        }
    }
}

/// Raw `--only` / `--rule` values from the CLI.
#[derive(Debug, Default)]
pub struct SelectionArgs {
    pub families: Vec<Family>,
    pub rules: Vec<String>,
}

/// Validated selection: which families run, and (optionally) which rule keys
/// survive filtering. `rules == None` means every rule in the families.
struct ResolvedSelection {
    families: Vec<Family>,
    rules: Option<FxHashSet<String>>,
    executed_rules: Vec<String>,
    requested_families: Vec<String>,
    requested_rules: Vec<String>,
}

impl ResolvedSelection {
    fn runs(&self, family: Family) -> bool {
        self.families.contains(&family)
    }

    /// Whether an issue survives `--rule` filtering. Parse errors always do:
    /// an unparseable file was excluded from every other check, so hiding it
    /// would let an incomplete analysis report clean.
    fn keeps(&self, issue: &Issue) -> bool {
        if matches!(issue, Issue::ParseError { .. }) {
            return true;
        }
        match &self.rules {
            None => true,
            Some(set) => set.contains(issue.rule_key().as_ref()),
        }
    }
}

/// Validate selectors against the rule catalog *before* any scanning. Unknown
/// rules, rules outside the selected families, and rules the config has
/// disabled are all rejected — each would otherwise produce an empty,
/// passing gate.
fn resolve_selection(
    args: SelectionArgs,
    config: &pyllow_config::ResolvedConfig,
    health_opts: &HealthOptions,
    smells_opts: &pyllow_analyzer::smells::SmellsOptions,
) -> Result<ResolvedSelection> {
    let mut families: Vec<Family> = if args.families.is_empty() {
        Family::ALL.to_vec()
    } else {
        let mut seen = FxHashSet::default();
        args.families
            .iter()
            .copied()
            .filter(|f| seen.insert(*f))
            .collect()
    };
    families.sort_by_key(|f| Family::ALL.iter().position(|x| x == f));

    let catalog = |family: Family| -> Vec<String> {
        match family {
            Family::Check => pyllow_analyzer::REACHABILITY_RULES
                .iter()
                .map(|r| r.to_string())
                .collect(),
            Family::Dupes => vec!["duplicate".to_string()],
            Family::Health => pyllow_analyzer::health::executed_rules(health_opts),
            Family::Smells => {
                let mut rules = super::smells::executed_smell_rules(smells_opts);
                if smells_opts.enabled.contains(&SmellRule::BannedApi) {
                    rules.extend(config.smells_banned_apis.iter().map(|b| b.id.clone()));
                }
                rules
            }
        }
    };
    let available: Vec<String> = families.iter().flat_map(|f| catalog(*f)).collect();

    let rules = if args.rules.is_empty() {
        None
    } else {
        let family_names: Vec<&str> = families.iter().map(|f| f.as_str()).collect();
        let mut set = FxHashSet::default();
        for rule in &args.rules {
            if available.contains(rule) {
                set.insert(rule.clone());
                continue;
            }
            // Known smell rule the config has turned off?
            if let Ok(smell) = rule.parse::<SmellRule>() {
                if families.contains(&Family::Smells) && !smells_opts.enabled.contains(&smell) {
                    anyhow::bail!(
                        "rule `{rule}` is disabled by config; add it to [smells].enabled to select it"
                    );
                }
            }
            let in_other_family = Family::ALL
                .iter()
                .filter(|f| !families.contains(f))
                .find(|f| catalog(**f).contains(rule));
            match in_other_family {
                Some(f) => anyhow::bail!(
                    "rule `{rule}` belongs to family `{}`, which is not selected (selected: {})",
                    f.as_str(),
                    family_names.join(", ")
                ),
                None => anyhow::bail!(
                    "unknown rule `{rule}` (available in {}: {})",
                    family_names.join(", "),
                    available.join(", ")
                ),
            }
        }
        Some(set)
    };

    let executed_rules = match &rules {
        None => available,
        Some(set) => available.into_iter().filter(|r| set.contains(r)).collect(),
    };

    Ok(ResolvedSelection {
        families,
        rules,
        executed_rules,
        requested_families: args
            .families
            .iter()
            .map(|f| f.as_str().to_string())
            .collect(),
        requested_rules: args.rules,
    })
}

/// Everything a staged-mode run needs. The `TempDir` owns the snapshot and
/// removes it when this drops — on success and on every error path alike.
struct StagedContext {
    tmp: tempfile::TempDir,
    /// Snapshot equivalent of the real project root (staged config included).
    snapshot_project_root: PathBuf,
    real_toplevel: PathBuf,
    /// `git diff --cached --no-renames` — renames appear as delete + add, so
    /// a renamed file is wholly in scope at its post-image path.
    cached_diff: String,
}

fn git_capture(root: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .with_context(|| format!("running git {args:?}"))?;
    if !out.status.success() {
        anyhow::bail!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Materialize the staged index into a temp dir. `Ok(None)` means no staged
/// Python changes — the gate passes without running analysis. Every git
/// failure is an error (exit 2), never an empty result: a broken repo must
/// not look like a clean one.
fn build_staged(path: &Path) -> Result<Option<StagedContext>> {
    let toplevel = PathBuf::from(git_capture(path, &["rev-parse", "--show-toplevel"])?.trim());

    let staged_names = git_capture(
        &toplevel,
        &["diff", "--cached", "--no-renames", "--name-only", "-z"],
    )?;
    if !staged_names.split('\0').any(|n| n.ends_with(".py")) {
        return Ok(None);
    }
    let cached_diff = git_capture(
        &toplevel,
        &[
            "diff",
            "--cached",
            "--no-renames",
            "--no-ext-diff",
            "--no-color",
        ],
    )?;

    let tmp = tempfile::TempDir::new().context("creating staged snapshot dir")?;
    // Trailing slash is required — without it git treats the prefix as a
    // filename prefix, not a directory.
    let prefix = format!("{}/", tmp.path().display());
    git_capture(
        &toplevel,
        &["checkout-index", "-a", &format!("--prefix={prefix}")],
    )
    .context("materializing staged index (checkout-index)")?;

    // The audited path may be a subdirectory of the repo; mirror it inside
    // the snapshot so config discovery sees the same layout.
    let canonical_path = path.canonicalize().context("resolving audit path")?;
    let canonical_top = toplevel.canonicalize().context("resolving git toplevel")?;
    let rel = canonical_path
        .strip_prefix(&canonical_top)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let snapshot_project_root = tmp.path().join(rel);
    if !snapshot_project_root.exists() {
        anyhow::bail!(
            "staged snapshot is incomplete: {} is missing",
            snapshot_project_root.display()
        );
    }

    Ok(Some(StagedContext {
        tmp,
        snapshot_project_root,
        real_toplevel: canonical_top,
        cached_diff,
    }))
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    path: PathBuf,
    base: String,
    diff_file: Option<PathBuf>,
    staged: bool,
    max_issues: usize,
    selection: SelectionArgs,
    format: Format,
    post: PostFlags,
) -> Result<bool> {
    let started = Instant::now();

    let staged_ctx = if staged { build_staged(&path)? } else { None };
    if staged && staged_ctx.is_none() {
        // No staged Python changes: pass without running analysis, but still
        // emit a valid (empty) machine document so pipelines can parse it.
        let (config, project_root) = super::load_config(&path)?;
        let health_opts = HealthOptions::default();
        let smells_opts = super::smells::options_from_config(&config, 5);
        let selection = resolve_selection(selection, &config, &health_opts, &smells_opts)?;
        let results = AnalysisResults {
            stats: AnalysisStats::default(),
            issues: Vec::new(),
            executed_rules: Vec::new(),
            selection: Some(pyllow_types::Selection {
                families_requested: selection.requested_families.clone(),
                families_executed: Vec::new(),
                rules_requested: selection.requested_rules.clone(),
            }),
        };
        eprintln!("no staged Python changes — nothing to audit");
        format.print(&results, &project_root)?;
        eprintln!("{} {}", "verdict:".dimmed(), Verdict::Pass.label());
        return Ok(false);
    }

    // In staged mode every downstream step — config, discovery, parsing —
    // reads the snapshot, so the analysis sees exactly the index content.
    let analysis_root = staged_ctx
        .as_ref()
        .map(|c| c.snapshot_project_root.clone())
        .unwrap_or_else(|| path.clone());
    let (config, project_root) = super::load_config(&analysis_root)?;

    let health_opts = HealthOptions::default();
    let smells_opts = super::smells::options_from_config(&config, 5);
    let selection = resolve_selection(selection, &config, &health_opts, &smells_opts)?;

    let scope = match &staged_ctx {
        Some(ctx) => AuditScope::Line(
            DiffIndex::from_unified_diff(&ctx.cached_diff, ctx.tmp.path())
                .context("parsing staged diff")?,
        ),
        None => build_scope(&base, diff_file.as_deref(), &project_root)?,
    };
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

    let package_roots = resolve_package_roots(&config).context("resolving package roots")?;
    let files = discover_python_files(&project_root, &package_roots, &config);

    // The reachability pass owns parsing when it runs; otherwise parse
    // directly so unselected families cost nothing beyond the parse.
    let (mut all_issues, parsed) = if selection.runs(Family::Check) {
        let (mut analysis, parsed) =
            analyze_with_parsed(&config).context("check analysis failed")?;
        (std::mem::take(&mut analysis.issues), parsed)
    } else {
        let (parsed, parse_errors) = pyllow_analyzer::parse_files_into_map(&files);
        (parse_errors, parsed)
    };

    if selection.runs(Family::Dupes) {
        all_issues.extend(run_dupes(&files, DupesOptions::default()));
    }
    if selection.runs(Family::Health) {
        all_issues.extend(run_health(&parsed, &project_root, health_opts));
    }
    let mut exemptions = Vec::new();
    if selection.runs(Family::Smells) {
        let mut smells_out = run_smells(&parsed, &smells_opts);
        all_issues.append(&mut smells_out.issues);
        super::smells::note_exemptions(&smells_out.exemptions);
        exemptions = smells_out.exemptions;
    }
    all_issues.retain(|i| selection.keeps(i));

    let total_before = all_issues.len();
    all_issues.retain(|i| scope.contains(i));

    let mut results_for_baseline = AnalysisResults {
        stats: AnalysisStats::default(),
        issues: std::mem::take(&mut all_issues),
        executed_rules: Vec::new(),
        selection: None,
    };
    let applied = apply(&mut results_for_baseline, &project_root, &post)?;
    note_baseline_filter(applied.suppressed, &post.baseline);
    all_issues = results_for_baseline.issues;

    // Report real paths, not snapshot paths. Suppressions and baselines above
    // ran against the snapshot (matching the staged config); only the final
    // report is rewritten.
    if let Some(ctx) = &staged_ctx {
        let snap_top = ctx
            .tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|_| ctx.tmp.path().to_path_buf());
        for issue in &mut all_issues {
            for p in issue.paths_mut() {
                if let Ok(rel) = p.strip_prefix(&snap_top) {
                    *p = ctx.real_toplevel.join(rel);
                }
            }
        }
    }
    let report_root = match &staged_ctx {
        Some(ctx) => {
            let snap_top = ctx
                .tmp
                .path()
                .canonicalize()
                .unwrap_or_else(|_| ctx.tmp.path().to_path_buf());
            match project_root
                .canonicalize()
                .unwrap_or_else(|_| project_root.clone())
                .strip_prefix(&snap_top)
            {
                Ok(rel) => ctx.real_toplevel.join(rel),
                Err(_) => project_root.clone(),
            }
        }
        None => project_root.clone(),
    };
    let in_scope = all_issues.len();

    let verdict = if applied.count_gate_failed {
        // The strict count gate failed; findings in scope are irrelevant.
        Verdict::Fail
    } else if in_scope == 0 {
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
            exemptions,
        },
        issues: all_issues,
        executed_rules: selection.executed_rules.clone(),
        selection: Some(pyllow_types::Selection {
            families_requested: selection.requested_families.clone(),
            families_executed: selection
                .families
                .iter()
                .map(|f| f.as_str().to_string())
                .collect(),
            rules_requested: selection.requested_rules.clone(),
        }),
    };
    format.print(&results, &report_root)?;
    render_score(&results, &post, format);
    render_ownership(&results, &report_root, &post, format);
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
        let scope = line_scope(&diff_touching_lines_11("foo.py"), dir.path());
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
        let scope = line_scope(&diff_touching_lines_11("foo.py"), dir.path());
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
        let scope = line_scope(&diff_touching_lines_11("b.py"), dir.path());
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
        let scope = line_scope(&diff_touching_lines_11("foo.py"), dir.path());
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
        let scope = line_scope(&diff_touching_lines_11("foo.py"), dir.path());
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
        let scope = line_scope(&diff_touching_lines_11("foo.py"), dir.path());
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
