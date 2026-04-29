use crate::postprocess::{apply, note_baseline_filter, PostFlags};
use crate::report::Format;
use anyhow::{Context, Result};
use pyllow_analyzer::analyze;
use std::path::PathBuf;

pub fn run(path: PathBuf, format: Format, post: PostFlags) -> Result<bool> {
    let (config, project_root) = super::load_config(&path)?;
    let mut results = analyze(&config).context("analysis failed")?;
    let suppressed = apply(&mut results, &project_root, &post)?;
    note_baseline_filter(suppressed, &post.baseline);
    let has_issues = !results.issues.is_empty();
    format.print(&results);
    Ok(has_issues)
}
