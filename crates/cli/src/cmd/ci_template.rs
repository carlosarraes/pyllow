//! `pyllow ci-template` — print a CI workflow scaffold.
//!
//! Removes the "how do I wire this up?" friction of CI integration. Default
//! emits to stdout (so it can be redirected); `--output` writes to a file.

use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

const GITHUB_ACTIONS: &str = include_str!("templates/github_actions.yml");
const GITLAB_CI: &str = include_str!("templates/gitlab_ci.yml");

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
#[clap(rename_all = "lowercase")]
pub enum Provider {
    /// GitHub Actions workflow with SARIF upload to Code Scanning
    Github,
    /// GitLab CI job with merge-request rules + Code Quality artifact
    Gitlab,
}

pub fn run(provider: Provider, output: Option<PathBuf>) -> Result<()> {
    let body = match provider {
        Provider::Github => GITHUB_ACTIONS,
        Provider::Gitlab => GITLAB_CI,
    };
    match output {
        Some(path) => {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("creating directory {}", parent.display()))?;
                }
            }
            fs::write(&path, body)
                .with_context(|| format!("writing template to {}", path.display()))?;
            eprintln!("wrote {} ({} bytes)", path.display(), body.len());
        }
        None => print!("{body}"),
    }
    Ok(())
}
