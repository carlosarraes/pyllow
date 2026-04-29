use crate::report::Format;
use anyhow::Result;
use pyllow_analyzer::health::{analyze, HealthOptions};
use pyllow_analyzer::{discover_python_files_pub, resolve_package_roots};
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
    format: Format,
) -> Result<bool> {
    let (config, project_root) = super::load_config(&path)?;
    let started = Instant::now();
    let package_roots = resolve_package_roots(&config);
    let files = discover_python_files_pub(&project_root, &package_roots, &config);

    let parsed_per_path: Vec<ParsedModule> = files
        .par_iter()
        .filter_map(|p| parse_file(p).ok())
        .collect();

    let mut parsed: FxHashMap<FileId, ParsedModule> = FxHashMap::default();
    for (i, m) in parsed_per_path.into_iter().enumerate() {
        parsed.insert(FileId(i as u32), m);
    }

    let issues = analyze(
        &parsed,
        &project_root,
        HealthOptions {
            cyclomatic_threshold: cyclomatic,
            cognitive_threshold: cognitive,
            maintainability_threshold: maintainability,
            hotspot_top_n: hotspot_top,
        },
    );

    let has_issues = !issues.is_empty();
    let results = AnalysisResults {
        stats: AnalysisStats {
            files_scanned: parsed.len(),
            entry_points: 0,
            plugins_run: vec!["health".to_string()],
            elapsed_ms: started.elapsed().as_millis() as u64,
        },
        issues,
    };
    format.print(&results);
    Ok(has_issues)
}
