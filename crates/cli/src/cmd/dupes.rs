use crate::postprocess::{
    apply, handle_snapshot, note_baseline_filter, render_ownership, render_score, PostFlags,
};
use crate::report::Format;
use anyhow::Result;
use pyllow_analyzer::dupes::{run_with_files, DupesOptions, Mode};
use pyllow_analyzer::{discover_python_files, resolve_package_roots};
use pyllow_types::{AnalysisResults, AnalysisStats};
use std::path::PathBuf;
use std::time::Instant;

#[derive(clap::ValueEnum, Clone, Copy, Debug, Default)]
#[clap(rename_all = "lowercase")]
pub enum DupesMode {
    Strict,
    #[default]
    Mild,
    Weak,
    Semantic,
}

impl From<DupesMode> for Mode {
    fn from(m: DupesMode) -> Self {
        match m {
            DupesMode::Strict => Self::Strict,
            DupesMode::Mild => Self::Mild,
            DupesMode::Weak => Self::Weak,
            DupesMode::Semantic => Self::Semantic,
        }
    }
}

pub fn run(
    path: PathBuf,
    window: usize,
    min_unique: usize,
    mode: DupesMode,
    format: Format,
    post: PostFlags,
) -> Result<bool> {
    let (config, project_root) = super::load_config(&path)?;
    let started = Instant::now();
    let package_roots = resolve_package_roots(&config);
    let files = discover_python_files(&project_root, &package_roots, &config);
    let issues = run_with_files(
        &files,
        DupesOptions {
            window,
            min_unique,
            mode: mode.into(),
        },
    );
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
    render_score(&results, &post);
    render_ownership(&results, &project_root, &post);
    handle_snapshot(&results, &post)?;
    Ok(has_issues)
}
