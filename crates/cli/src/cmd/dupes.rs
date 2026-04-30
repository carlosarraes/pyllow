use crate::postprocess::{
    apply, handle_snapshot, note_baseline_filter, render_ownership, render_score, PostFlags,
};
use crate::report::Format;
use anyhow::{anyhow, Context, Result};
use pyllow_analyzer::dupes::{run_with_files, DupesOptions, Mode};
use pyllow_analyzer::{discover_python_files, resolve_package_roots};
use pyllow_types::{AnalysisResults, AnalysisStats, Issue};
use std::path::{Path, PathBuf};
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
    trace: Option<String>,
    skip_local: bool,
    format: Format,
    post: PostFlags,
) -> Result<bool> {
    let (config, project_root) = super::load_config(&path)?;
    let started = Instant::now();
    let package_roots = resolve_package_roots(&config);
    let files = discover_python_files(&project_root, &package_roots, &config);
    let mut issues = run_with_files(
        &files,
        DupesOptions {
            window,
            min_unique,
            mode: mode.into(),
            ..DupesOptions::default()
        },
    );
    if let Some(arg) = trace.as_deref() {
        let (trace_path, trace_line) = parse_trace(arg, &project_root)?;
        issues.retain(|issue| issue_matches_trace(issue, &trace_path, trace_line));
    }
    if skip_local {
        issues.retain(|issue| !is_intra_directory(issue));
    }
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

fn parse_trace(arg: &str, project_root: &Path) -> Result<(PathBuf, u32)> {
    let (file, line) = arg
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("--trace expects <file>:<line>, got: {arg}"))?;
    let line: u32 = line
        .parse()
        .with_context(|| format!("--trace line must be an integer, got: {line}"))?;
    let raw = project_root.join(file);
    let canonical = raw
        .canonicalize()
        .with_context(|| format!("--trace path not found: {}", raw.display()))?;
    Ok((canonical, line))
}

fn issue_matches_trace(issue: &Issue, trace_path: &Path, trace_line: u32) -> bool {
    let Issue::Duplicate { occurrences, .. } = issue else {
        return false;
    };
    occurrences.iter().any(|occ| {
        let canonical = occ.path.canonicalize().unwrap_or_else(|_| occ.path.clone());
        canonical == trace_path && occ.start_line <= trace_line && trace_line <= occ.end_line
    })
}

fn is_intra_directory(issue: &Issue) -> bool {
    let Issue::Duplicate { occurrences, .. } = issue else {
        return false;
    };
    let mut parents = occurrences.iter().filter_map(|o| o.path.parent());
    let Some(first) = parents.next() else {
        return false;
    };
    parents.all(|p| p == first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_types::DuplicateOccurrence;

    fn dup(occs: &[(&str, u32, u32)]) -> Issue {
        Issue::Duplicate {
            token_count: 30,
            occurrences: occs
                .iter()
                .map(|(p, s, e)| DuplicateOccurrence {
                    path: PathBuf::from(p),
                    start_line: *s,
                    end_line: *e,
                })
                .collect(),
        }
    }

    #[test]
    fn intra_directory_clone_skipped() {
        let same_dir = dup(&[("src/a.py", 10, 30), ("src/b.py", 5, 25)]);
        let cross_dir = dup(&[("src/a.py", 10, 30), ("tests/a.py", 5, 25)]);
        assert!(is_intra_directory(&same_dir));
        assert!(!is_intra_directory(&cross_dir));
    }
}
