use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod cmd;
mod report;

#[derive(Parser)]
#[command(name = "pyllow", version, about = "Codebase intelligence for Python")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Analyze for unused files and unused imports
    Check {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, value_enum, default_value_t = report::Format::Human)]
        format: report::Format,
    },
    /// Scaffold pyllow.toml (or [tool.pyllow] in pyproject.toml)
    Init {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Write [tool.pyllow] into existing pyproject.toml instead of creating pyllow.toml
        #[arg(long)]
        pyproject: bool,
        /// Overwrite an existing config
        #[arg(long)]
        force: bool,
    },
    /// Inspect what pyllow sees: entry points, files, plugins
    List {
        /// What to list. Use `all` for everything.
        #[arg(value_enum, default_value_t = cmd::list::What::All)]
        what: cmd::list::What,
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, value_enum, default_value_t = report::Format::Human)]
        format: report::Format,
    },
    /// Auto-remove unused imports detected by `check`
    Fix {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Print what would change without modifying files
        #[arg(long)]
        dry_run: bool,
    },
    /// PR-scoped check: only flag issues in files changed since base branch
    Audit {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Base ref to diff against
        #[arg(long, default_value = "main")]
        base: String,
        #[arg(long, value_enum, default_value_t = report::Format::Human)]
        format: report::Format,
    },
    /// Detect duplicate code blocks (token-normalized clones)
    Dupes {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Window size (number of consecutive tokens to compare)
        #[arg(long, default_value_t = 50)]
        window: usize,
        /// Minimum unique token kinds in a window for it to count
        #[arg(long, default_value_t = 6)]
        min_unique: usize,
        #[arg(long, value_enum, default_value_t = report::Format::Human)]
        format: report::Format,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let exit_with_findings = match cli.command {
        Command::Check { path, format } => cmd::check::run(path, format)?,
        Command::Init {
            path,
            pyproject,
            force,
        } => {
            cmd::init::run(path, pyproject, force)?;
            false
        }
        Command::List { what, path, format } => {
            cmd::list::run(what, path, format)?;
            false
        }
        Command::Fix { path, dry_run } => {
            cmd::fix::run(path, dry_run)?;
            false
        }
        Command::Audit { path, base, format } => cmd::audit::run(path, base, format)?,
        Command::Dupes {
            path,
            window,
            min_unique,
            format,
        } => cmd::dupes::run(path, window, min_unique, format)?,
    };
    if exit_with_findings {
        std::process::exit(1);
    }
    Ok(())
}
