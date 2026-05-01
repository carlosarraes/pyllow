//! `pyllow config` — print the resolved configuration.
//!
//! Useful for debugging entry-point discovery, plugin enable state, smell
//! rule overrides, `.pyllowignore` interactions, and `[tool.pyllow]` ↔
//! standalone `pyllow.toml` precedence. The output is exactly what every
//! other subcommand sees as input.

use anyhow::{Context, Result};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, clap::ValueEnum, Default)]
#[clap(rename_all = "lowercase")]
pub enum ConfigFormat {
    /// TOML (matches `pyllow.toml` shape; default)
    #[default]
    Toml,
    Json,
}

pub fn run(path: PathBuf, format: ConfigFormat) -> Result<()> {
    let (config, _) = super::load_config(&path)?;
    let serialized = match format {
        ConfigFormat::Toml => {
            toml::to_string_pretty(&config).context("serializing config to TOML")?
        }
        ConfigFormat::Json => {
            serde_json::to_string_pretty(&config).context("serializing config to JSON")?
        }
    };
    println!("{serialized}");
    Ok(())
}
