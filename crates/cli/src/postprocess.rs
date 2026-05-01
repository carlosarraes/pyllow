use crate::report::Format;
use anyhow::{Context, Result};
use clap::Args;
use colored::Colorize;
use pyllow_analyzer::{baseline, ownership, score, snapshot, suppressions};
use pyllow_types::{AnalysisResults, Issue};
use std::path::{Path, PathBuf};

#[derive(Args, Clone, Debug, Default)]
pub struct PostFlags {
    /// Filter out issues whose fingerprint appears in this baseline file
    #[arg(long)]
    pub baseline: Option<PathBuf>,
    /// Save current issues as a baseline file (overwrites if it exists)
    #[arg(long)]
    pub save_baseline: Option<PathBuf>,
    /// Print a 0-100 health score with letter grade after the issues table
    #[arg(long)]
    pub score: bool,
    /// Save the current run's score and metric counts to a snapshot file
    #[arg(long)]
    pub save_snapshot: Option<PathBuf>,
    /// Compare current run against a saved snapshot; print per-metric deltas
    #[arg(long)]
    pub trend: Option<PathBuf>,
    /// Group issues by CODEOWNERS team (or top-level directory if no CODEOWNERS file)
    #[arg(long)]
    pub ownership: bool,
}

pub fn apply(
    results: &mut AnalysisResults,
    project_root: &Path,
    flags: &PostFlags,
) -> Result<usize> {
    let dropped_by_noqa = suppressions::filter(&mut results.issues, project_root);
    if dropped_by_noqa > 0 {
        eprintln!(
            "{} {} issue{} suppressed by noqa directives",
            "noqa:".dimmed(),
            dropped_by_noqa,
            if dropped_by_noqa == 1 { "" } else { "s" }
        );
    }
    let mut suppressed = 0usize;
    if let Some(path) = &flags.baseline {
        let set =
            baseline::load(path).with_context(|| format!("loading baseline {}", path.display()))?;
        suppressed = baseline::filter(&mut results.issues, &set, project_root);
    }
    if let Some(path) = &flags.save_baseline {
        baseline::save(path, &results.issues, project_root)
            .with_context(|| format!("saving baseline {}", path.display()))?;
        eprintln!(
            "{} {} ({} issue{} captured)",
            "saved baseline:".green().bold(),
            path.display(),
            results.issues.len(),
            if results.issues.len() == 1 { "" } else { "s" }
        );
    }
    Ok(suppressed)
}

pub fn note_baseline_filter(suppressed: usize, baseline: &Option<PathBuf>) {
    if suppressed > 0 {
        if let Some(path) = baseline {
            eprintln!(
                "{} {} issue{} suppressed by baseline {}",
                "baseline:".dimmed(),
                suppressed,
                if suppressed == 1 { "" } else { "s" },
                path.display()
            );
        }
    }
}

pub fn render_score(results: &AnalysisResults, flags: &PostFlags, format: Format) {
    if !flags.score {
        return;
    }
    let s = score::compute(&results.issues);
    let colored = match s.grade {
        'A' => format!("{}", s.value).green().bold(),
        'B' => format!("{}", s.value).bright_green().bold(),
        'C' => format!("{}", s.value).yellow().bold(),
        'D' => format!("{}", s.value).bright_red().bold(),
        _ => format!("{}", s.value).red().bold(),
    };
    let line = format!(
        "{} {}/100 grade {} ({})",
        "score:".dimmed(),
        colored,
        format!("{}", s.grade).bold(),
        s.label()
    );
    if format.is_machine_readable() {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
}

pub fn handle_snapshot(results: &AnalysisResults, flags: &PostFlags, format: Format) -> Result<()> {
    if let Some(prev_path) = &flags.trend {
        let previous = snapshot::load(prev_path)
            .with_context(|| format!("loading snapshot {}", prev_path.display()))?;
        let current = snapshot::Snapshot::from_issues(&results.issues);
        let diff = snapshot::compare(&previous, &current);
        render_trend(&previous, &current, &diff, format);
    }
    if let Some(path) = &flags.save_snapshot {
        let snap = snapshot::Snapshot::from_issues(&results.issues);
        snapshot::save(path, &snap)
            .with_context(|| format!("saving snapshot {}", path.display()))?;
        eprintln!(
            "{} {} (score {}/100 grade {})",
            "saved snapshot:".green().bold(),
            path.display(),
            snap.score.value,
            snap.score.grade
        );
    }
    Ok(())
}

pub fn render_ownership(
    results: &AnalysisResults,
    project_root: &Path,
    flags: &PostFlags,
    format: Format,
) {
    if !flags.ownership {
        return;
    }
    let codeowners = ownership::Codeowners::load(project_root);
    let buckets = match &codeowners {
        Some(co) => ownership::group_by_owner(&results.issues, project_root, co),
        None => {
            eprintln!(
                "{} no CODEOWNERS found; grouping by top-level directory",
                "ownership:".dimmed()
            );
            ownership::group_by_top_level_dir(&results.issues, project_root)
        }
    };
    let mut entries: Vec<(String, Vec<&Issue>)> = buckets.into_iter().collect();
    entries.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    let mut lines = vec![String::new(), format!("{}", "## by ownership".bold())];
    for (label, issues) in entries {
        lines.push(format!(
            "  {} {} {}",
            format!("{:>4}", issues.len()).bold(),
            label.cyan(),
            "issues".dimmed()
        ));
    }
    let body = lines.join("\n");
    if format.is_machine_readable() {
        eprintln!("{body}");
    } else {
        println!("{body}");
    }
}

fn render_trend(
    previous: &snapshot::Snapshot,
    current: &snapshot::Snapshot,
    diff: &snapshot::Diff,
    format: Format,
) {
    use std::cmp::Ordering;

    let arrow = |delta: i32| -> colored::ColoredString {
        match delta.cmp(&0) {
            Ordering::Less => format!("{delta:+}").green().bold(),
            Ordering::Greater => format!("{delta:+}").red().bold(),
            Ordering::Equal => "  0".dimmed().bold(),
        }
    };
    let mut lines = vec![format!(
        "{} score {}/100 \u{2192} {}/100 ({})",
        "trend:".dimmed(),
        previous.score.value,
        current.score.value,
        arrow(diff.score_delta)
    )];
    let rows = [
        ("total issues", diff.total_issues_delta),
        ("unused-file", diff.unused_files_delta),
        ("unused-import", diff.unused_imports_delta),
        ("unused-dep", diff.unused_deps_delta),
        ("duplicate", diff.duplicates_delta),
        ("complexity", diff.complexity_delta),
        ("low-maintainability", diff.low_maintainability_delta),
        ("hotspot", diff.hotspots_delta),
        ("smell", diff.smells_delta),
        ("circular-dependency", diff.circular_deps_delta),
        ("refactor-target", diff.refactor_targets_delta),
        ("feature-flag", diff.feature_flags_delta),
    ];
    for (label, delta) in rows {
        if delta == 0 {
            continue;
        }
        lines.push(format!("        {} {}", arrow(delta), label.dimmed()));
    }
    let body = lines.join("\n");
    if format.is_machine_readable() {
        eprintln!("{body}");
    } else {
        println!("{body}");
    }
}
