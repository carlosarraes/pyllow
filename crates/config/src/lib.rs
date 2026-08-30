use pyllow_types::SmellRule;
use rustc_hash::FxHashSet;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("io error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("unknown smell rule `{name}` in [smells].{field} (valid rules: {valid})")]
    UnknownSmellRule {
        name: String,
        field: &'static str,
        valid: String,
    },
    #[error("toml parse error in {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid glob in [[suppress]] entry path={pattern:?}: {source}")]
    InvalidSuppressGlob {
        pattern: String,
        #[source]
        source: globset::Error,
    },
    #[error("invalid glob in [[boundaries.zones]] {context}={pattern:?}: {source}")]
    InvalidBoundaryGlob {
        context: &'static str,
        pattern: String,
        #[source]
        source: globset::Error,
    },
    #[error("[[boundaries.rules]] with from={from:?} sets both allow and deny — pick one")]
    ConflictingBoundaryRule { from: String },
}

/// Compiled `[[suppress]]` entry. Filters issues at the `path` glob whose
/// `rule_key()` is in `rules` (or any rule when `rules` is empty) and whose
/// line matches `line` (or any line when `line` is None).
#[derive(Debug, Clone)]
pub struct SuppressEntry {
    pub path_glob: globset::GlobMatcher,
    pub rules: FxHashSet<String>,
    pub line: Option<u32>,
    pub reason: Option<String>,
}

/// Compiled architecture-boundary zone. A file belongs to a zone when its
/// project-root-relative path matches at least one of `patterns`.
#[derive(Debug, Clone)]
pub struct ResolvedZone {
    pub name: String,
    pub patterns: Vec<globset::GlobMatcher>,
}

/// Compiled boundary rule. `from` is a glob over zone names; an edge from a
/// matching zone is allowed if `to_zone` matches at least one `allow` entry
/// (whitelist mode), denied if it matches any `deny` entry (blacklist mode).
/// Exactly one of `allow`/`deny` is populated — mixing them is rejected at
/// load time as a `ConflictingBoundaryRule`.
#[derive(Debug, Clone)]
pub struct ResolvedRule {
    pub from: globset::GlobMatcher,
    pub allow: Vec<globset::GlobMatcher>,
    pub deny: Vec<globset::GlobMatcher>,
}

#[derive(Debug, Clone, Default)]
pub struct BoundaryConfig {
    pub zones: Vec<ResolvedZone>,
    pub rules: Vec<ResolvedRule>,
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
    pub smells_enabled: Vec<SmellRule>,
    pub smells_disabled: Vec<SmellRule>,
    pub smells_todo_density_threshold: Option<u32>,
    /// Extra terminal name segments treated as money-shaped by the
    /// `money-as-float` smell rule (added to the built-in defaults).
    pub smells_money_extra_patterns: Vec<String>,
    /// Minimum distinct files a clone family must span to be reported by
    /// `pyllow dupes`. Default 2 (any cross-file clone). CLI `--min-occurrences`
    /// overrides this when set.
    pub dupes_min_occurrences: usize,
    /// Pyllow-native `[[suppress]]` entries that drop issues matching path,
    /// rule, and (optionally) line. Applied between noqa filtering and
    /// baseline filtering in the postprocess pipeline.
    #[serde(skip)]
    pub suppress: Vec<SuppressEntry>,
    /// Architecture-boundary zones + rules. Empty by default (no enforcement).
    #[serde(skip)]
    pub boundaries: BoundaryConfig,
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
            smells_enabled: vec![],
            smells_disabled: vec![],
            smells_todo_density_threshold: None,
            smells_money_extra_patterns: vec![],
            dupes_min_occurrences: 2,
            suppress: vec![],
            boundaries: BoundaryConfig::default(),
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
    dupes: Option<DupesConfig>,
    #[serde(default)]
    suppress: Vec<SuppressEntryFile>,
    boundaries: Option<BoundariesFile>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct BoundariesFile {
    preset: Option<BoundaryPreset>,
    zones: Vec<ZoneFile>,
    rules: Vec<RuleFile>,
}

/// Curated architecture presets. Each expands to a canned set of zones and
/// rules that get prepended to any user-declared `[[boundaries.zones]]`/
/// `[[boundaries.rules]]` so users can extend (not override) the preset.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum BoundaryPreset {
    /// Mutually-isolated feature modules under `src/features/<name>` with a
    /// shared library zone for cross-feature utilities.
    Bulletproof,
    /// Classic 3-tier: presentation → business → data. Lower tiers must not
    /// depend on higher tiers.
    Layered,
    /// Ports & adapters: domain at the core, application orchestrates,
    /// adapters reach outward. Domain must not import adapters or
    /// application.
    Hexagonal,
    /// Feature-Sliced Design: entities < features < widgets < pages.
    /// Each layer may only import from layers below it.
    FeatureSliced,
}

