use crate::postprocess::{apply, note_baseline_filter, render_score, PostFlags};
use crate::report::Format;
use anyhow::Result;
use pyllow_analyzer::dupes::{run_with_files, DupesOptions};
use pyllow_analyzer::{discover_python_files, resolve_package_roots};
use pyllow_types::{AnalysisResults, AnalysisStats};
use std::path::PathBuf;
use std::time::Instant;

pub fn run(
    path: PathBuf,
    window: usize,
    min_unique: usize,
    format: Format,
    post: PostFlags,
) -> Result<bool> {
    let (config, project_root) = super::load_config(&path)?;
    let started = Instant::now();
    let package_roots = resolve_package_roots(&config);
    let files = discover_python_files(&project_root, &package_roots, &config);
    let issues = run_with_files(&files, DupesOptions { window, min_unique });
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
    Ok(has_issues)
}
