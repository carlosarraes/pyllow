use crate::report::Format;
use anyhow::{Context, Result};
use pyllow_analyzer::analyze;
use pyllow_types::AnalysisResults;
use rustc_hash::FxHashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn run(path: PathBuf, base: String, format: Format) -> Result<bool> {
    let (config, project_root) = super::load_config(&path)?;
    let changed = changed_files_since(&project_root, &base)?;
    if changed.is_empty() {
        eprintln!(
            "warning: no files changed since {} (audit will be empty)",
            base
        );
    }
    let mut results = analyze(&config).context("analysis failed")?;
    let before = results.issues.len();
    filter_to_changed(&mut results, &project_root, &changed);
    let has_issues = !results.issues.is_empty();
    eprintln!(
        "auditing {} changed file{} since {} ({} of {} issues in scope)",
        changed.len(),
        if changed.len() == 1 { "" } else { "s" },
        base,
        results.issues.len(),
        before
    );
    format.print(&results);
    Ok(has_issues)
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

fn filter_to_changed(
    results: &mut AnalysisResults,
    _project_root: &Path,
    changed: &FxHashSet<PathBuf>,
) {
    results.issues.retain(|issue| {
        let path = issue.path();
        match path.canonicalize() {
            Ok(canonical) => changed.contains(&canonical),
            Err(_) => false,
        }
    });
}