/// Kebab-case names of every supported `[boundaries] preset = "..."` value.
/// Exposed so the CLI (`pyllow init --boundaries`) can validate user input
/// without duplicating the list.
pub const KNOWN_BOUNDARY_PRESETS: &[&str] =
    &["bulletproof", "layered", "hexagonal", "feature-sliced"];

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ZoneFile {
    name: String,
    patterns: Vec<String>,
    /// Each entry is a parent directory; for every immediate child directory
    /// containing at least one `.py` file, an additional zone is generated
    /// with name `"{name}/{child}"` and pattern `"{dir}/{child}/**"`.
    #[serde(alias = "autoDiscover")]
    auto_discover: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RuleFile {
    from: String,
    allow: Vec<String>,
    deny: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SuppressEntryFile {
    path: String,
    rules: Vec<String>,
    line: Option<u32>,
    reason: Option<String>,
}

// `[smells]` keys are snake_case (matching ruff/pyflakes rule names);
// camelCase aliases preserve historical configs.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SmellsConfig {
    /// Opt-in list for rules that ship disabled. Unknown names are rejected.
    enabled: Vec<String>,
    disabled: Vec<String>,
    #[serde(alias = "todoDensityThreshold")]
    todo_density_threshold: Option<u32>,
    #[serde(alias = "moneyAsFloat")]
    money_as_float: Option<MoneyAsFloatConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DupesConfig {
    #[serde(alias = "minOccurrences")]
    min_occurrences: Option<usize>,
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
            cfg.merge(parsed)?;
        } else if let Some(parsed) =
            read_toml::<PyProjectFile>(&project_root.join("pyproject.toml"))?
        {
            if let Some(section) = parsed.tool.and_then(|t| t.pyllow) {
                cfg.merge(section)?;
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
    fn merge(&mut self, file: PyllowFile) -> Result<(), ConfigError> {
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
            self.smells_enabled = parse_smell_rules(&s.enabled, "enabled")?;
            self.smells_disabled = parse_smell_rules(&s.disabled, "disabled")?;
            self.smells_todo_density_threshold = s.todo_density_threshold;
            if let Some(m) = s.money_as_float {
                self.smells_money_extra_patterns = m.extra_name_patterns;
            }
        }
        if let Some(d) = file.dupes {
            if let Some(n) = d.min_occurrences {
                self.dupes_min_occurrences = n;
            }
        }
        for raw in file.suppress {
            let pattern = raw.path.clone();
            let glob = globset::Glob::new(&pattern).map_err(|source| {
                ConfigError::InvalidSuppressGlob {
                    pattern: pattern.clone(),
                    source,
                }
            })?;
            self.suppress.push(SuppressEntry {
                path_glob: glob.compile_matcher(),
                rules: raw.rules.into_iter().collect(),
                line: raw.line,
                reason: raw.reason,
            });
        }
        if let Some(b) = file.boundaries {
            self.boundaries = resolve_boundaries(b, &self.project_root)?;
        }
        Ok(())
    }
}

/// Expand a `[boundaries] preset = "..."` into its canned zones and rules.
/// Each preset uses one canonical Python layout name per layer — users who
/// have aliases (`src/api/**`, `src/views/**`, etc.) can extend via their own
/// `[[boundaries.zones]]` entries, which the loader appends to the preset's.
fn preset_layout(preset: BoundaryPreset) -> BoundariesFile {
    fn zone(name: &str, patterns: &[&str], auto: &[&str]) -> ZoneFile {
        ZoneFile {
            name: name.into(),
            patterns: patterns.iter().map(|s| (*s).into()).collect(),
            auto_discover: auto.iter().map(|s| (*s).into()).collect(),
        }
    }
    fn deny(from: &str, deny: &[&str]) -> RuleFile {
        RuleFile {
            from: from.into(),
            allow: vec![],
            deny: deny.iter().map(|s| (*s).into()).collect(),
        }
    }
    fn allow(from: &str, allow: &[&str]) -> RuleFile {
        RuleFile {
            from: from.into(),
            allow: allow.iter().map(|s| (*s).into()).collect(),
            deny: vec![],
        }
    }
    match preset {
        BoundaryPreset::Bulletproof => BoundariesFile {
            preset: None,
            zones: vec![
                zone("features", &[], &["src/features"]),
                zone("shared", &["src/shared/**", "src/lib/**"], &[]),
            ],
            // Cross-feature imports are the canonical Bulletproof violation.
            // Shared is implicitly OK because no rule denies it.
            rules: vec![deny("features/*", &["features/*"])],
        },
        BoundaryPreset::Layered => BoundariesFile {
            preset: None,
            zones: vec![
                zone("presentation", &["src/presentation/**"], &[]),
                zone("business", &["src/business/**"], &[]),
                zone("data", &["src/data/**"], &[]),
            ],
            // Downward-only: lower tiers cannot import higher tiers.
            rules: vec![
                deny("data", &["business", "presentation"]),
                deny("business", &["presentation"]),
            ],
        },
        BoundaryPreset::Hexagonal => BoundariesFile {
            preset: None,
            zones: vec![
                zone("domain", &["src/domain/**"], &[]),
                zone("application", &["src/application/**"], &[]),
                zone(
                    "adapters",
                    &["src/adapters/**", "src/infrastructure/**"],
                    &[],
                ),
            ],
            // Domain stays pure; application orchestrates without reaching
            // back into adapters.
            rules: vec![
                deny("domain", &["application", "adapters"]),
                deny("application", &["adapters"]),
            ],
        },
        BoundaryPreset::FeatureSliced => BoundariesFile {
            preset: None,
            zones: vec![
                zone("entities", &[], &["src/entities"]),
                zone("features", &[], &["src/features"]),
                zone("widgets", &[], &["src/widgets"]),
                zone("pages", &[], &["src/pages"]),
                zone("shared", &["src/shared/**"], &[]),
            ],
            // Each layer may only import from layers below + shared.
            rules: vec![
                allow("entities/*", &["shared"]),
                allow("features/*", &["entities/*", "shared"]),
                allow("widgets/*", &["features/*", "entities/*", "shared"]),
                allow(
                    "pages/*",
                    &["widgets/*", "features/*", "entities/*", "shared"],
                ),
            ],
        },
    }
}

fn resolve_boundaries(
    file: BoundariesFile,
    project_root: &Path,
) -> Result<BoundaryConfig, ConfigError> {
    let mut combined = file;
    if let Some(preset) = combined.preset {
        let mut canned = preset_layout(preset);
        // Preset zones/rules come first; user additions append.
        canned.zones.extend(combined.zones);
        canned.rules.extend(combined.rules);
        combined.zones = canned.zones;
        combined.rules = canned.rules;
    }
    let mut zones: Vec<ResolvedZone> = Vec::new();
    for zone in combined.zones {
        let patterns = compile_globs(&zone.patterns, "patterns")?;
        if !patterns.is_empty() {
            zones.push(ResolvedZone {
                name: zone.name.clone(),
                patterns,
            });
        }
        // Expand auto_discover: each entry is a parent directory whose
        // immediate child directories become sibling zones named
        // "{parent_zone_name}/{child}" with pattern "{auto_discover_dir}/{child}/**".
        // Skip child dirs that contain no .py files — empty layout dirs
        // shouldn't create phantom zones.
        for parent_dir_str in &zone.auto_discover {
            let parent_dir = project_root.join(parent_dir_str);
            let Ok(entries) = fs::read_dir(&parent_dir) else {
                continue;
            };
            let mut children: Vec<String> = entries
                .flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .filter(|e| dir_contains_py(&e.path()))
                .filter_map(|e| e.file_name().into_string().ok())
                .collect();
            children.sort();
            for child in children {
                let pattern = format!("{parent_dir_str}/{child}/**");
                let glob = globset::Glob::new(&pattern).map_err(|source| {
                    ConfigError::InvalidBoundaryGlob {
                        context: "auto_discover",
                        pattern: pattern.clone(),
                        source,
                    }
                })?;
                zones.push(ResolvedZone {
                    name: format!("{}/{child}", zone.name),
                    patterns: vec![glob.compile_matcher()],
                });
            }
        }
    }
    let mut rules: Vec<ResolvedRule> = Vec::new();
    for r in combined.rules {
        if !r.allow.is_empty() && !r.deny.is_empty() {
            return Err(ConfigError::ConflictingBoundaryRule { from: r.from });
        }
        let from = globset::Glob::new(&r.from)
            .map_err(|source| ConfigError::InvalidBoundaryGlob {
                context: "rules.from",
                pattern: r.from.clone(),
                source,
            })?
            .compile_matcher();
        rules.push(ResolvedRule {
            from,
            allow: compile_globs(&r.allow, "rules.allow")?,
            deny: compile_globs(&r.deny, "rules.deny")?,
        });
    }
    Ok(BoundaryConfig { zones, rules })
}

fn compile_globs(
    patterns: &[String],
    context: &'static str,
) -> Result<Vec<globset::GlobMatcher>, ConfigError> {
    patterns
        .iter()
        .map(|p| {
            globset::Glob::new(p)
                .map(|g| g.compile_matcher())
                .map_err(|source| ConfigError::InvalidBoundaryGlob {
                    context,
                    pattern: p.clone(),
                    source,
                })
        })
        .collect()
}

fn dir_contains_py(dir: &Path) -> bool {
    fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().any(|e| {
                let p = e.path();
                p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("py")
            })
        })
        .unwrap_or(false)
}

