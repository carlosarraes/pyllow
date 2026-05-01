//! Markdown report for PR comments.
//!
//! Designed to render cleanly in GitHub/GitLab MR comments: H2 sections per
//! rule, totals at the top, file:line tables. No ANSI colors, no Unicode
//! arrows in column headers (rule names use kebab-case).

use pyllow_types::{AnalysisResults, Issue};
use std::collections::BTreeMap;

pub fn print(results: &AnalysisResults) {
    print!("{}", render(results));
}

pub fn render(results: &AnalysisResults) -> String {
    let mut out = String::new();
    out.push_str("# pyllow report\n\n");

    if results.issues.is_empty() {
        out.push_str(&format!(
            "**No issues found.** {} files scanned in {} ms.\n",
            results.stats.files_scanned, results.stats.elapsed_ms
        ));
        return out;
    }

    let by_rule = group_by_rule(&results.issues);
    out.push_str("## Summary\n\n");
    out.push_str("| Rule | Count |\n|---|---:|\n");
    for (rule, issues) in &by_rule {
        out.push_str(&format!("| `{}` | {} |\n", rule, issues.len()));
    }
    out.push_str(&format!("| **Total** | **{}** |\n\n", results.issues.len()));

    for (rule, issues) in &by_rule {
        let description = issues
            .first()
            .map(|i| i.rule_short_description())
            .unwrap_or("");
        out.push_str(&format!("## `{rule}`\n\n"));
        if !description.is_empty() {
            out.push_str(&format!("_{description}_\n\n"));
        }
        out.push_str("| Location | Detail |\n|---|---|\n");
        for issue in issues {
            let location = format_location(issue);
            let detail = format_detail(issue);
            out.push_str(&format!("| `{location}` | {detail} |\n"));
        }
        out.push('\n');
    }

    out.push_str(&format!(
        "_{} files scanned in {} ms._\n",
        results.stats.files_scanned, results.stats.elapsed_ms
    ));
    out
}

fn group_by_rule(issues: &[Issue]) -> BTreeMap<&'static str, Vec<&Issue>> {
    let mut map: BTreeMap<&'static str, Vec<&Issue>> = BTreeMap::new();
    for issue in issues {
        map.entry(issue.rule_key()).or_default().push(issue);
    }
    map
}

fn format_location(issue: &Issue) -> String {
    let path = issue.path().display().to_string();
    match issue.line() {
        Some(line) => format!("{path}:{line}"),
        None => path,
    }
}

fn format_detail(issue: &Issue) -> String {
    match issue {
        Issue::UnusedFile { .. } => "unreachable from any entry point".to_string(),
        Issue::UnusedImport { name, module, .. } => {
            format!("`{name}` from `{module}`")
        }
        Issue::UnusedDep { name, source, .. } => format!("`{name}` (declared in {source})"),
        Issue::Duplicate {
            token_count,
            occurrences,
        } => format!("{token_count} tokens × {} locations", occurrences.len()),
        Issue::Complexity {
            function,
            cyclomatic,
            cognitive,
            ..
        } => format!("`{function}` cc={cyclomatic} cog={cognitive}"),
        Issue::LowMaintainability {
            score,
            avg_cyclomatic,
            loc,
            ..
        } => format!("MI={score} (avg cc={avg_cyclomatic:.1}, loc={loc})"),
        Issue::Hotspot {
            cyclomatic,
            churn,
            score,
            ..
        } => format!("cc={cyclomatic} × churn={churn} → {score:.1}"),
        Issue::Smell { detail, .. } => detail.clone(),
        Issue::CircularDependency { cycle } => super::format_cycle_path(cycle),
        Issue::RefactorTarget {
            function,
            cyclomatic,
            cognitive,
            effort,
            ..
        } => format!(
            "`{function}` cc={cyclomatic} cog={cognitive} effort={}",
            effort.as_str()
        ),
        Issue::FeatureFlag { flag, provider, .. } => {
            format!("`{flag}` (via {})", provider.as_str())
        }
        Issue::ParseError { message, .. } => message.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_types::{AnalysisStats, SmellRule};
    use std::path::PathBuf;

    fn results(issues: Vec<Issue>) -> AnalysisResults {
        AnalysisResults {
            issues,
            stats: AnalysisStats {
                files_scanned: 5,
                elapsed_ms: 42,
                ..Default::default()
            },
        }
    }

    #[test]
    fn empty_results_emit_no_issues_header() {
        let md = render(&results(vec![]));
        assert!(md.contains("# pyllow report"));
        assert!(md.contains("**No issues found.**"));
        assert!(md.contains("5 files scanned"));
    }

    #[test]
    fn single_issue_emits_summary_table_and_section() {
        let md = render(&results(vec![Issue::UnusedImport {
            path: PathBuf::from("a.py"),
            line: 3,
            name: "os".into(),
            module: "os".into(),
        }]));
        assert!(md.contains("## Summary"));
        assert!(md.contains("| `unused-import` | 1 |"));
        assert!(md.contains("| **Total** | **1** |"));
        assert!(md.contains("## `unused-import`"));
        assert!(md.contains("`a.py:3`"));
        assert!(md.contains("`os` from `os`"));
    }

    #[test]
    fn multiple_rules_each_get_a_section_sorted_alphabetically() {
        let md = render(&results(vec![
            Issue::UnusedFile {
                path: PathBuf::from("x.py"),
            },
            Issue::Smell {
                path: PathBuf::from("y.py"),
                line: 1,
                rule: SmellRule::MutableDefault,
                detail: "argument `foo` has mutable default".into(),
            },
        ]));
        // BTreeMap sort = mutable-default before unused-file alphabetically.
        let mutable_idx = md
            .find("## `mutable-default`")
            .expect("mutable-default section");
        let unused_idx = md.find("## `unused-file`").expect("unused-file section");
        assert!(
            mutable_idx < unused_idx,
            "sections must be sorted alphabetically"
        );
    }

    #[test]
    fn rule_section_includes_short_description() {
        let md = render(&results(vec![Issue::Smell {
            path: PathBuf::from("y.py"),
            line: 1,
            rule: SmellRule::MutableDefault,
            detail: "argument `foo` has mutable default".into(),
        }]));
        // Rule's short_description appears in italics after the section heading.
        assert!(md.contains("_Function argument has a mutable default value_"));
    }

    #[test]
    fn issue_without_line_shows_path_only() {
        let md = render(&results(vec![Issue::UnusedFile {
            path: PathBuf::from("orphan.py"),
        }]));
        assert!(md.contains("`orphan.py`"));
        // No line suffix.
        assert!(!md.contains("orphan.py:"));
    }

    #[test]
    fn report_ends_with_files_and_elapsed_footer() {
        let md = render(&results(vec![Issue::UnusedFile {
            path: PathBuf::from("orphan.py"),
        }]));
        let trimmed = md.trim_end();
        assert!(
            trimmed.ends_with("_5 files scanned in 42 ms._"),
            "report must end with files-and-elapsed footer, got tail: {:?}",
            &trimmed[trimmed.len().saturating_sub(80)..]
        );
    }
}
