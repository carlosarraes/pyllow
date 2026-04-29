use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod cmd;
mod postprocess;
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
        #[command(flatten)]
        post: postprocess::PostFlags,
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
    /// PR-scoped audit: combines check + dupes + health on changed files; exits with verdict
    Audit {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Base ref to diff against
        #[arg(long, default_value = "main")]
        base: String,
        /// Findings <= this = WARN (exit 0); > this = FAIL (exit 1). 0 = strict.
        #[arg(long, default_value_t = 0)]
        max_issues: usize,
        #[arg(long, value_enum, default_value_t = report::Format::Human)]
        format: report::Format,
        #[command(flatten)]
        post: postprocess::PostFlags,
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
        #[command(flatten)]
        post: postprocess::PostFlags,
    },
    /// Compute complexity, maintainability, and hotspot metrics
    Health {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Flag functions whose cyclomatic complexity exceeds this threshold
        #[arg(long, default_value_t = 10)]
        cyclomatic: u32,
        /// Flag functions whose cognitive complexity exceeds this threshold
        #[arg(long, default_value_t = 15)]
        cognitive: u32,
        /// Flag files whose maintainability index falls below this threshold
        #[arg(long, default_value_t = 30)]
        maintainability: u32,
        /// Maximum number of hotspots (cc × git churn) to report
        #[arg(long, default_value_t = 10)]
        hotspot_top: usize,
        #[arg(long, value_enum, default_value_t = report::Format::Human)]
        format: report::Format,
        #[command(flatten)]
        post: postprocess::PostFlags,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let exit_with_findings = match cli.command {
        Command::Check { path, format, post } => cmd::check::run(path, format, post)?,
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
        Command::Audit {
            path,
            base,
            max_issues,
            format,
            post,
        } => cmd::audit::run(path, base, max_issues, format, post)?,
        Command::Dupes {
            path,
            window,
            min_unique,
            format,
            post,
        } => cmd::dupes::run(path, window, min_unique, format, post)?,
        Command::Health {
            path,
            cyclomatic,
            cognitive,
            maintainability,
            hotspot_top,
            format,
            post,
        } => cmd::health::run(
            path,
            cyclomatic,
            cognitive,
            maintainability,
            hotspot_top,
            format,
            post,
        )?,
    };
    if exit_with_findings {
        std::process::exit(1);
    }
    Ok(())
}
