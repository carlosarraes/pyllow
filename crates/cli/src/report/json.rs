//! Versioned machine-output envelope (issue #8).
//!
//! CI integrations depend on the fields documented in `docs/machine-output.md`,
//! not on whatever `AnalysisResults` happens to serialize to. `diagnostics` is
//! the stable, uniform view of every finding; `issues` remains the richer
//! variant-tagged view for tooling that wants per-family detail.

use anyhow::{Context, Result};
use pyllow_types::{AnalysisResults, AnalysisStats, Issue};
use serde::Serialize;
use std::path::{Path, PathBuf};

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
    rule: &'static str,
    message: String,
}

/// Executed-rule metadata. Grouped under `rules` so #1 and #4 can add
/// `requested`/`disabled` alongside `executed` without a schema bump.
#[derive(Serialize)]
struct Rules<'a> {
    executed: &'a [String],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Envelope<'a> {
    schema_version: u32,
    tool: &'static str,
    rules: Rules<'a>,
    diagnostics: Vec<Diagnostic>,
    issues: Vec<Issue>,
    stats: &'a AnalysisStats,
}

/// Convert an absolute issue path into a repository-relative POSIX path.
/// Canonicalizes both sides first so symlinked roots (`/tmp` → `/private/tmp`)
/// still strip cleanly; falls back to a raw strip for paths that no longer
/// exist on disk.
fn relative_posix(path: &Path, project_root: &Path) -> String {
    let stripped = match (path.canonicalize(), project_root.canonicalize()) {
        (Ok(p), Ok(root)) => p.strip_prefix(&root).map(Path::to_path_buf).ok(),
        _ => None,
    }
    .or_else(|| path.strip_prefix(project_root).map(Path::to_path_buf).ok())
    .unwrap_or_else(|| path.to_path_buf());

    stripped
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
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
        rule: issue.rule_key(),
        message: issue.message(),
    }
}

/// Rewrite every path on the issue to be repository-relative, so the detailed
/// `issues` view obeys the same path rule as `diagnostics`.
fn relativized(issue: &Issue, project_root: &Path) -> Issue {
    let mut copy = issue.clone();
    for path in copy.paths_mut() {
        *path = PathBuf::from(relative_posix(path, project_root));
    }
    copy
}

pub fn render(results: &AnalysisResults, project_root: &Path) -> Result<String> {
    let envelope = Envelope {
        schema_version: SCHEMA_VERSION,
        tool: TOOL_NAME,
        rules: Rules {
            executed: &results.executed_rules,
        },
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
