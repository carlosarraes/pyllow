use colored::Colorize;
use pyllow_types::{AnalysisResults, Issue};
use tabled::{builder::Builder, settings::Style};

pub fn print(results: &AnalysisResults) {
    if results.issues.is_empty() {
        println!(
            "{} no issues found ({} files scanned, {} entry points, {} ms)",
            "ok".green().bold(),
            results.stats.files_scanned,
            results.stats.entry_points,
            results.stats.elapsed_ms,
        );
        return;
    }

    let mut builder = Builder::new();
    builder.push_record(["kind", "location", "detail"]);
    for issue in &results.issues {
        match issue {
            Issue::UnusedFile { path } => {
                builder.push_record([
                    "unused-file",
                    &path.display().to_string(),
                    "",
                ]);
            }
            Issue::UnusedImport {
                path,
                line,
                name,
                module,
            } => {
                builder.push_record([
                    "unused-import",
                    &format!("{}:{}", path.display(), line),
                    &format!("{} (from {})", name, module),
                ]);
            }
            Issue::UnusedDep { path, name, source } => {
                builder.push_record([
                    "unused-dep",
                    &path.display().to_string(),
                    &format!("{} (in {})", name, source),
                ]);
            }
        }
    }
    let mut table = builder.build();
    table.with(Style::rounded());
    println!("{table}");
    println!(
        "{} {} issue{} \u{2014} {} files scanned, {} entry points, {} ms",
        "found".yellow().bold(),
        results.issues.len(),
        if results.issues.len() == 1 { "" } else { "s" },
        results.stats.files_scanned,
        results.stats.entry_points,
        results.stats.elapsed_ms,
    );
}
