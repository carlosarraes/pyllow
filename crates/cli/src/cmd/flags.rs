use crate::postprocess::{
    apply, handle_snapshot, note_baseline_filter, render_ownership, render_score, PostFlags,
};
use crate::report::Format;
use anyhow::Result;
use pyllow_analyzer::flags::analyze;
use pyllow_analyzer::{discover_python_files, parse_files_into_map, resolve_package_roots};
use pyllow_types::{AnalysisResults, AnalysisStats};
use std::path::PathBuf;
use std::time::Instant;

pub fn run(path: PathBuf, format: Format, post: PostFlags) -> Result<bool> {
    let (config, project_root) = super::load_config(&path)?;
    let started = Instant::now();
    let package_roots = resolve_package_roots(&config);
    let files = discover_python_files(&project_root, &package_roots, &config);
    let (parsed, mut issues) = parse_files_into_map(&files);

    issues.extend(analyze(&parsed));

    let mut results = AnalysisResults {
        stats: AnalysisStats {
            files_scanned: parsed.len(),
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
