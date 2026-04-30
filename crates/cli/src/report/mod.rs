use pyllow_types::AnalysisResults;

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