/// Convert configured rule names into rule values, rejecting anything
/// unrecognised. Runs during config load so a typo fails the run outright
/// rather than silently selecting nothing.
fn parse_smell_rules(names: &[String], field: &'static str) -> Result<Vec<SmellRule>, ConfigError> {
    names
        .iter()
        .map(|name| {
            SmellRule::from_str(name).map_err(|_| ConfigError::UnknownSmellRule {
                name: name.clone(),
                field,
                valid: SmellRule::all()
                    .iter()
                    .map(|r| r.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // #1: unknown rule names must fail validation *before* analysis. Warning
    // and continuing means a typo in a policy gate silently disables nothing
    // (or, for `enabled`, silently enables nothing) and the gate passes.
    #[test]
    fn unknown_disabled_rule_name_is_rejected() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyllow.toml"),
            "[smells]\ndisabled = [\"mutable-default\", \"no-such-rule\"]\n",
        )
        .unwrap();
        let err = ResolvedConfig::load(dir.path()).expect_err("unknown rule must fail");
        assert!(
            err.to_string().contains("no-such-rule"),
            "error should name the offending rule: {err}"
        );
    }

    #[test]
    fn unknown_enabled_rule_name_is_rejected() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyllow.toml"),
            "[smells]\nenabled = [\"definitely-not-a-rule\"]\n",
        )
        .unwrap();
        let err = ResolvedConfig::load(dir.path()).expect_err("unknown rule must fail");
        assert!(err.to_string().contains("definitely-not-a-rule"), "{err}");
    }

    #[test]
    fn known_rule_names_are_accepted_in_both_lists() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyllow.toml"),
            "[smells]\nenabled = [\"stray-print\"]\ndisabled = [\"broad-except\"]\n",
        )
        .unwrap();
        let cfg = ResolvedConfig::load(dir.path()).expect("valid rule names must load");
        assert_eq!(cfg.smells_enabled, vec![SmellRule::StrayPrint]);
        assert_eq!(cfg.smells_disabled, vec![SmellRule::BroadExcept]);
    }

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
    fn dupes_default_min_occurrences_is_two() {
        let dir = tempdir().unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        assert_eq!(cfg.dupes_min_occurrences, 2);
    }

    #[test]
    fn dupes_section_accepts_snake_case_min_occurrences() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyllow.toml"),
            "[dupes]\nmin_occurrences = 3\n",
        )
        .unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        assert_eq!(cfg.dupes_min_occurrences, 3);
    }

    #[test]
    fn dupes_section_accepts_camel_case_min_occurrences() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyllow.toml"),
            "[dupes]\nminOccurrences = 4\n",
        )
        .unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        assert_eq!(cfg.dupes_min_occurrences, 4);
    }

    #[test]
    fn suppress_entry_loads_with_minimum_fields() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyllow.toml"),
            r#"
