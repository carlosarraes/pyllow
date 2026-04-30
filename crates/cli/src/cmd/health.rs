use crate::postprocess::{
    apply, handle_snapshot, note_baseline_filter, render_ownership, render_score, PostFlags,
};
use crate::report::Format;
use anyhow::Result;
use pyllow_analyzer::health::{analyze, HealthOptions};
use pyllow_analyzer::{discover_python_files, resolve_package_roots};
use pyllow_extract::{parse_file, ParsedModule};
use pyllow_types::{AnalysisResults, AnalysisStats, FileId};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::path::PathBuf;
use std::time::Instant;

pub fn run(
    path: PathBuf,
    cyclomatic: u32,
    cognitive: u32,
    maintainability: u32,
    hotspot_top: usize,
    top: Option<usize>,
    format: Format,
    post: PostFlags,
) -> Result<bool> {
    let (config, project_root) = super::load_config(&path)?;
    let started = Instant::now();
    let package_roots = resolve_package_roots(&config);
    let files = discover_python_files(&project_root, &package_roots, &config);

    let parsed_modules: Vec<ParsedModule> = files
        .par_iter()
        .filter_map(|p| parse_file(p).ok())
        .collect();
    let parsed: FxHashMap<FileId, ParsedModule> = parsed_modules
        .into_iter()
        .enumerate()
        .map(|(i, m)| (FileId(i as u32), m))
        .collect();

    let issues = analyze(
        &parsed,
        &project_root,
        HealthOptions {
            cyclomatic_threshold: cyclomatic,
            cognitive_threshold: cognitive,
            maintainability_threshold: maintainability,
            hotspot_top_n: hotspot_top,
            top,
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
    let suppressed = apply(&mut results, &project_root, &post)?;
    note_baseline_filter(suppressed, &post.baseline);
    let has_issues = !results.issues.is_empty();
    format.print(&results);
    render_score(&results, &post);
    render_ownership(&results, &project_root, &post);
    handle_snapshot(&results, &post)?;
    Ok(has_issues)
}
