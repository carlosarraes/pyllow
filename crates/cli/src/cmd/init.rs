use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_PYLLOW_TOML: &str = r#"# pyllow.toml — codebase intelligence for Python
# https://github.com/carlosarraes/pyllow

# Roots that contain importable Python packages.
# Auto-detected from `src/` layout if omitted.
# packageRoots = ["src"]

# Files pyllow should treat as reachability roots beyond what plugins detect.
# Useful for FastAPI factory patterns (`app = create_app(...)` in main.py).
# entryPoints = ["src/main.py"]

# Per-plugin overrides. All plugins enabled by default.
# [plugins.fastapi]
# enabled = true
"#;

const DEFAULT_PYLLOWIGNORE: &str =
    "# pyllow ignore patterns (gitignore-style globs)\n# Lines starting with # are comments.\n";

pub fn run(path: PathBuf, write_pyproject: bool, force: bool) -> Result<()> {
    let project_root = path
        .canonicalize()
        .with_context(|| format!("cannot resolve path: {}", path.display()))?;

    if write_pyproject {
        write_into_pyproject(&project_root, force)?;
    } else {
        write_pyllow_toml(&project_root, force)?;
    }
    Ok(())
}

fn write_pyllow_toml(root: &Path, force: bool) -> Result<()> {
    let target = root.join("pyllow.toml");
    if target.exists() && !force {
        bail!(
            "{} already exists; use --force to overwrite",
            target.display()
        );
    }
    fs::write(&target, DEFAULT_PYLLOW_TOML)
        .with_context(|| format!("writing {}", target.display()))?;
    println!("Created {}", target.display());

    let ignore_path = root.join(".pyllowignore");
    if !ignore_path.exists() {
        fs::write(&ignore_path, DEFAULT_PYLLOWIGNORE)
            .with_context(|| format!("writing {}", ignore_path.display()))?;
        println!("Created {}", ignore_path.display());
    }
    Ok(())
}

fn write_into_pyproject(root: &Path, force: bool) -> Result<()> {
    let target = root.join("pyproject.toml");
    if !target.exists() {
        bail!(
            "{} not found; run `pyllow init` (without --pyproject) to create pyllow.toml instead",
            target.display()
        );
    }
    let existing =
        fs::read_to_string(&target).with_context(|| format!("reading {}", target.display()))?;

    let has_existing = has_tool_pyllow_section(&existing);
    if has_existing && !force {
        bail!(
            "{} already has a [tool.pyllow] section; use --force to overwrite",
            target.display()
        );
    }

    let new_contents = if has_existing && force {
        replace_tool_pyllow(&existing)
    } else {
        let mut s = existing;
        if !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str("\n[tool.pyllow]\n");
        s.push_str("# Roots that contain importable Python packages (auto-detected if omitted).\n");
        s.push_str("# packageRoots = [\"src\"]\n");
        s.push_str("# entryPoints = [\"src/main.py\"]\n");
        s
    };

    fs::write(&target, new_contents).with_context(|| format!("writing {}", target.display()))?;
    println!("Updated {}", target.display());
    Ok(())
}

fn replace_tool_pyllow(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut in_section = false;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            in_section = is_tool_pyllow_header(trimmed);
            if !in_section {
                out.push_str(line);
                out.push('\n');
            }
            continue;
        }
        if !in_section {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("\n[tool.pyllow]\n");
    out.push_str("# Roots that contain importable Python packages (auto-detected if omitted).\n");
    out.push_str("# packageRoots = [\"src\"]\n");
    out.push_str("# entryPoints = [\"src/main.py\"]\n");
    out
}

fn has_tool_pyllow_section(source: &str) -> bool {
    source
        .lines()
        .map(str::trim_start)
        .any(is_tool_pyllow_header)
}

