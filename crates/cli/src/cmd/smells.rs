use crate::postprocess::{
    apply, handle_snapshot, note_baseline_filter, render_ownership, render_score, PostFlags,
};
use crate::report::Format;
use anyhow::{Context, Result};
use pyllow_analyzer::smells::{run_with_files, SmellsOptions};

use pyllow_analyzer::{discover_python_files, resolve_package_roots};
use pyllow_types::{active_smell_rules, AnalysisResults, AnalysisStats, SmellRule};
use std::path::PathBuf;
use std::time::Instant;

pub fn run(path: PathBuf, todo_threshold: u32, format: Format, post: PostFlags) -> Result<bool> {
    let (config, project_root) = super::load_config(&path)?;
    let started = Instant::now();
    let package_roots = resolve_package_roots(&config).context("resolving package roots")?;
    let files = discover_python_files(&project_root, &package_roots, &config);

    let opts = options_from_config(&config, todo_threshold);
    let output = run_with_files(&files, &opts);
    note_exemptions(&output.exemptions);

    let mut results = AnalysisResults {
        stats: AnalysisStats {
            files_scanned: files.len(),
            entry_points: 0,
            plugins_run: Vec::new(),
            elapsed_ms: started.elapsed().as_millis() as u64,
            exemptions: output.exemptions,
        },
        issues: output.issues,
        selection: None,
            executed_rules: executed_smell_rules(&opts),
    };
    let applied = apply(&mut results, &project_root, &post)?;
    note_baseline_filter(applied.suppressed, &post.baseline);
    let has_issues = !results.issues.is_empty() || applied.count_gate_failed;
    format.print(&results, &project_root)?;
    render_score(&results, &post, format);
    render_ownership(&results, &project_root, &post, format);
    handle_snapshot(&results, &post, format)?;
    Ok(has_issues)
}

/// One dimmed stderr line per framework exemption, so a suppressed finding
/// is always visible to a human reading the log.
pub fn note_exemptions(exemptions: &[String]) {
    use colored::Colorize;
    for note in exemptions {
        eprintln!("{} {note}", "exempt:".dimmed());
    }
}

/// Smell rules that actually ran: every rule not disabled by config. A rule
/// that ran and found nothing still belongs here.
pub fn executed_smell_rules(opts: &SmellsOptions) -> Vec<String> {
    // Iterate the canonical rule order rather than the hash set, so the
    // reported list is deterministic across runs.
    SmellRule::all()
        .iter()
        .filter(|r| opts.enabled.contains(r))
        .map(|r| r.as_str().to_string())
        .collect()
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
        enabled: active_smell_rules(&config.smells_enabled, &config.smells_disabled),
        todo_density_threshold: config
            .smells_todo_density_threshold
            .unwrap_or(todo_threshold_default),
        money_extra_words: config.smells_money_extra_patterns.clone(),
        banned_apis: config.smells_banned_apis.clone(),
        fastapi_policy: config
            .plugins
            .get("fastapi")
            .map(|p| p.enabled)
            .unwrap_or(true),
    }
}


