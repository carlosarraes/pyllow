use crate::postprocess::{apply, note_baseline_filter, PostFlags};
use crate::report::Format;
use anyhow::{Context, Result};
use colored::Colorize;
use pyllow_analyzer::dupes::{run_with_files as run_dupes, DupesOptions};
use pyllow_analyzer::health::{analyze as run_health, HealthOptions};
use pyllow_analyzer::{analyze, discover_python_files, resolve_package_roots};
use pyllow_extract::{parse_file, ParsedModule};
use pyllow_types::{AnalysisResults, AnalysisStats, FileId, Issue};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

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
    max_issues: usize,
    format: Format,
    post: PostFlags,
) -> Result<bool> {
    let (config, project_root) = super::load_config(&path)?;
    let started = Instant::now();
    let changed = changed_files_since(&project_root, &base)?;
    if changed.is_empty() {
        eprintln!("warning: no files changed since {} (audit will be empty)", base);
    }

    let mut all_issues: Vec<Issue> = analyze(&config).context("check analysis failed")?.issues;

    let package_roots = resolve_package_roots(&config);
    let files = discover_python_files(&project_root, &package_roots, &config);

    all_issues.extend(run_dupes(&files, DupesOptions::default()));

    let parsed_modules: Vec<ParsedModule> = files
        .par_iter()
        .filter_map(|p| parse_file(p).ok())
        .collect();
    let parsed: FxHashMap<FileId, ParsedModule> = parsed_modules
        .into_iter()
        .enumerate()
        .map(|(i, m)| (FileId(i as u32), m))
        .collect();
    all_issues.extend(run_health(
        &parsed,
        &project_root,
        HealthOptions::default(),
    ));

    let total_before = all_issues.len();
    all_issues.retain(|i| issue_in_changed_scope(i, &changed));

    let mut results_for_baseline = AnalysisResults {
        stats: AnalysisStats::default(),
        issues: std::mem::take(&mut all_issues),
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

    eprintln!(
        "auditing {} changed file{} since {} ({} of {} issues in scope)",
        changed.len(),
        if changed.len() == 1 { "" } else { "s" },
        base,
        in_scope,
        total_before
    );

    let results = AnalysisResults {
        stats: AnalysisStats {
            files_scanned: files.len(),
            entry_points: 0,
            plugins_run: Vec::new(),
            elapsed_ms: started.elapsed().as_millis() as u64,
        },
        issues: all_issues,
    };
    format.print(&results);
    println!(
        "{} {} {} ({} ms)",
        "verdict:".dimmed(),
        verdict.label(),
        format!("{} issue{} in PR scope", in_scope, if in_scope == 1 { "" } else { "s" }).dimmed(),
        results.stats.elapsed_ms
    );

    Ok(verdict.is_fail())
}

fn issue_in_changed_scope(issue: &Issue, changed: &FxHashSet<PathBuf>) -> bool {
    match issue {
        Issue::Duplicate { occurrences, .. } => occurrences
            .iter()
            .any(|o| canonical_in_set(&o.path, changed)),
        _ => canonical_in_set(issue.path(), changed),
    }
}

fn canonical_in_set(path: &Path, set: &FxHashSet<PathBuf>) -> bool {
    path.canonicalize()
        .map(|c| set.contains(&c))
        .unwrap_or(false)
}

fn changed_files_since(project_root: &Path, base: &str) -> Result<FxHashSet<PathBuf>> {
    let output = Command::new("git")
        .args(["diff", "--name-only", &format!("{base}...HEAD")])
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
