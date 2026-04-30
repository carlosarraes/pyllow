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
        .map(|p: &std::path::PathBuf| {
            Path::new(p)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(" \u{2192} ")
}