/// True for `[tool.pyllow]`, `[tool.pyllow.plugins.fastapi]`, and the
/// `[[tool.pyllow.x]]` array-of-tables form. Comparing against the
/// literal `[tool.pyllow]` left every nested subtable in place during a
/// `--force` replacement, so stale plugin settings kept biasing config
/// loads after the user explicitly asked for a clean slate.
fn is_tool_pyllow_header(trimmed: &str) -> bool {
    let head = match trimmed.split('#').next() {
        Some(s) => s.trim_end(),
        None => return false,
    };
    let inner = head
        .strip_prefix("[[")
        .and_then(|s| s.strip_suffix("]]"))
        .or_else(|| head.strip_prefix('[').and_then(|s| s.strip_suffix(']')));
    match inner {
        Some(key) => {
            let key = key.trim();
            key == "tool.pyllow" || key.starts_with("tool.pyllow.")
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn creates_pyllow_toml_and_pyllowignore() {
        let tmp = tempdir().unwrap();
        run(tmp.path().to_path_buf(), false, false).unwrap();
        assert!(tmp.path().join("pyllow.toml").exists());
        assert!(tmp.path().join(".pyllowignore").exists());
    }

    #[test]
    fn refuses_to_overwrite_without_force() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("pyllow.toml"), "existing").unwrap();
        let err = run(tmp.path().to_path_buf(), false, false).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn force_overwrites() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("pyllow.toml"), "old").unwrap();
        run(tmp.path().to_path_buf(), false, true).unwrap();
        let contents = fs::read_to_string(tmp.path().join("pyllow.toml")).unwrap();
        assert!(contents.contains("pyllow.toml"));
    }

    #[test]
    fn appends_to_pyproject() {
        let tmp = tempdir().unwrap();
        fs::write(
            tmp.path().join("pyproject.toml"),
            "[project]\nname = \"x\"\n",
        )
        .unwrap();
        run(tmp.path().to_path_buf(), true, false).unwrap();
        let contents = fs::read_to_string(tmp.path().join("pyproject.toml")).unwrap();
        assert!(contents.contains("[project]"));
        assert!(contents.contains("[tool.pyllow]"));
    }

    #[test]
    fn pyproject_refuses_existing_tool_pyllow() {
        let tmp = tempdir().unwrap();
        fs::write(
            tmp.path().join("pyproject.toml"),
            "[tool.pyllow]\nentryPoints = []\n",
        )
        .unwrap();
        let err = run(tmp.path().to_path_buf(), true, false).unwrap_err();
        assert!(err.to_string().contains("[tool.pyllow]"));
    }

    #[test]
    fn pyproject_force_replaces_section() {
        let tmp = tempdir().unwrap();
        fs::write(
            tmp.path().join("pyproject.toml"),
            "[project]\nname = \"x\"\n\n[tool.pyllow]\nentryPoints = [\"old.py\"]\n",
        )
        .unwrap();
        run(tmp.path().to_path_buf(), true, true).unwrap();
        let contents = fs::read_to_string(tmp.path().join("pyproject.toml")).unwrap();
        assert!(contents.contains("[project]"));
        assert!(contents.contains("[tool.pyllow]"));
        assert!(!contents.contains("old.py"));
    }

    #[test]
    fn pyproject_force_drops_tool_pyllow_subtables() {
        let tmp = tempdir().unwrap();
        fs::write(
            tmp.path().join("pyproject.toml"),
            "[project]\nname = \"x\"\n\n[tool.pyllow]\nentryPoints = [\"old.py\"]\n\n[tool.pyllow.plugins.fastapi]\nenabled = false\n\n[tool.other]\nkeep = true\n",
        )
        .unwrap();
        run(tmp.path().to_path_buf(), true, true).unwrap();
        let contents = fs::read_to_string(tmp.path().join("pyproject.toml")).unwrap();
        assert!(contents.contains("[tool.other]"));
        assert!(contents.contains("keep = true"));
        assert!(!contents.contains("[tool.pyllow.plugins.fastapi]"));
        assert!(!contents.contains("enabled = false"));
        assert!(!contents.contains("old.py"));
    }

    #[test]
    fn pyproject_refuses_existing_tool_pyllow_subtable() {
        let tmp = tempdir().unwrap();
        fs::write(
            tmp.path().join("pyproject.toml"),
            "[tool.pyllow.plugins.fastapi]\nenabled = false\n",
        )
        .unwrap();
        let err = run(tmp.path().to_path_buf(), true, false).unwrap_err();
        assert!(err.to_string().contains("[tool.pyllow]"));
    }
}
