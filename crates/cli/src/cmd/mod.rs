pub mod audit;
pub mod check;
pub mod dupes;
pub mod fix;
pub mod flags;
pub mod health;
pub mod init;
pub mod list;
pub mod llm;
pub mod smells;

use anyhow::{Context, Result};
use pyllow_config::ResolvedConfig;
use std::path::{Path, PathBuf};

pub fn load_config(path: &Path) -> Result<(ResolvedConfig, PathBuf)> {
    let project_root = path
        .canonicalize()
        .with_context(|| format!("cannot resolve path: {}", path.display()))?;
    let mut config =
        ResolvedConfig::load(&project_root).context("loading pyllow config")?;
    config.project_root = project_root.clone();
    Ok((config, project_root))
}
