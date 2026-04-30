//! SARIF 2.1.0 output for pyllow.
//!
//! Spec: <https://docs.oasis-open.org/sarif/sarif/v2.1.0/sarif-v2.1.0.html>
//!
//! GitHub Code Scanning + GitLab Code Quality both consume this format
//! directly. Each `Issue` variant maps to a SARIF `result` linked back to a
//! single rule definition in `tool.driver.rules`.

use pyllow_types::{AnalysisResults, Issue};
use serde_json::{json, Value};

const SCHEMA: &str = "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/main/Documents/CommitteeSpecifications/2.1.0/sarif-schema-2.1.0.json";
const VERSION: &str = "2.1.0";
const TOOL_NAME: &str = "pyllow";
const README_BASE: &str =
    "https://github.com/carlosarraes/pyllow/blob/main/README.md";

pub fn print(results: &AnalysisResults) {
    let report = build(results);
    match serde_json::to_string_pretty(&report) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("error serializing SARIF: {e}"),
    }
}

fn build(results: &AnalysisResults) -> Value {
    let rules = build_rule_catalog(&results.issues);
    let rule_index: std::collections::HashMap<&str, usize> = rules
        .iter()
        .enumerate()
        .filter_map(|(i, r)| r.get("id").and_then(|v| v.as_str()).map(|id| (id, i)))
        .collect();

    let sarif_results: Vec<Value> = results
        .issues
        .iter()
        .map(|issue| issue_to_result(issue, &rule_index))
        .collect();

    json!({
        "$schema": SCHEMA,
        "version": VERSION,
        "runs": [{
            "tool": {
                "driver": {
                    "name": TOOL_NAME,
                    "version": env!("CARGO_PKG_VERSION"),
                    "informationUri": "https://github.com/carlosarraes/pyllow",
                    "rules": rules,
                }
            },
            "results": sarif_results,
        }]
    })
}

/// Walk all issues, collect distinct rule keys, and emit one rule object each.
/// Order is stable (kebab-case alphabetical) so SARIF diffs are diffable.
fn build_rule_catalog(issues: &[Issue]) -> Vec<Value> {
    let mut keys: Vec<&'static str> = issues.iter().map(|i| i.rule_key()).collect();
    keys.sort_unstable();
    keys.dedup();
    keys.iter()
        .map(|k| {
            json!({
                "id": k,
                "name": kebab_to_pascal(k),
                "shortDescription": { "text": rule_short_description(k) },
                "helpUri": format!("{}#rule-{}", README_BASE, k),
                "defaultConfiguration": { "level": rule_level(k) },
            })
        })
        .collect()
}

fn issue_to_result(issue: &Issue, rule_index: &std::collections::HashMap<&str, usize>) -> Value {
    let rule_id = issue.rule_key();
    let mut result = json!({
        "ruleId": rule_id,
        "level": rule_level(rule_id),
        "message": { "text": issue_message(issue) },
        "locations": [physical_location(issue)],
    });
    if let Some(idx) = rule_index.get(rule_id) {
        result["ruleIndex"] = json!(*idx);
    }
    // For Duplicate / CircularDependency: surface every involved file as a
    // related location so reviewers can jump to all sites of the issue.
    let related = related_locations(issue);
    if !related.is_empty() {
        result["relatedLocations"] = json!(related);
    }
    result
}

fn physical_location(issue: &Issue) -> Value {
    let path = issue.path();
    let mut region = json!({});
    if let Some(line) = issue.line() {
        region["startLine"] = json!(line);
    }
    json!({
        "physicalLocation": {
            "artifactLocation": { "uri": path.to_string_lossy() },
            "region": region,
        }
    })
}

