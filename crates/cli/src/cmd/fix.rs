use anyhow::{Context, Result};
use colored::Colorize;
use pyllow_analyzer::analyze;
use pyllow_types::Issue;
use rustc_hash::FxHashMap;
use std::fs;
use std::path::PathBuf;

pub fn run(path: PathBuf, dry_run: bool) -> Result<()> {
    let (config, _root) = super::load_config(&path)?;
    let results = analyze(&config).context("analysis failed")?;

    let mut by_file: FxHashMap<PathBuf, Vec<(u32, String)>> = FxHashMap::default();
    for issue in &results.issues {
        if let Issue::UnusedImport { path, line, name, .. } = issue {
            by_file
                .entry(path.clone())
                .or_default()
                .push((*line, name.clone()));
        }
    }

    if by_file.is_empty() {
        println!("{} no unused imports to fix", "ok".green().bold());
        return Ok(());
    }

    let mut total_removed = 0usize;
    let mut total_skipped = 0usize;
    let mut files_changed = 0usize;
    let mut files_with_skips: Vec<(PathBuf, Vec<(u32, String)>)> = Vec::new();

    for (file_path, mut issues) in by_file {
        issues.sort_by(|a, b| b.0.cmp(&a.0));
        let raw = fs::read_to_string(&file_path)
            .with_context(|| format!("reading {}", file_path.display()))?;
        let mut lines: Vec<String> = raw.lines().map(String::from).collect();
        let trailing_newline = raw.ends_with('\n');

        let mut removed_in_file = 0usize;
        let mut skipped_in_file: Vec<(u32, String)> = Vec::new();

        for (line_no, name) in &issues {
            let idx = (*line_no as usize).saturating_sub(1);
            let Some(line) = lines.get(idx) else {
                skipped_in_file.push((*line_no, name.clone()));
                continue;
            };
            if can_safely_remove(line, name) {
                lines.remove(idx);
                removed_in_file += 1;
            } else {
                skipped_in_file.push((*line_no, name.clone()));
            }
        }

        if removed_in_file > 0 {
            total_removed += removed_in_file;
            files_changed += 1;
            if !dry_run {
                let mut out = lines.join("\n");
                if trailing_newline {
                    out.push('\n');
                }
                fs::write(&file_path, out)
                    .with_context(|| format!("writing {}", file_path.display()))?;
            }
            println!(
                "{} {} ({} removed)",
                if dry_run { "would fix".yellow() } else { "fixed".green() },
                file_path.display(),
                removed_in_file
            );
        }

        total_skipped += skipped_in_file.len();
        if !skipped_in_file.is_empty() {
            files_with_skips.push((file_path, skipped_in_file));
        }
    }

    println!();
    println!(
        "{} {} import{} {} across {} file{}",
        if dry_run {
            "would remove".yellow().bold()
        } else {
            "removed".green().bold()
        },
        total_removed,
        if total_removed == 1 { "" } else { "s" },
        if dry_run { "(dry run)" } else { "" }.dimmed(),
        files_changed,
        if files_changed == 1 { "" } else { "s" },
    );
    if total_skipped > 0 {
        println!(
            "{} {} import{} on multi-binding lines (manual edit required):",
            "skipped".dimmed(),
            total_skipped,
            if total_skipped == 1 { "" } else { "s" },
        );
        for (file, skips) in &files_with_skips {
            for (line, name) in skips {
                println!("  {}:{} {}", file.display(), line, name);
            }
        }
    }
    Ok(())
}

fn can_safely_remove(line: &str, name: &str) -> bool {
    let trimmed = line.trim_start();
    let stripped = match trimmed.find('#') {
        Some(idx) => trimmed[..idx].trim_end(),
        None => trimmed.trim_end(),
    };
    if stripped.is_empty() {
        return false;
    }
    // `import <module>` — sole import; bound name is name
    if let Some(rest) = stripped.strip_prefix("import ") {
        if rest.contains(',') || rest.contains('(') {
            return false;
        }
        if let Some((module, alias)) = rest.split_once(" as ") {
            return alias.trim() == name && !module.contains(',');
        }
        let first_segment = rest.split('.').next().unwrap_or("").trim();
        return first_segment == name;
    }
    // `from <module> import <name>` or `from <module> import <name> as <alias>`
    if let Some(rest) = stripped.strip_prefix("from ") {
        let Some(import_idx) = rest.find(" import ") else {
            return false;
        };
        let import_part = rest[import_idx + " import ".len()..].trim();
        if import_part.contains(',') || import_part.starts_with('(') {
            return false;
        }
        if let Some((bound_name, alias)) = import_part.split_once(" as ") {
            return alias.trim() == name && !bound_name.contains(',');
        }
        return import_part == name;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safely_removes_simple_import() {
        assert!(can_safely_remove("import os", "os"));
        assert!(can_safely_remove("import os  ", "os"));
        assert!(can_safely_remove("    import os", "os"));
        assert!(can_safely_remove("import os  # noqa: ignored", "os"));
    }

    #[test]
    fn safely_removes_aliased_import() {
        assert!(can_safely_remove("import numpy as np", "np"));
    }

    #[test]
    fn safely_removes_dotted_import() {
        assert!(can_safely_remove("import os.path", "os"));
    }

    #[test]
    fn safely_removes_from_import() {
        assert!(can_safely_remove("from typing import List", "List"));
        assert!(can_safely_remove("from typing import List as L", "L"));
    }

    #[test]
    fn refuses_multi_binding_lines() {
        assert!(!can_safely_remove("import os, sys", "os"));
        assert!(!can_safely_remove("from typing import List, Dict", "List"));
    }

    #[test]
    fn refuses_paren_grouped_imports() {
        assert!(!can_safely_remove("from typing import (List)", "List"));
        assert!(!can_safely_remove("from typing import (", "List"));
    }

    #[test]
    fn refuses_unrelated_lines() {
        assert!(!can_safely_remove("x = 1", "x"));
        assert!(!can_safely_remove("def import_data():", "data"));
    }
}
