use crate::report::Format;
use anyhow::{Context, Result};
use pyllow_analyzer::analyze;
use std::path::PathBuf;

pub fn run(path: PathBuf, format: Format) -> Result<bool> {
    let (config, _root) = super::load_config(&path)?;
    let results = analyze(&config).context("analysis failed")?;
    let has_issues = !results.issues.is_empty();
    format.print(&results);
    Ok(has_issues)
}
