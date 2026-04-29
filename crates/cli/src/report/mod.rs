use pyllow_types::AnalysisResults;

mod human;
mod json;

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Format {
    Human,
    Json,
}

impl Format {
    pub fn print(self, results: &AnalysisResults) {
        match self {
            Format::Human => human::print(results),
            Format::Json => json::print(results),
        }
    }
}
