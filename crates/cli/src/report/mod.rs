use anyhow::Result;
use pyllow_types::{AnalysisResults, Issue};
use std::path::{Path, PathBuf};

mod human;
mod json;
mod markdown;
mod sarif;

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Format {
    Human,
    Json,
    Sarif,
    Markdown,
}

impl Format {
    /// `project_root` anchors repository-relative paths in machine output.
    /// Returns `Err` when a document cannot be rendered — a run that could not
    /// emit its results must not be reported as clean.
    pub fn print(self, results: &AnalysisResults, project_root: &Path) -> Result<()> {
        match self {
            Format::Human => human::print(results),
            Format::Json => json::print(results, project_root)?,
            Format::Sarif => sarif::print(results, project_root)?,
            Format::Markdown => markdown::print(results),
        }
        Ok(())
    }

    /// True when stdout is reserved for a machine-readable document
    /// (JSON, SARIF). Auxiliary human renderers (score, ownership,
    /// trend, verdict) must skip stdout in this mode or they corrupt
    /// the document — they can still go to stderr.
    pub fn is_machine_readable(self) -> bool {
        matches!(self, Format::Json | Format::Sarif)
    }
}

/// Convert an absolute issue path into a repository-relative POSIX path.
/// Canonicalizes both sides first so symlinked roots (`/tmp` → `/private/tmp`)
/// still strip cleanly; falls back to a raw strip for paths that no longer
/// exist on disk.
///
/// Shared by the JSON and SARIF renderers so their locations agree by
/// construction rather than by two implementations happening to match.
pub(crate) fn relative_posix(path: &Path, project_root: &Path) -> String {
    let stripped = match (path.canonicalize(), project_root.canonicalize()) {
        (Ok(p), Ok(root)) => p.strip_prefix(&root).map(Path::to_path_buf).ok(),
        _ => None,
    }
    .or_else(|| path.strip_prefix(project_root).map(Path::to_path_buf).ok())
    .unwrap_or_else(|| path.to_path_buf());

    stripped
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// A copy of `issue` with every path rewritten repository-relative.
pub(crate) fn relativized(issue: &Issue, project_root: &Path) -> Issue {
    let mut copy = issue.clone();
    for path in copy.paths_mut() {
        *path = PathBuf::from(relative_posix(path, project_root));
    }
    copy
}

/// Render a circular-dependency cycle as `a.py → b.py → c.py` for any
/// reporter. Empty/non-UTF-8 file names render as empty segments rather
/// than failing the whole row.
pub(crate) fn format_cycle_path(cycle: &[std::path::PathBuf]) -> String {
    cycle
        .iter()
        .map(|p| file_name_lossy(p))
        .collect::<Vec<_>>()
        .join(" \u{2192} ")
}

/// Render a cycle compactly for terminal tables — large SCCs in libraries
/// like pydantic span 40+ files, which blows out column widths. For
/// cycles longer than `max` files, show the first 2 and last 1 with a
/// `… (N total)` middle. Full path stays available in SARIF/JSON output.
pub(crate) fn format_cycle_summary(cycle: &[std::path::PathBuf], max: usize) -> String {
    if cycle.len() <= max {
        return format_cycle_path(cycle);
    }
    let head: Vec<String> = cycle.iter().take(2).map(|p| file_name_lossy(p)).collect();
    let tail = cycle.last().map(|p| file_name_lossy(p)).unwrap_or_default();
    format!(
        "{} \u{2192} … ({} total) \u{2192} {}",
        head.join(" \u{2192} "),
        cycle.len(),
        tail
    )
}

fn file_name_lossy(p: &Path) -> String {
    p.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}
