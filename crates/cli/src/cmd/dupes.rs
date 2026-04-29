use crate::report::Format;
use anyhow::Result;
use pyllow_analyzer::dupes::{run as run_dupes, DupesOptions};
use pyllow_types::{AnalysisResults, AnalysisStats};
use std::path::PathBuf;
use std::time::Instant;

pub fn run(path: PathBuf, window: usize, min_unique: usize, format: Format) -> Result<bool> {
    let (config, project_root) = super::load_config(&path)?;
    let _ = config;
    let started = Instant::now();
    let issues = run_dupes(
        &project_root,
        DupesOptions { window, min_unique },
    );
    let has_issues = !issues.is_empty();
    let results = AnalysisResults {
        stats: AnalysisStats {
            files_scanned: 0,
            entry_points: 0,
            plugins_run: vec!["dupes".to_string()],
            elapsed_ms: started.elapsed().as_millis() as u64,
        },
        issues,
    };
    format.print(&results);
    Ok(has_issues)
}