[[suppress]]
path = "src/legacy.py"
rules = ["unused-import"]
"#,
        )
        .unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        assert_eq!(cfg.suppress.len(), 1);
        let entry = &cfg.suppress[0];
        assert!(entry.path_glob.is_match("src/legacy.py"));
        assert!(!entry.path_glob.is_match("src/other.py"));
        assert!(entry.rules.contains("unused-import"));
        assert!(entry.line.is_none());
    }

    #[test]
    fn suppress_entry_with_line_and_reason() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyllow.toml"),
            r#"
[[suppress]]
path = "src/foo.py"
rules = ["broad-except"]
line = 42
reason = "intentional catch-all for backwards compat"
"#,
        )
        .unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        let entry = &cfg.suppress[0];
        assert_eq!(entry.line, Some(42));
        assert!(entry
            .reason
            .as_deref()
            .unwrap()
            .contains("backwards compat"));
    }

    #[test]
    fn suppress_glob_supports_double_star() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyllow.toml"),
            r#"
[[suppress]]
path = "tests/**/*.py"
rules = ["stray-print"]
"#,
        )
        .unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        let entry = &cfg.suppress[0];
        assert!(entry.path_glob.is_match("tests/unit/foo.py"));
        assert!(entry.path_glob.is_match("tests/integration/sub/bar.py"));
        assert!(!entry.path_glob.is_match("src/foo.py"));
    }

    #[test]
    fn invalid_suppress_glob_returns_error() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyllow.toml"),
            r#"
