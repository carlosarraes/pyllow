use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use pyllow_analyzer::analyze;
use pyllow_config::ResolvedConfig;
use std::path::PathBuf;

mod report;

#[derive(Parser)]
#[command(name = "pyllow", version, about = "Codebase intelligence for Python")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Check {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, value_enum, default_value_t = report::Format::Human)]
        format: report::Format,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Check { path, format } => {
            let project_root = path
                .canonicalize()
                .with_context(|| format!("cannot resolve path: {}", path.display()))?;
            let mut config =
                ResolvedConfig::load(&project_root).context("loading pyllow config")?;
            config.project_root = project_root;
            let results = analyze(&config).context("analysis failed")?;
            let has_issues = !results.issues.is_empty();
            format.print(&results);
            if has_issues {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}
