use crate::postprocess::{
    apply, handle_snapshot, note_baseline_filter, render_ownership, render_score, PostFlags,
};
use crate::report::Format;
use anyhow::{Context, Result};
use colored::Colorize;
use pyllow_analyzer::smells::{run_with_files, SmellsOptions};
use pyllow_analyzer::{discover_python_files, resolve_package_roots};
use pyllow_types::{AnalysisResults, AnalysisStats, SmellRule};
use rustc_hash::FxHashSet;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Instant;

pub fn run(path: PathBuf, todo_threshold: u32, format: Format, post: PostFlags) -> Result<bool> {
    let (config, project_root) = super::load_config(&path)?;
    let started = Instant::now();
    let package_roots = resolve_package_roots(&config).context("resolving package roots")?;
    let files = discover_python_files(&project_root, &package_roots, &config);

    let opts = options_from_config(&config, todo_threshold);
    let issues = run_with_files(&files, &opts);

    let mut results = AnalysisResults {
        stats: AnalysisStats {
            files_scanned: files.len(),
            entry_points: 0,
            plugins_run: Vec::new(),
            elapsed_ms: started.elapsed().as_millis() as u64,
        },
        issues,
    };
    let suppressed = apply(&mut results, &project_root, &post)?;
    note_baseline_filter(suppressed, &post.baseline);
    let has_issues = !results.issues.is_empty();
    format.print(&results);
    render_score(&results, &post, format);
    render_ownership(&results, &project_root, &post, format);
    handle_snapshot(&results, &post, format)?;
    Ok(has_issues)
}

/// Build `SmellsOptions` from the project's `[smells]` config. Used by
/// `pyllow smells` (with a CLI default for `--todo-threshold`) and by
/// `pyllow audit`, which previously ignored the config entirely and made
/// the PR gate diverge from the standalone command.
pub fn options_from_config(
    config: &pyllow_config::ResolvedConfig,
    todo_threshold_default: u32,
) -> SmellsOptions {
    SmellsOptions {
        disabled: smells_disabled_from_config(config),
        todo_density_threshold: config
            .smells_todo_density_threshold
            .unwrap_or(todo_threshold_default),
        money_extra_words: config.smells_money_extra_patterns.clone(),
    }
}

fn smells_disabled_from_config(config: &pyllow_config::ResolvedConfig) -> FxHashSet<SmellRule> {
    let mut set = FxHashSet::default();
    for raw in &config.smells_disabled {
        if let Ok(rule) = SmellRule::from_str(raw) {
            set.insert(rule);
        } else {
            eprintln!("{} unknown smell rule in config: {raw}", "warning:".bold());
        }
    }
    set
}