[[suppress]]
path = "src/["
rules = []
"#,
        )
        .unwrap();
        let err = ResolvedConfig::load(dir.path()).expect_err("invalid glob should error");
        let msg = err.to_string();
        assert!(
            msg.contains("suppress") || msg.contains("glob"),
            "error message should mention suppress/glob; got: {msg}"
        );
    }

    #[test]
    fn boundaries_zone_with_patterns_loads() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyllow.toml"),
            r#"
[[boundaries.zones]]
name = "core"
patterns = ["src/core/**"]
"#,
        )
        .unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        assert_eq!(cfg.boundaries.zones.len(), 1);
        let zone = &cfg.boundaries.zones[0];
        assert_eq!(zone.name, "core");
        assert!(zone.patterns[0].is_match("src/core/foo.py"));
        assert!(!zone.patterns[0].is_match("src/api/foo.py"));
    }

    #[test]
    fn boundaries_auto_discover_expands_child_dirs() {
        let dir = tempdir().unwrap();
        // Create a layout: src/features/{auth,billing,empty}/...
        fs::create_dir_all(dir.path().join("src/features/auth")).unwrap();
        fs::write(dir.path().join("src/features/auth/__init__.py"), "").unwrap();
        fs::create_dir_all(dir.path().join("src/features/billing")).unwrap();
        fs::write(dir.path().join("src/features/billing/api.py"), "").unwrap();
        fs::create_dir_all(dir.path().join("src/features/empty")).unwrap();
        // The "empty" dir has no .py files; auto_discover should skip it.

        fs::write(
            dir.path().join("pyllow.toml"),
            r#"
[[boundaries.zones]]
name = "features"
auto_discover = ["src/features"]
"#,
        )
        .unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        let names: Vec<&str> = cfg
            .boundaries
            .zones
            .iter()
            .map(|z| z.name.as_str())
            .collect();
        assert!(names.contains(&"features/auth"), "got names {names:?}");
        assert!(names.contains(&"features/billing"), "got names {names:?}");
        assert!(!names.contains(&"features/empty"), "got names {names:?}");

        // Match check: the synthesized pattern targets the child dir.
        let auth_zone = cfg
            .boundaries
            .zones
            .iter()
            .find(|z| z.name == "features/auth")
            .unwrap();
        assert!(auth_zone.patterns[0].is_match("src/features/auth/handlers.py"));
        // The barrel `src/features/__init__.py` is unclassified.
        assert!(!auth_zone.patterns[0].is_match("src/features/__init__.py"));
    }

    #[test]
    fn boundaries_rule_with_deny_loads() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyllow.toml"),
            r#"
