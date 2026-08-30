//! Versioned machine-output envelope (issue #8).
//!
//! CI integrations depend on the fields documented in `docs/machine-output.md`,
//! not on whatever `AnalysisResults` happens to serialize to. `diagnostics` is
//! the stable, uniform view of every finding; `issues` remains the richer
//! variant-tagged view for tooling that wants per-family detail.

use super::{relative_posix, relativized};
use anyhow::{Context, Result};
use pyllow_types::{AnalysisResults, AnalysisStats, Issue};
use serde::Serialize;
use std::path::Path;

/// Bump only on a breaking change to the envelope. Additive fields do not
/// bump it — see the compatibility policy in `docs/machine-output.md`.
pub const SCHEMA_VERSION: u32 = 1;
pub const TOOL_NAME: &str = "pyllow";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Diagnostic {
    /// Repository-relative POSIX path.
    path: String,
    /// One-based inclusive. `null` for file-level findings, which own no
    /// specific range — never faked to line 1.
    start_line: Option<u32>,
    end_line: Option<u32>,
    rule: String,
    message: String,
}

/// Executed-rule metadata. Grouped under `rules` so #1 and #4 can add
/// `requested`/`disabled` alongside `executed` without a schema bump.
#[derive(Serialize)]
struct Rules<'a> {
    executed: &'a [String],
    requested: &'a [String],
}

#[derive(Serialize)]
struct Families<'a> {
    executed: &'a [String],
    requested: &'a [String],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Envelope<'a> {
    schema_version: u32,
    tool: &'static str,
    rules: Rules<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    families: Option<Families<'a>>,
    diagnostics: Vec<Diagnostic>,
    issues: Vec<Issue>,
    stats: &'a AnalysisStats,
}

fn diagnostic(issue: &Issue, project_root: &Path) -> Diagnostic {
    let (start_line, end_line) = match issue.range() {
        Some((start, end)) => (Some(start), Some(end)),
        None => (None, None),
    };
    Diagnostic {
        path: relative_posix(issue.path(), project_root),
        start_line,
        end_line,
        rule: issue.rule_key().into_owned(),
        message: issue.message(),
    }
}

pub fn render(results: &AnalysisResults, project_root: &Path) -> Result<String> {
    let envelope = Envelope {
        schema_version: SCHEMA_VERSION,
        tool: TOOL_NAME,
        rules: Rules {
            executed: &results.executed_rules,
            requested: results
                .selection
                .as_ref()
                .map_or(&[][..], |s| &s.rules_requested),
        },
        families: results.selection.as_ref().map(|s| Families {
            executed: &s.families_executed,
            requested: &s.families_requested,
        }),
        diagnostics: results
            .issues
            .iter()
            .map(|i| diagnostic(i, project_root))
            .collect(),
        issues: results
            .issues
            .iter()
            .map(|i| relativized(i, project_root))
            .collect(),
        stats: &results.stats,
    };
    serde_json::to_string_pretty(&envelope).context("serializing analysis results to JSON")
}

pub fn print(results: &AnalysisResults, project_root: &Path) -> Result<()> {
    println!("{}", render(results, project_root)?);
    Ok(())
}
