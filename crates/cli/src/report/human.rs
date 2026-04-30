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
            Issue::Duplicate {
                token_count,
                occurrences,
            } => {
                let primary = occurrences
                    .first()
                    .map(|o| {
                        format!(
                            "{}:{}-{}",
                            o.path.display(),
                            o.start_line,
                            o.end_line
                        )
                    })
                    .unwrap_or_default();
                let detail = format!(
                    "{} tokens across {} locations",
                    token_count,
                    occurrences.len()
                );
                builder.push_record(["duplicate", &primary, &detail]);
            }
            Issue::Complexity {
                path,
                line,
                function,
                cyclomatic,
                cognitive,
            } => {
                builder.push_record([
                    "complexity",
                    &format!("{}:{}", path.display(), line),
                    &format!(
                        "{} (cyclomatic={}, cognitive={})",
                        function, cyclomatic, cognitive
                    ),
                ]);
            }
            Issue::LowMaintainability {
                path,
                score,
                avg_cyclomatic,
                loc,
            } => {
                builder.push_record([
                    "low-maintainability",
                    &path.display().to_string(),
                    &format!(
                        "MI={} (avg_cc={:.1}, loc={})",
                        score, avg_cyclomatic, loc
                    ),
                ]);
            }
            Issue::Hotspot {
                path,
                cyclomatic,
                churn,
                score,
            } => {
                builder.push_record([
                    "hotspot",
                    &path.display().to_string(),
                    &format!(
                        "cc={} \u{00d7} churn={} \u{2192} {:.1}",
                        cyclomatic, churn, score
                    ),
                ]);
            }
            Issue::Smell {
                path,
                line,
                rule,
                detail,
            } => {
                builder.push_record([
                    rule.as_str(),
                    &format!("{}:{}", path.display(), line),
                    detail,
                ]);
            }
            Issue::CircularDependency { cycle } => {
                let primary = cycle
                    .first()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                let detail = cycle
                    .iter()
                    .map(|p| {
                        p.file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect::<Vec<_>>()
                    .join(" \u{2192} ");
                builder.push_record(["circular-dependency", &primary, &detail]);
            }
            Issue::RefactorTarget {
                path,
                line,
                function,
                cyclomatic,
                cognitive,
                effort,
            } => {
                builder.push_record([
                    "refactor-target",
                    &format!("{}:{}", path.display(), line),
                    &format!(
                        "{} (cc={}, cog={}, effort={})",
                        function,
                        cyclomatic,
                        cognitive,
                        effort.as_str()
                    ),
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