[[boundaries.rules]]
from = "features/*"
deny = ["features/*"]
"#,
        )
        .unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        let rule = &cfg.boundaries.rules[0];
        assert!(rule.from.is_match("features/auth"));
        assert!(rule.deny[0].is_match("features/billing"));
        assert!(rule.allow.is_empty());
    }

    #[test]
    fn boundaries_rule_with_both_allow_and_deny_errors() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyllow.toml"),
            r#"
[[boundaries.rules]]
from = "features/*"
allow = ["shared"]
deny = ["features/*"]
"#,
        )
        .unwrap();
        let err = ResolvedConfig::load(dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("allow and deny") || msg.contains("pick one"),
            "got: {msg}"
        );
    }

    #[test]
    fn preset_bulletproof_creates_features_and_shared_zones() {
        let dir = tempdir().unwrap();
        // Need at least one .py file in a features child so auto_discover
        // doesn't drop the zone for being empty.
        std::fs::create_dir_all(dir.path().join("src/features/auth")).unwrap();
        std::fs::write(dir.path().join("src/features/auth/__init__.py"), "").unwrap();
        fs::write(
            dir.path().join("pyllow.toml"),
            r#"
[boundaries]
preset = "bulletproof"
"#,
        )
        .unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        let names: Vec<&str> = cfg
            .boundaries
            .zones
            .iter()
            .map(|z| z.name.as_str())
            .collect();
        assert!(
            names.contains(&"features/auth"),
            "bulletproof should auto-discover features/auth; got {names:?}"
        );
        assert!(
            names.contains(&"shared"),
            "bulletproof should include a shared zone; got {names:?}"
        );
        // Cross-feature deny rule should exist.
        assert!(
            cfg.boundaries
                .rules
                .iter()
                .any(|r| r.from.is_match("features/auth") && !r.deny.is_empty()),
            "bulletproof should have a features/* deny rule"
        );
    }

    #[test]
    fn preset_layered_creates_downward_only_rules() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyllow.toml"),
            r#"