fn related_locations(issue: &Issue) -> Vec<Value> {
    match issue {
        Issue::Duplicate { occurrences, .. } => occurrences
            .iter()
            .skip(1) // first is already in `locations`
            .map(|o| {
                json!({
                    "physicalLocation": {
                        "artifactLocation": { "uri": o.path.to_string_lossy() },
                        "region": {
                            "startLine": o.start_line,
                            "endLine": o.end_line,
                        },
                    }
                })
            })
            .collect(),
        Issue::CircularDependency { cycle } => cycle
            .iter()
            .skip(1)
            .map(|p| {
                json!({
                    "physicalLocation": {
                        "artifactLocation": { "uri": p.to_string_lossy() },
                    }
                })
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn issue_message(issue: &Issue) -> String {
    match issue {
        Issue::UnusedFile { path } => format!("File `{}` is not reachable from any entry point", path.display()),
        Issue::UnusedImport { name, module, .. } => format!("Unused import `{name}` from `{module}`"),
        Issue::UnusedDep { name, source, .. } => format!("Dependency `{name}` declared in `{source}` is never imported"),
        Issue::Duplicate { token_count, occurrences } => format!(
            "Duplicate code: {} tokens repeated across {} location(s)",
            token_count,
            occurrences.len()
        ),
        Issue::Complexity { function, cyclomatic, cognitive, .. } => format!(
            "Function `{function}` has high complexity (cyclomatic={cyclomatic}, cognitive={cognitive})"
        ),
        Issue::LowMaintainability { score, avg_cyclomatic, loc, .. } => format!(
            "Low maintainability index: {score} (avg cc={avg_cyclomatic:.1}, loc={loc})"
        ),
        Issue::Hotspot { cyclomatic, churn, score, .. } => format!(
            "Hotspot: cc={cyclomatic} × churn={churn} → {score:.1}"
        ),
        Issue::Smell { detail, .. } => detail.clone(),
        Issue::CircularDependency { cycle } => {
            let names: Vec<String> = cycle
                .iter()
                .map(|p| {
                    p.file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or_default()
                        .to_string()
                })
                .collect();
            format!("Circular dependency: {}", names.join(" \u{2192} "))
        }
    }
}

fn rule_short_description(rule_id: &str) -> &'static str {
    match rule_id {
        "unused-file" => "File is not reachable from any entry point",
        "unused-import" => "Imported name is never used in the module",
        "unused-dep" => "Dependency is declared but never imported",
        "duplicate" => "Repeated code block detected across the codebase",
        "complexity" => "Function exceeds cyclomatic or cognitive complexity threshold",
        "low-maintainability" => "File maintainability index falls below threshold",
        "hotspot" => "File has high complexity × git churn (refactor risk)",
        "circular-dependency" => "Module import graph contains a cycle",
        "mutable-default" => "Function argument has a mutable default value",
        "broad-except" => "except: or except Exception: catches too broadly",
        "sentinel-equality" => "Compare against True/False/None using `is` not `==`",
        "truthy-length-check" => "Use truthy/falsy check instead of len(x) == 0 / > 0",
        "unreachable-after-exit" => "Statement after return/raise/break/continue is unreachable",
        "passthrough-function" => "Wrapper function only forwards arguments",
        "stray-print" => "print() in non-CLI module — use logging",
        "single-method-class" => "Class has one method and no state — could be a function",
        "high-todo-density" => "File contains many TODO/FIXME markers",
        "raise-from-none" => "raise ... from None discards the original exception",
        _ => "Pyllow finding",
    }
}

/// Map a rule key to a SARIF default level: error / warning / note.
fn rule_level(rule_id: &str) -> &'static str {
    match rule_id {
        // Hard correctness/maintenance failures
        "circular-dependency"
        | "unused-file"
        | "mutable-default"
        | "raise-from-none"
        | "low-maintainability" => "error",
        // Likely problems but case-by-case
        "unused-import"
        | "unused-dep"
        | "broad-except"
        | "unreachable-after-exit"
        | "duplicate"
        | "complexity"
        | "hotspot" => "warning",
        // Stylistic / code-smell signals
        _ => "note",
    }
}

fn kebab_to_pascal(s: &str) -> String {
    s.split('-')
        .map(|w| {
            let mut chars = w.chars();
            chars
                .next()
                .map(|c| c.to_ascii_uppercase().to_string() + chars.as_str())
                .unwrap_or_default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_types::{AnalysisStats, SmellRule};
    use std::path::PathBuf;

    fn results_with(issues: Vec<Issue>) -> AnalysisResults {
        AnalysisResults {
            issues,
            stats: AnalysisStats::default(),
        }
    }

    #[test]
    fn empty_run_emits_zero_results() {
        let report = build(&results_with(vec![]));
        assert_eq!(report["version"], VERSION);
        assert_eq!(report["runs"][0]["results"].as_array().unwrap().len(), 0);
        assert_eq!(report["runs"][0]["tool"]["driver"]["name"], TOOL_NAME);
    }

    #[test]
    fn rule_catalog_is_deduplicated_and_sorted() {
        let report = build(&results_with(vec![
            Issue::UnusedImport {
                path: PathBuf::from("a.py"),
                line: 1,
                name: "os".into(),
                module: "os".into(),
            },
            Issue::UnusedImport {
                path: PathBuf::from("b.py"),
                line: 2,
                name: "sys".into(),
                module: "sys".into(),
            },
            Issue::UnusedFile {
                path: PathBuf::from("dead.py"),
            },
        ]));
        let rules = report["runs"][0]["tool"]["driver"]["rules"]
            .as_array()
            .unwrap();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0]["id"], "unused-file");
        assert_eq!(rules[1]["id"], "unused-import");
    }

    #[test]
    fn smell_emits_correct_rule_id_and_level() {
        let report = build(&results_with(vec![Issue::Smell {
            path: PathBuf::from("x.py"),
            line: 5,
            rule: SmellRule::MutableDefault,
            detail: "argument `x` has mutable default".into(),
        }]));
        let result = &report["runs"][0]["results"][0];
        assert_eq!(result["ruleId"], "mutable-default");
        assert_eq!(result["level"], "error");
        assert_eq!(
            result["locations"][0]["physicalLocation"]["region"]["startLine"],
            5
        );
    }

    #[test]
    fn circular_dependency_emits_related_locations() {
        let report = build(&results_with(vec![Issue::CircularDependency {
            cycle: vec![
                PathBuf::from("a.py"),
                PathBuf::from("b.py"),
                PathBuf::from("c.py"),
            ],
        }]));
        let result = &report["runs"][0]["results"][0];
        assert_eq!(result["ruleId"], "circular-dependency");
        let related = result["relatedLocations"].as_array().unwrap();
        assert_eq!(related.len(), 2); // first file is in `locations`, rest in related
    }

    #[test]
    fn duplicate_emits_related_locations() {
        use pyllow_types::DuplicateOccurrence;
        let report = build(&results_with(vec![Issue::Duplicate {
            token_count: 50,
            occurrences: vec![
                DuplicateOccurrence {
                    path: PathBuf::from("a.py"),
                    start_line: 1,
                    end_line: 10,
                },
                DuplicateOccurrence {
                    path: PathBuf::from("b.py"),
                    start_line: 20,
                    end_line: 29,
                },
            ],
        }]));
        let result = &report["runs"][0]["results"][0];
        assert_eq!(result["ruleId"], "duplicate");
        let related = result["relatedLocations"].as_array().unwrap();
        assert_eq!(related.len(), 1);
        assert_eq!(related[0]["physicalLocation"]["region"]["startLine"], 20);
    }
}
