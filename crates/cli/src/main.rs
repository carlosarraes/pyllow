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
        /// Show only circular-dependency findings (suppresses unused-* output)
        #[arg(long)]
        circular_deps: bool,
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
        /// Token-normalization mode. weak strips literal contents; semantic also strips identifiers.
        #[arg(long, value_enum, default_value_t = cmd::dupes::DupesMode::Mild)]
        mode: cmd::dupes::DupesMode,
        /// Show only clone families containing this location (file:line)
        #[arg(long, value_name = "FILE:LINE")]
        trace: Option<String>,
        /// Exclude clone groups whose occurrences all share one directory
        #[arg(long)]
        skip_local: bool,
        #[arg(long, value_enum, default_value_t = report::Format::Human)]
        format: report::Format,
        #[command(flatten)]
        post: postprocess::PostFlags,
    },
    /// Inventory feature flags (env vars, Django settings, SDK calls)
    Flags {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, value_enum, default_value_t = report::Format::Human)]
        format: report::Format,
        #[command(flatten)]
        post: postprocess::PostFlags,
    },
    /// Detect Python anti-patterns common in AI-generated code
    Smells {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Flag files where TODO/FIXME marker count meets or exceeds this threshold
        #[arg(long, default_value_t = 5)]
        todo_threshold: u32,
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
        /// Show the N most complex functions regardless of threshold (replaces threshold filtering)
        #[arg(long, value_name = "N")]
        top: Option<usize>,
        /// Emit ranked refactoring targets classified by effort
        #[arg(long)]
        targets: bool,
        /// Filter --targets output to a single effort bucket
        #[arg(long, value_enum, value_name = "LEVEL")]
        effort: Option<cmd::health::EffortArg>,
        #[arg(long, value_enum, default_value_t = report::Format::Human)]
        format: report::Format,
        #[command(flatten)]
        post: postprocess::PostFlags,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let exit_with_findings = match cli.command {
        Command::Check {
            path,
            circular_deps,
            format,
            post,
        } => cmd::check::run(path, circular_deps, format, post)?,
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
            mode,
            trace,
            skip_local,
            format,
            post,
        } => cmd::dupes::run(
            path,
            window,
            min_unique,
            mode,
            trace,
            skip_local,
            format,
            post,
        )?,
        Command::Health {
            path,
            cyclomatic,
            cognitive,
            maintainability,
            hotspot_top,
            top,
            targets,
            effort,
            format,
            post,
        } => cmd::health::run(cmd::health::HealthArgs {
            path,
            cyclomatic,
            cognitive,
            maintainability,
            hotspot_top,
            top,
            targets,
            target_effort: effort,
            format,
            post,
        })?,
        Command::Smells {
            path,
            todo_threshold,
            format,
            post,
        } => cmd::smells::run(path, todo_threshold, format, post)?,
        Command::Flags { path, format, post } => cmd::flags::run(path, format, post)?,
    };
    if exit_with_findings {
        std::process::exit(1);
    }
    Ok(())
}
