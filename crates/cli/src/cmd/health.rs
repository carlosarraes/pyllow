use crate::postprocess::{
    apply, handle_snapshot, note_baseline_filter, render_ownership, render_score, PostFlags,
};
use crate::report::Format;
use anyhow::Result;
use pyllow_analyzer::health::{analyze, HealthOptions};
use pyllow_analyzer::{discover_python_files, parse_files_into_map, resolve_package_roots};
use pyllow_types::{AnalysisResults, AnalysisStats, Effort};
use std::path::PathBuf;
use std::time::Instant;

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
#[clap(rename_all = "lowercase")]
pub enum EffortArg {
    Low,
    Medium,
    High,
}

impl From<EffortArg> for Effort {
    fn from(e: EffortArg) -> Self {
        match e {
            EffortArg::Low => Self::Low,
            EffortArg::Medium => Self::Medium,
            EffortArg::High => Self::High,
        }
    }
}

/// CLI-level configuration for `pyllow health`. Mirrors the parsed clap
/// arguments and converts to `HealthOptions` for the analyzer.
pub struct HealthArgs {
    pub path: PathBuf,
    pub cyclomatic: u32,
    pub cognitive: u32,
    pub maintainability: u32,
    pub hotspot_top: usize,
    pub top: Option<usize>,
    pub targets: bool,
    pub target_effort: Option<EffortArg>,
    pub format: Format,
    pub post: PostFlags,
}

pub fn run(args: HealthArgs) -> Result<bool> {
    let (config, project_root) = super::load_config(&args.path)?;
    let started = Instant::now();
    let package_roots = resolve_package_roots(&config);
    let files = discover_python_files(&project_root, &package_roots, &config);
    let parsed = parse_files_into_map(&files);

    let issues = analyze(
        &parsed,
        &project_root,
        HealthOptions {
            cyclomatic_threshold: args.cyclomatic,
            cognitive_threshold: args.cognitive,
            maintainability_threshold: args.maintainability,
            hotspot_top_n: args.hotspot_top,
            top: args.top,
            targets: args.targets,
            target_effort: args.target_effort.map(Effort::from),
            ..Default::default()
        },
    );

    let mut results = AnalysisResults {
        stats: AnalysisStats {
            files_scanned: parsed.len(),
            entry_points: 0,
            plugins_run: Vec::new(),
            elapsed_ms: started.elapsed().as_millis() as u64,
        },
        issues,
    };
    let suppressed = apply(&mut results, &project_root, &args.post)?;
    note_baseline_filter(suppressed, &args.post.baseline);
    let has_issues = !results.issues.is_empty();
    args.format.print(&results);
    render_score(&results, &args.post);
    render_ownership(&results, &project_root, &args.post);
    handle_snapshot(&results, &args.post)?;
    Ok(has_issues)
}
