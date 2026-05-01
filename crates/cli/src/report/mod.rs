use pyllow_types::AnalysisResults;
use std::path::Path;

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
    pub fn print(self, results: &AnalysisResults) {
        match self {
            Format::Human => human::print(results),
            Format::Json => json::print(results),
            Format::Sarif => sarif::print(results),
            Format::Markdown => markdown::print(results),
        }
    }
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
pub(crate) fn format_cycle_summary(
    cycle: &[std::path::PathBuf],
    max: usize,
) -> String {
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