[boundaries]
preset = "layered"
"#,
        )
        .unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        let names: Vec<&str> = cfg
            .boundaries
            .zones
            .iter()
            .map(|z| z.name.as_str())
            .collect();
        assert!(names.contains(&"presentation"), "got {names:?}");
        assert!(names.contains(&"business"), "got {names:?}");
        assert!(names.contains(&"data"), "got {names:?}");
        // data must not be allowed to import business or presentation
        let data_rule = cfg
            .boundaries
            .rules
            .iter()
            .find(|r| r.from.is_match("data"))
            .expect("layered should have a rule keyed on data");
        assert!(data_rule.deny.iter().any(|g| g.is_match("business")));
        assert!(data_rule.deny.iter().any(|g| g.is_match("presentation")));
    }

    #[test]
    fn preset_hexagonal_protects_domain_from_adapters() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyllow.toml"),
            r#"
[boundaries]
preset = "hexagonal"
"#,
        )
        .unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        let domain_rule = cfg
            .boundaries
            .rules
            .iter()
            .find(|r| r.from.is_match("domain"))
            .expect("hexagonal should have a rule keyed on domain");
        // domain must not import adapters
        assert!(domain_rule.deny.iter().any(|g| g.is_match("adapters")));
    }

    #[test]
    fn preset_feature_sliced_creates_layered_features() {
        let dir = tempdir().unwrap();
        // Auto_discover needs at least one .py per child so a zone is created.
        for layer in ["entities", "features", "widgets", "pages"] {
            let path = dir.path().join("src").join(layer).join("sample");
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("__init__.py"), "").unwrap();
        }
        fs::write(
            dir.path().join("pyllow.toml"),
            r#"
[boundaries]
preset = "feature-sliced"
"#,
        )
        .unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        let names: Vec<&str> = cfg
            .boundaries
            .zones
            .iter()
            .map(|z| z.name.as_str())
            .collect();
        for layer in [
            "entities/sample",
            "features/sample",
            "widgets/sample",
            "pages/sample",
        ] {
            assert!(
                names.contains(&layer),
                "feature-sliced should auto-discover {layer}; got {names:?}"
            );
        }
    }

    #[test]
    fn user_zones_and_rules_add_to_preset() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyllow.toml"),
            r#"
[boundaries]
preset = "layered"

[[boundaries.zones]]
name = "vendored"
patterns = ["vendored/**"]

[[boundaries.rules]]
from = "presentation"
deny = ["vendored"]
"#,
        )
        .unwrap();
        let cfg = ResolvedConfig::load(dir.path()).unwrap();
        // Layered preset adds 3 zones; user added 1 → at least 4 total.
        let names: Vec<&str> = cfg
            .boundaries
            .zones
            .iter()
            .map(|z| z.name.as_str())
            .collect();
        assert!(names.contains(&"vendored"));
        assert!(names.contains(&"presentation"));
        // Both the layered "presentation→business denied" rule AND the user's
        // "presentation→vendored denied" rule must be present.
        let presentation_rules: Vec<_> = cfg
            .boundaries
            .rules
            .iter()
            .filter(|r| r.from.is_match("presentation"))
            .collect();
        assert!(
            !presentation_rules.is_empty(),
            "user rule on `presentation` must coexist with preset rules"
        );
        // Specifically, the user-added rule (deny vendored) exists.
        assert!(presentation_rules
            .iter()
            .any(|r| r.deny.iter().any(|g| g.is_match("vendored"))));
    }

    #[test]
    fn unknown_preset_returns_error() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyllow.toml"),
            r#"
[boundaries]
preset = "made-up-thing"
"#,
        )
        .unwrap();
        ResolvedConfig::load(dir.path()).expect_err("unknown preset should error");
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
