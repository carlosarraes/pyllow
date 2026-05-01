use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("io error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("toml parse error in {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ResolvedConfig {
    pub project_root: PathBuf,
    pub package_roots: Vec<PathBuf>,
    pub ignore_patterns: Vec<String>,
    pub entry_points: Vec<PathBuf>,
    pub python_version: String,
    pub plugins: BTreeMap<String, PluginConfig>,
    pub smells_disabled: Vec<String>,
    pub smells_todo_density_threshold: Option<u32>,
    /// Extra terminal name segments treated as money-shaped by the
    /// `money-as-float` smell rule (added to the built-in defaults).
    pub smells_money_extra_patterns: Vec<String>,
}

impl Default for ResolvedConfig {
    fn default() -> Self {
        Self {
            project_root: PathBuf::from("."),
            package_roots: vec![],
            ignore_patterns: default_ignore_patterns(),
            entry_points: vec![],
            python_version: "3.11".to_string(),
            plugins: default_plugins(),
            smells_disabled: vec![],
            smells_todo_density_threshold: None,
            smells_money_extra_patterns: vec![],
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PluginConfig {
    pub enabled: bool,
}

fn default_ignore_patterns() -> Vec<String> {
    vec![
        "**/.venv/**".into(),
        "**/venv/**".into(),
        "**/.env/**".into(),
        "**/__pycache__/**".into(),
        "**/.tox/**".into(),
        "**/.nox/**".into(),
        "**/build/**".into(),
        "**/dist/**".into(),
        "**/.pytest_cache/**".into(),
        "**/.mypy_cache/**".into(),
        "**/.ruff_cache/**".into(),
        "**/site-packages/**".into(),
        "**/.git/**".into(),
        "**/.github/**".into(),
        "**/.gitlab/**".into(),
        "**/.circleci/**".into(),
        "**/node_modules/**".into(),
    ]
}

fn default_plugins() -> BTreeMap<String, PluginConfig> {
    let mut plugins = BTreeMap::new();
    plugins.insert("fastapi".into(), PluginConfig { enabled: true });
    plugins.insert("fastmcp".into(), PluginConfig { enabled: true });
    plugins.insert("pytest".into(), PluginConfig { enabled: true });
    plugins.insert("prefect".into(), PluginConfig { enabled: true });
    plugins.insert("script".into(), PluginConfig { enabled: true });
    plugins.insert("click".into(), PluginConfig { enabled: true });
    plugins.insert("pydantic".into(), PluginConfig { enabled: true });
    plugins.insert("sqlalchemy".into(), PluginConfig { enabled: true });
    plugins.insert("django".into(), PluginConfig { enabled: true });
    plugins.insert("celery".into(), PluginConfig { enabled: true });
    plugins.insert("sqlmodel".into(), PluginConfig { enabled: true });
    plugins.insert("marshmallow".into(), PluginConfig { enabled: true });
    plugins.insert("starlette".into(), PluginConfig { enabled: true });
    plugins.insert("aiohttp".into(), PluginConfig { enabled: true });
    plugins.insert("flask".into(), PluginConfig { enabled: true });
    plugins.insert("beanie".into(), PluginConfig { enabled: true });
    plugins.insert("alembic".into(), PluginConfig { enabled: true });
    plugins
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct PyllowFile {
    package_roots: Option<Vec<PathBuf>>,
    ignore_patterns: Option<Vec<String>>,
    entry_points: Option<Vec<PathBuf>>,
    python_version: Option<String>,
    plugins: Option<BTreeMap<String, PluginConfig>>,
    smells: Option<SmellsConfig>,
}

// `[smells]` is the one nested config block where snake_case keys are
// documented (matching ruff/pyflakes conventions for rule names like
// `high-todo-density`), but the rest of pyllow.toml uses camelCase to
// match the top-level `[project]` style. Accept both spellings via
// `#[serde(alias)]` so historical configs that wrote `todoDensityThreshold`
// and `[smells.moneyAsFloat]` keep working — silently ignoring those
// would change a user's smell thresholds without warning.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SmellsConfig {
    disabled: Vec<String>,
    #[serde(alias = "todoDensityThreshold")]
    todo_density_threshold: Option<u32>,
    #[serde(alias = "moneyAsFloat")]
    money_as_float: Option<MoneyAsFloatConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct MoneyAsFloatConfig {
    #[serde(alias = "extraNamePatterns")]
    extra_name_patterns: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct PyProjectFile {
    tool: Option<ToolTable>,
}

#[derive(Debug, Default, Deserialize)]
struct ToolTable {
    pyllow: Option<PyllowFile>,
}

impl ResolvedConfig {
    pub fn load(project_root: &Path) -> Result<Self, ConfigError> {
        let mut cfg = Self {
            project_root: project_root.to_path_buf(),
            ..Self::default()
        };

        if let Some(parsed) = read_toml::<PyllowFile>(&project_root.join("pyllow.toml"))? {
            cfg.merge(parsed);
        } else if let Some(parsed) =
            read_toml::<PyProjectFile>(&project_root.join("pyproject.toml"))?
        {
            if let Some(section) = parsed.tool.and_then(|t| t.pyllow) {
                cfg.merge(section);
            }
        }

        cfg.merge_pyllowignore(&project_root.join(".pyllowignore"))?;
        Ok(cfg)
    }

    fn merge_pyllowignore(&mut self, path: &Path) -> Result<(), ConfigError> {
        match fs::read_to_string(path) {
            Ok(raw) => {
                for line in raw.lines() {
                    let trimmed = line.trim();
                    if trimmed.is_empty() || trimmed.starts_with('#') {
                        continue;
                    }
                    self.ignore_patterns.push(trimmed.to_string());
                }
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(ConfigError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }
}

fn read_toml<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, ConfigError> {
    match fs::read_to_string(path) {
        Ok(raw) => toml::from_str(&raw)
            .map(Some)
            .map_err(|source| ConfigError::Toml {
                path: path.to_path_buf(),
                source,
            }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

impl ResolvedConfig {
    fn merge(&mut self, file: PyllowFile) {
        if let Some(v) = file.package_roots {
            self.package_roots = v;
        }
        if let Some(v) = file.ignore_patterns {
            self.ignore_patterns.extend(v);
        }
        if let Some(v) = file.entry_points {
            self.entry_points = v;
        }
        if let Some(v) = file.python_version {
            self.python_version = v;
        }
        if let Some(v) = file.plugins {
            for (k, plugin_cfg) in v {
                self.plugins.insert(k, plugin_cfg);
            }
        }
        if let Some(s) = file.smells {
            self.smells_disabled = s.disabled;
            self.smells_todo_density_threshold = s.todo_density_threshold;
            if let Some(m) = s.money_as_float {
                self.smells_money_extra_patterns = m.extra_name_patterns;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn loads_defaults_when_no_config() {
        let dir = tempdir().unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        assert_eq!(cfg.python_version, "3.11");
        assert!(cfg.plugins.contains_key("fastapi"));
        assert!(cfg.ignore_patterns.iter().any(|p| p.contains(".venv")));
    }

    #[test]
    fn loads_pyllow_toml() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyllow.toml"),
            "packageRoots = [\"src/app\"]\npythonVersion = \"3.12\"\n[plugins.fastapi]\nenabled = false",
        )
        .unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        assert_eq!(cfg.package_roots, vec![PathBuf::from("src/app")]);
        assert_eq!(cfg.python_version, "3.12");
        assert!(!cfg.plugins["fastapi"].enabled);
    }

    #[test]
    fn loads_tool_pyllow_from_pyproject() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.pyllow]\npackageRoots = [\"app\"]\nentryPoints = [\"app/main.py\"]",
        )
        .unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        assert_eq!(cfg.package_roots, vec![PathBuf::from("app")]);
        assert_eq!(cfg.entry_points, vec![PathBuf::from("app/main.py")]);
    }

    #[test]
    fn pyllow_toml_takes_precedence_over_pyproject() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("pyllow.toml"), "pythonVersion = \"3.13\"").unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.pyllow]\npythonVersion = \"3.10\"",
        )
        .unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        assert_eq!(cfg.python_version, "3.13");
    }

    #[test]
    fn appends_pyllowignore_patterns_to_ignore_list() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join(".pyllowignore"),
            "# pyllow ignore\nscripts/**\n\ntests/**\n  docs/**  \n",
        )
        .unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        assert!(cfg.ignore_patterns.contains(&"scripts/**".to_string()));
        assert!(cfg.ignore_patterns.contains(&"tests/**".to_string()));
        assert!(cfg.ignore_patterns.contains(&"docs/**".to_string()));
        assert!(cfg.ignore_patterns.iter().any(|p| p.contains(".venv")));
    }

    #[test]
    fn smells_section_accepts_snake_case_keys() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyllow.toml"),
            "[smells]\ntodo_density_threshold = 9\n\n[smells.money_as_float]\nextra_name_patterns = [\"premium\"]\n",
        )
        .unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        assert_eq!(cfg.smells_todo_density_threshold, Some(9));
        assert_eq!(cfg.smells_money_extra_patterns, vec!["premium".to_string()]);
    }

    #[test]
    fn smells_section_accepts_camel_case_keys_for_compat() {
        // Historical configs used camelCase to match the rest of
        // pyllow.toml. After switching to snake_case the new spelling is
        // canonical, but silently ignoring the old form would change a
        // user's smell thresholds without warning. Accept both via
        // serde aliases.
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyllow.toml"),
            "[smells]\ntodoDensityThreshold = 7\n\n[smells.moneyAsFloat]\nextraNamePatterns = [\"legacy\"]\n",
        )
        .unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        assert_eq!(cfg.smells_todo_density_threshold, Some(7));
        assert_eq!(cfg.smells_money_extra_patterns, vec!["legacy".to_string()]);
    }

    #[test]
    fn pyllowignore_combines_with_pyllow_toml() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyllow.toml"),
            "ignorePatterns = [\"build/**\"]",
        )
        .unwrap();
        fs::write(dir.path().join(".pyllowignore"), "vendor/**\n").unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        assert!(cfg.ignore_patterns.contains(&"build/**".to_string()));
        assert!(cfg.ignore_patterns.contains(&"vendor/**".to_string()));
    }
}
