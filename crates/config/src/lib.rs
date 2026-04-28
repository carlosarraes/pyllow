use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
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
        "**/node_modules/**".into(),
    ]
}

fn default_plugins() -> BTreeMap<String, PluginConfig> {
    let mut plugins = BTreeMap::new();
    plugins.insert("fastapi".into(), PluginConfig { enabled: true });
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

        let standalone = project_root.join("pyllow.toml");
        if standalone.exists() {
            let raw = fs::read_to_string(&standalone).map_err(|e| ConfigError::Io {
                path: standalone.clone(),
                source: e,
            })?;
            let parsed: PyllowFile = toml::from_str(&raw).map_err(|e| ConfigError::Toml {
                path: standalone,
                source: e,
            })?;
            cfg.merge(parsed);
            return Ok(cfg);
        }

        let pyproject = project_root.join("pyproject.toml");
        if pyproject.exists() {
            let raw = fs::read_to_string(&pyproject).map_err(|e| ConfigError::Io {
                path: pyproject.clone(),
                source: e,
            })?;
            let parsed: PyProjectFile = toml::from_str(&raw).map_err(|e| ConfigError::Toml {
                path: pyproject,
                source: e,
            })?;
            if let Some(section) = parsed.tool.and_then(|t| t.pyllow) {
                cfg.merge(section);
            }
        }

        Ok(cfg)
    }

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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmpdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pyllow-cfg-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn loads_defaults_when_no_config() {
        let dir = tmpdir("loads_defaults_when_no_config");
        let cfg = ResolvedConfig::load(&dir).unwrap();
        assert_eq!(cfg.python_version, "3.11");
        assert!(cfg.plugins.contains_key("fastapi"));
        assert!(cfg.ignore_patterns.iter().any(|p| p.contains(".venv")));
    }

    #[test]
    fn loads_pyllow_toml() {
        let dir = tmpdir("loads_pyllow_toml");
        let mut f = fs::File::create(dir.join("pyllow.toml")).unwrap();
        writeln!(
            f,
            "packageRoots = [\"src/app\"]\npythonVersion = \"3.12\"\n[plugins.fastapi]\nenabled = false"
        )
        .unwrap();
        let cfg = ResolvedConfig::load(&dir).unwrap();
        assert_eq!(cfg.package_roots, vec![PathBuf::from("src/app")]);
        assert_eq!(cfg.python_version, "3.12");
        assert!(!cfg.plugins["fastapi"].enabled);
    }

    #[test]
    fn loads_tool_pyllow_from_pyproject() {
        let dir = tmpdir("loads_tool_pyllow_from_pyproject");
        let mut f = fs::File::create(dir.join("pyproject.toml")).unwrap();
        writeln!(
            f,
            "[tool.pyllow]\npackageRoots = [\"app\"]\nentryPoints = [\"app/main.py\"]"
        )
        .unwrap();
        let cfg = ResolvedConfig::load(&dir).unwrap();
        assert_eq!(cfg.package_roots, vec![PathBuf::from("app")]);
        assert_eq!(cfg.entry_points, vec![PathBuf::from("app/main.py")]);
    }

    #[test]
    fn pyllow_toml_takes_precedence_over_pyproject() {
        let dir = tmpdir("pyllow_toml_takes_precedence_over_pyproject");
        fs::write(dir.join("pyllow.toml"), "pythonVersion = \"3.13\"").unwrap();
        fs::write(
            dir.join("pyproject.toml"),
            "[tool.pyllow]\npythonVersion = \"3.10\"",
        )
        .unwrap();
        let cfg = ResolvedConfig::load(&dir).unwrap();
        assert_eq!(cfg.python_version, "3.13");
    }
}
