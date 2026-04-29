use anyhow::{Context, Result};
use clap::Args;
use colored::Colorize;
use pyllow_analyzer::{baseline, score};
use pyllow_types::AnalysisResults;
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
}

pub fn apply(
    results: &mut AnalysisResults,
    project_root: &Path,
    flags: &PostFlags,
) -> Result<usize> {
    let mut suppressed = 0usize;
    if let Some(path) = &flags.baseline {
        let set = baseline::load(path)
            .with_context(|| format!("loading baseline {}", path.display()))?;
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

pub fn render_score(results: &AnalysisResults, flags: &PostFlags) {
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
    println!(
        "{} {}/100 grade {} ({})",
        "score:".dimmed(),
        colored,
        format!("{}", s.grade).bold(),
        s.label()
    );
}
