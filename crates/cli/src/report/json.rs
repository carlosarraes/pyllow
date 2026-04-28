use pyllow_types::AnalysisResults;

pub fn print(results: &AnalysisResults) {
    match serde_json::to_string_pretty(results) {
        Ok(s) => println!("{}", s),
        Err(e) => eprintln!("error serializing results: {}", e),
    }
}
