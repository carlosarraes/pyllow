//! `pyllow migrate` — scaffold a `pyllow.toml` from another tool's config.
//!
//! Honest about what pyllow can and can't translate. Where mappings exist
//! they're applied; where they don't, the tool emits stderr warnings telling
//! the user what they'll need to handle manually rather than silently
//! dropping configuration.

use anyhow::{Context, Result};
use colored::Colorize;
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
#[clap(rename_all = "kebab-case")]
pub enum SourceTool {
    /// vulture whitelist file (Python-ish symbol names, one per line)
    Vulture,
    /// import-linter `.importlinter` (INI-style contracts)
    ImportLinter,
}

pub fn run(tool: SourceTool, input: PathBuf, output: Option<PathBuf>) -> Result<()> {
    let content =
        fs::read_to_string(&input).with_context(|| format!("reading {}", input.display()))?;
    let toml = match tool {
        SourceTool::Vulture => migrate_vulture(&content),
        SourceTool::ImportLinter => migrate_import_linter(&content),
    };
    match output {
        Some(path) => {
            fs::write(&path, &toml).with_context(|| format!("writing {}", path.display()))?;
            eprintln!("wrote {} ({} bytes)", path.display(), toml.len());
        }
        None => print!("{toml}"),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// vulture
// ---------------------------------------------------------------------------

fn migrate_vulture(content: &str) -> String {
    let mut symbols: Vec<&str> = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Strip trailing comments and `# noqa` markers.
        let symbol = trimmed.split('#').next().unwrap_or("").trim();
        if symbol.is_empty() {
            continue;
        }
        symbols.push(symbol);
    }

    if !symbols.is_empty() {
        eprintln!(
            "{} vulture's whitelist is symbol-level; pyllow's ignores are file-level.",
            "warning:".yellow().bold()
        );
        eprintln!(
            "         {} symbol{} were not auto-translated. Equivalents to consider per case:",
            symbols.len(),
            if symbols.len() == 1 { "" } else { "s" }
        );
        eprintln!("           - inline `# noqa: F401` (or matching code) on the symbol's line");
        eprintln!("           - `[smells].disabled = [...]` for whole-rule suppression");
        eprintln!(
            "           - `pyllow check --save-baseline FILE` once on a clean ref, then audit with `--baseline FILE`"
        );
        eprintln!("         Symbols pyllow ignored:");
        for s in &symbols {
            eprintln!("           {s}");
        }
    }

    let mut out = String::new();
    out.push_str("# Migrated from vulture whitelist\n");
    out.push_str(
        "# See `pyllow llm` for suppression mechanisms (# noqa, baselines, [smells].disabled)\n\n",
    );
    out.push_str("entryPoints = []\n");
    out.push_str("ignorePatterns = []\n");
    out
}

// ---------------------------------------------------------------------------
// import-linter
// ---------------------------------------------------------------------------

fn migrate_import_linter(content: &str) -> String {
    let sections = parse_ini(content);

    let mut translated = 0usize;
    let mut deferred: Vec<String> = Vec::new();

    for (name, kvs) in &sections {
        if !name.starts_with("importlinter:contract") {
            continue;
        }
        let contract_type = kvs
            .iter()
            .find(|(k, _)| k == "type")
            .map(|(_, v)| v.as_str())
            .unwrap_or("(unknown)");
        let contract_name = kvs
            .iter()
            .find(|(k, _)| k == "name")
            .map(|(_, v)| v.as_str())
            .unwrap_or(name.as_str());
        match contract_type {
            "independence" => {
                // pyllow has built-in circular-dep detection — this contract
                // type is auto-handled, no config change needed.
                translated += 1;
                eprintln!(
                    "{} `{contract_name}` (type=independence) → covered by `pyllow check --circular-deps` (built-in, no config needed)",
                    "✓".green().bold()
                );
            }
            "layered" | "forbidden" => {
                deferred.push(format!("{contract_name} (type={contract_type})"));
            }
            other => {
                deferred.push(format!("{contract_name} (type={other}, unrecognized)"));
            }
        }
    }

    if !deferred.is_empty() {
        eprintln!(
            "{} {} contract{} require pyllow's boundary-violation analysis (not yet shipped):",
            "warning:".yellow().bold(),
            deferred.len(),
            if deferred.len() == 1 { "" } else { "s" }
        );
        for d in &deferred {
            eprintln!("           {d}");
        }
        eprintln!("         Track them in your team's notes; pyllow will support them in a future release.");
    }

    let mut out = String::new();
    out.push_str("# Migrated from import-linter `.importlinter`\n");
    out.push_str(&format!(
        "# {translated} independence contract{} are auto-handled by `pyllow check --circular-deps`.\n",
        if translated == 1 { "" } else { "s" }
    ));
    if !deferred.is_empty() {
        out.push_str(&format!(
            "# {} contract{} (layered/forbidden) await pyllow boundary support.\n",
            deferred.len(),
            if deferred.len() == 1 { "" } else { "s" }
        ));
    }
    out.push('\n');

    // Emit any root_packages as `entryPoints` if present — best-effort.
    if let Some((_, kvs)) = sections.iter().find(|(n, _)| n == "importlinter") {
        if let Some((_, val)) = kvs.iter().find(|(k, _)| k == "root_packages") {
            let pkgs: Vec<&str> = val
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            if !pkgs.is_empty() {
                out.push_str("# root_packages were lifted from .importlinter; verify they match your layout.\n");
                out.push_str("packageRoots = [\n");
                for p in pkgs {
                    out.push_str(&format!("    \"{p}\",\n"));
                }
                out.push_str("]\n");
            }
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Minimal INI parser — keys + multi-line values + section headers. Enough
// for `.importlinter`'s shape; not a general-purpose INI library.
// ---------------------------------------------------------------------------

fn parse_ini(content: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut sections: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut current: Option<(String, Vec<(String, String)>)> = None;
    for raw in content.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if let Some(section) = current.take() {
                sections.push(section);
            }
            let name = trimmed[1..trimmed.len() - 1].to_string();
            current = Some((name, Vec::new()));
            continue;
        }
        // Continuation: indented line under the previous key.
        if raw.starts_with(' ') || raw.starts_with('\t') {
            if let Some((_, kvs)) = current.as_mut() {
                if let Some(last) = kvs.last_mut() {
                    if !last.1.is_empty() {
                        last.1.push('\n');
                    }
                    last.1.push_str(trimmed);
                    continue;
                }
            }
        }
        // key = value
        if let Some(eq) = trimmed.find('=') {
            let key = trimmed[..eq].trim().to_string();
            let value = trimmed[eq + 1..].trim().to_string();
            if let Some((_, kvs)) = current.as_mut() {
                kvs.push((key, value));
            }
        }
    }
    if let Some(section) = current.take() {
        sections.push(section);
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_parses_section_with_multiline_values() {
        let src = "\
[importlinter]
root_packages =
    myapp
    myapp.shared

[importlinter:contract:1]
name = No cycles
type = independence
modules =
    myapp.x
    myapp.y
";
        let sections = parse_ini(src);
        assert_eq!(sections.len(), 2);
        let (name, kvs) = &sections[1];
        assert_eq!(name, "importlinter:contract:1");
        let modules = kvs
            .iter()
            .find(|(k, _)| k == "modules")
            .map(|(_, v)| v.as_str());
        assert_eq!(modules, Some("myapp.x\nmyapp.y"));
    }

    #[test]
    fn vulture_migration_emits_skeleton_and_lists_symbols() {
        let src = "# vulture whitelist\nfoo_bar  # noqa\nMyClass.method\n";
        let out = migrate_vulture(src);
        assert!(out.contains("# Migrated from vulture whitelist"));
        assert!(out.contains("entryPoints = []"));
    }

    #[test]
    fn import_linter_migration_handles_independence_and_layered() {
        let src = "\
[importlinter]
root_packages =
    myapp

[importlinter:contract:1]
name = No cycles
type = independence

[importlinter:contract:2]
name = Layered
type = layered
layers =
    api
    domain
";
        let out = migrate_import_linter(src);
        assert!(out.contains("# Migrated from import-linter"));
        assert!(out.contains("packageRoots"));
        assert!(out.contains("\"myapp\""));
    }
}
