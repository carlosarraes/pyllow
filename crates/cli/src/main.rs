use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

/// Analysis completed and every finding is either absent or non-blocking.
const EXIT_CLEAN: u8 = 0;
/// Analysis completed and produced blocking findings.
const EXIT_FINDINGS: u8 = 1;
/// Analysis did not complete: configuration, git, parsing, I/O, or internal
/// failure. Never returned for a successful run, and a run that fails this way
/// must never be reported as clean — a CI gate cannot distinguish "your code
/// has problems" from "the tool broke" if these share an exit code.
const EXIT_OPERATIONAL: u8 = 2;

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
        /// Seed a `[boundaries]` section with a curated preset. One of: bulletproof, layered, hexagonal, feature-sliced
        #[arg(long, value_name = "PRESET")]
        boundaries: Option<String>,
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
        /// Instead of mutating source, append [[suppress]] entries for current issues to pyllow.toml. Use to grandfather in legacy code.
        #[arg(long)]
        suppress: bool,
    },
    /// PR-scoped audit: combines check + dupes + health on changed files; exits with verdict
    Audit {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Base ref to diff against. Ignored when --diff-file is set.
        #[arg(long, default_value = "main")]
        base: String,
        /// Unified-diff file (e.g. `git diff origin/main > /tmp/pr.diff`). When set, scope is restricted to lines containing `+` additions rather than whole files.
        #[arg(long, value_name = "PATH")]
        diff_file: Option<PathBuf>,
        /// Analyze the exact staged Git index (pre-commit mode). Scope is the staged diff; worktree-only edits are invisible. Ignores --base.
        #[arg(long, conflicts_with = "diff_file")]
        staged: bool,
        /// Findings <= this = WARN (exit 0); > this = FAIL (exit 1). 0 = strict.
        #[arg(long, default_value_t = 0)]
        max_issues: usize,
        /// Run only these analysis families (repeatable). Unselected families are skipped entirely.
        #[arg(long, value_enum, value_name = "FAMILY")]
        only: Vec<cmd::audit::Family>,
        /// Within the selected families, report only these rules (repeatable). Requires --only.
        #[arg(long, value_name = "RULE", requires = "only")]
        rule: Vec<String>,
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
        /// Minimum distinct files in a clone family (default 2 from config; raise to surface widely-replicated patterns)
        #[arg(long, value_parser = cmd::dupes::parse_min_occurrences)]
        min_occurrences: Option<usize>,
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
    /// Print the resolved pyllow configuration for the given project root
    Config {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, value_enum, default_value_t = cmd::config::ConfigFormat::Toml)]
        format: cmd::config::ConfigFormat,
    },
    /// Print a CI workflow scaffold (GitHub Actions or GitLab CI)
    CiTemplate {
        #[arg(value_enum)]
        provider: cmd::ci_template::Provider,
        /// Write to file instead of stdout
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
    /// Re-run `check` on file change (Ctrl-C to exit)
    Watch {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, value_enum, default_value_t = report::Format::Human)]
        format: report::Format,
        #[command(flatten)]
        post: postprocess::PostFlags,
    },
    /// Scaffold pyllow.toml from another tool's config (vulture, import-linter)
    Migrate {
        #[arg(value_enum)]
        tool: cmd::migrate::SourceTool,
        /// Path to the source config file
        input: PathBuf,
        /// Write the generated pyllow.toml to this path (default: stdout)
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
    /// Print the agent-facing operating manual for pyllow
    Llm,
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

fn main() -> ExitCode {
    match run() {
        Ok(false) => ExitCode::from(EXIT_CLEAN),
        Ok(true) => ExitCode::from(EXIT_FINDINGS),
        Err(err) => {
            // Matches anyhow's own Termination formatting, so the "Caused by"
            // chain survives the move off `fn main() -> Result<()>`.
            eprintln!("Error: {err:?}");
            ExitCode::from(EXIT_OPERATIONAL)
        }
    }
}

/// Runs the requested command. `Ok(true)` means analysis completed with
/// blocking findings; any operational failure is an `Err`.
fn run() -> Result<bool> {
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
            boundaries,
        } => {
            cmd::init::run(path, pyproject, force, boundaries)?;
            false
        }
        Command::List { what, path, format } => {
            cmd::list::run(what, path, format)?;
            false
        }
        Command::Fix {
            path,
            dry_run,
            suppress,
        } => {
            cmd::fix::run(path, dry_run, suppress)?;
            false
        }
        Command::Audit {
            path,
            base,
            diff_file,
            staged,
            max_issues,
            only,
            rule,
            format,
            post,
        } => cmd::audit::run(
            path,
            base,
            diff_file,
            staged,
            max_issues,
            cmd::audit::SelectionArgs {
                families: only,
                rules: rule,
            },
            format,
            post,
        )?,
        Command::Dupes {
            path,
            window,
            min_unique,
            min_occurrences,
            mode,
            trace,
            skip_local,
            format,
            post,
        } => cmd::dupes::run(
            path,
            window,
            min_unique,
            min_occurrences,
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
        Command::Llm => {
            cmd::llm::run()?;
            false
        }
        Command::Config { path, format } => {
            cmd::config::run(path, format)?;
            false
        }
        Command::CiTemplate { provider, output } => {
            cmd::ci_template::run(provider, output)?;
            false
        }
        Command::Watch { path, format, post } => {
            cmd::watch::run(path, format, post)?;
            false
        }
        Command::Migrate {
            tool,
            input,
            output,
        } => {
            cmd::migrate::run(tool, input, output)?;
            false
        }
    };
    Ok(exit_with_findings)
}
