use pyllow_types::AnalysisResults;

mod human;
mod json;

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Format {
    Human,
    Json,
}

pub fn print(results: &AnalysisResults, format: Format) {
    match format {
        Format::Human => human::print(results),
        Format::Json => json::print(results),
    }
}
