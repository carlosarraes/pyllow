use ignore::WalkBuilder;
use pyllow_config::ResolvedConfig;
use pyllow_extract::{parse_file, ParsedModule};
use pyllow_graph::{dotted_module_for, FileRegistry, ModuleGraph, ModuleResolver};
use pyllow_types::{
    AnalysisResults, AnalysisStats, EntryPoint, EntryPointSource, FileId, ImportKind, Inventory,
    InventoryEntryPoint, InventoryFile, Issue, PluginResult,
};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub mod baseline;
pub mod circular;
mod deps;
pub mod dupes;
pub mod flags;
pub mod health;
pub mod ownership;
pub mod score;
pub mod smells;
pub mod snapshot;
pub mod suppressions;
mod walker;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AnalyzerError {
    #[error("config error: {0}")]
    Config(#[from] pyllow_config::ConfigError),
    #[error("parse error: {0}")]
    Extract(#[from] pyllow_extract::ExtractError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

type PluginDiscover = fn(&FxHashMap<FileId, ParsedModule>) -> PluginResult;

const PLUGINS: &[(&str, PluginDiscover)] = &[
    (
        pyllow_plugin_script::PLUGIN_NAME,
        pyllow_plugin_script::discover,
    ),
    (
        pyllow_plugin_fastapi::PLUGIN_NAME,
        pyllow_plugin_fastapi::discover,
    ),
    (
        pyllow_plugin_fastmcp::PLUGIN_NAME,
        pyllow_plugin_fastmcp::discover,
    ),
    (
        pyllow_plugin_pytest::PLUGIN_NAME,
        pyllow_plugin_pytest::discover,
    ),
    (
        pyllow_plugin_prefect::PLUGIN_NAME,
        pyllow_plugin_prefect::discover,
    ),
    (
        pyllow_plugin_click::PLUGIN_NAME,
        pyllow_plugin_click::discover,
    ),
    (
        pyllow_plugin_pydantic::PLUGIN_NAME,
        pyllow_plugin_pydantic::discover,
    ),
    (
        pyllow_plugin_sqlalchemy::PLUGIN_NAME,
        pyllow_plugin_sqlalchemy::discover,
    ),
    (
        pyllow_plugin_django::PLUGIN_NAME,
        pyllow_plugin_django::discover,
    ),
    (
        pyllow_plugin_celery::PLUGIN_NAME,
        pyllow_plugin_celery::discover,
    ),
    (
        pyllow_plugin_beanie::PLUGIN_NAME,
        pyllow_plugin_beanie::discover,
    ),
    (
        pyllow_plugin_alembic::PLUGIN_NAME,
        pyllow_plugin_alembic::discover,
    ),
];

fn run_enabled_plugins(
    config: &ResolvedConfig,
    parsed: &FxHashMap<FileId, ParsedModule>,
    entries: &mut Vec<EntryPoint>,
    plugins_run: &mut Vec<String>,
) {
    let results: Vec<PluginResult> = PLUGINS
        .par_iter()
        .filter(|(name, _)| {
            config.plugins.get(*name).map(|c| c.enabled).unwrap_or(false)
        })
        .map(|(_, discover)| discover(parsed))
        .collect();
    for result in &results {
        merge_plugin_result(result, entries);
    }
    plugins_run.extend(results.into_iter().map(|r| r.plugin_name));
}

/// Seed the entry-point list from every static source pyllow knows about:
/// `pyllow.toml` `entryPoints`, PEP 562 module-level `__getattr__`, and
/// `pyproject.toml` scripts/entry-points groups. Used by both
/// `analyze_with_parsed` and `collect_inventory` so they stay in lockstep.
fn seed_static_entries(
    config: &ResolvedConfig,
    project_root: &Path,
    registry: &FileRegistry,
    resolver: &ModuleResolver<'_>,
    parsed: &FxHashMap<FileId, ParsedModule>,
    pyproject_entries: Vec<deps::PyprojectEntry>,
) -> Vec<EntryPoint> {
    let mut entries = Vec::new();

    for ep_path in &config.entry_points {
        let abs = if ep_path.is_absolute() {
            ep_path.clone()
        } else {
            project_root.join(ep_path)
        };
        if let Some(id) = registry.id_for(&abs) {
            entries.push(EntryPoint {
                file: id,
                source: EntryPointSource::Config,
            });
        }
    }

    // PEP 562: top-level `__getattr__` (e.g. pydantic v1's
    // `getattr_migration` shim) is a deliberate dynamic-attribute surface.
    // External importers hit it via `from pkg.mod import attr` triggering
    // `__getattr__`; pyllow can't see those callers, so the module is live.
    for (id, module) in parsed {
        if module.has_module_getattr {
            entries.push(EntryPoint {
                file: *id,
                source: EntryPointSource::ModuleGetattr,
            });
        }
    }

    // `[project.scripts]`, `[project.gui-scripts]`, and
    // `[project.entry-points."<group>"]` declare modules consumed by
    // external tooling (pip console_scripts, mypy plugins, hypothesis
    // plugins, etc.) that aren't visible to static analysis.
    for entry in pyproject_entries {
        if let Some(id) = resolver.resolve_dotted(&entry.module) {
            entries.push(EntryPoint {
                file: id,
                source: EntryPointSource::PyprojectEntryPoint(entry.group),
            });
        }
    }

    entries
}

/// Parse a list of files in parallel and return them keyed by synthetic
/// `FileId`s (assigned by enumeration order). Suitable for CLI commands
/// like `health`, `flags`, and `smells` that don't need a `FileRegistry`
/// because their analyses only consume the parsed AST map.
pub fn parse_files_into_map(files: &[PathBuf]) -> FxHashMap<FileId, ParsedModule> {
    files
        .par_iter()
        .filter_map(|p| parse_file(p).ok())
        .collect::<Vec<_>>()
        .into_iter()
        .enumerate()
        .map(|(i, m)| (FileId(i as u32), m))
        .collect()
}

/// Parse files in parallel and register each one in a `FileRegistry`.
/// Used by the analyzer pipeline (which needs real file→FileId lookups
/// for graph traversal) — distinct from `parse_files_into_map`, which is
/// for CLI commands that only consume ASTs.
fn parse_and_register(
    files: &[PathBuf],
    package_roots: &[PathBuf],
    warn_on_error: bool,
) -> (FileRegistry, FxHashMap<FileId, ParsedModule>) {
    let parsed_per_path: Vec<(PathBuf, ParsedModule)> = files
        .par_iter()
        .filter_map(|path| match parse_file(path) {
            Ok(m) => Some((path.clone(), m)),
            Err(e) => {
                if warn_on_error {
                    eprintln!("warning: skipping {}: {}", path.display(), e);
                }
                None
            }
        })
        .collect();

    let mut registry = FileRegistry::default();
    let mut parsed: FxHashMap<FileId, ParsedModule> = FxHashMap::default();
    for (path, module) in parsed_per_path {
        let dotted = dotted_module_for(&path, package_roots).unwrap_or_default();
        let id = registry.register(path, dotted);
        parsed.insert(id, module);
    }
    (registry, parsed)
}

/// Run dead-code analysis and return only the result. Callers that also need
/// the parsed AST (audit pipeline → health, smells, dupes) should use
/// [`analyze_with_parsed`] to avoid re-parsing every file.
pub fn analyze(config: &ResolvedConfig) -> Result<AnalysisResults, AnalyzerError> {
    Ok(analyze_with_parsed(config)?.0)
}

/// Same as [`analyze`] but also returns the parsed module map so a follow-up
/// pass (e.g. audit's combined check + dupes + health + smells run) can
/// reuse the AST without a second `parse_file` per file.
pub fn analyze_with_parsed(
    config: &ResolvedConfig,
) -> Result<(AnalysisResults, FxHashMap<FileId, ParsedModule>), AnalyzerError> {
    let started = Instant::now();
    let project_root = &config.project_root;
    let package_roots = resolve_package_roots(config);

    let files = discover_python_files(project_root, &package_roots, config);
    let (registry, parsed) = parse_and_register(&files, &package_roots, true);

    let resolver = ModuleResolver::build(&registry);
    let pyproject = deps::read_pyproject(project_root);

    let mut entries = seed_static_entries(
        config,
        project_root,
        &registry,
        &resolver,
        &parsed,
        pyproject.entries,
    );
    let mut plugins_run = Vec::new();
    run_enabled_plugins(config, &parsed, &mut entries, &mut plugins_run);

    let graph = ModuleGraph::build(&resolver, &parsed, entries);

    let mut issues = Vec::new();
    let unreachable: rustc_hash::FxHashSet<FileId> = graph
        .unreachable_files(&registry, &resolver)
        .into_iter()
        .collect();
    for id in &unreachable {
        if let Some(node) = registry.get(*id) {
            issues.push(Issue::UnusedFile {
                path: node.path.clone(),
            });
        }
    }
    for (id, module) in &parsed {
        if unreachable.contains(id) {
            continue;
        }
        let Some(node) = registry.get(*id) else {
            continue;
        };
        for ui in &module.unused_imports {
            issues.push(Issue::UnusedImport {
                path: node.path.clone(),
                line: ui.line,
                name: ui.name.clone(),
                module: ui.module.clone(),
            });
        }
    }

    issues.extend(circular::analyze(&graph, &registry));

    let imported_top_level: FxHashSet<String> = parsed
        .values()
        .flat_map(|m| m.imports.iter())
        .filter(|i| matches!(i.kind, ImportKind::Absolute))
        .filter_map(|i| i.raw.split('.').next().map(String::from))
        .filter(|s| !s.is_empty())
        .collect();
    for dep in &pyproject.deps {
        if deps::is_implicit_runtime(&dep.name) {
            continue;
        }
        let candidates = deps::dist_to_import_names(&dep.name);
        let used = candidates.iter().any(|c| imported_top_level.contains(c));
        if !used {
            issues.push(Issue::UnusedDep {
                path: pyproject.path.clone(),
                name: dep.name.clone(),
                source: dep.source.clone(),
            });
        }
    }

    issues.sort_by(|a, b| {
        (a.path(), a.line().unwrap_or(0)).cmp(&(b.path(), b.line().unwrap_or(0)))
    });

    let results = AnalysisResults {
        issues,
        stats: AnalysisStats {
            files_scanned: registry.len(),
            entry_points: graph.entry_points.len(),
            plugins_run,
            elapsed_ms: started.elapsed().as_millis() as u64,
        },
    };
    Ok((results, parsed))
}

pub fn collect_inventory(config: &ResolvedConfig) -> Result<Inventory, AnalyzerError> {
    let project_root = &config.project_root;
    let package_roots = resolve_package_roots(config);

    let files = discover_python_files(project_root, &package_roots, config);
    let (registry, parsed) = parse_and_register(&files, &package_roots, false);

    let resolver = ModuleResolver::build(&registry);
    let pyproject = deps::read_pyproject(project_root);

    let mut entries = seed_static_entries(
        config,
        project_root,
        &registry,
        &resolver,
        &parsed,
        pyproject.entries,
    );
    let mut plugins_run = Vec::new();
    run_enabled_plugins(config, &parsed, &mut entries, &mut plugins_run);

    let mut inventory_entries: Vec<InventoryEntryPoint> = entries
        .iter()
        .filter_map(|e| {
            let node = registry.get(e.file)?;
            let dotted = registry.dotted_of(e.file).unwrap_or("").to_string();
            Some(InventoryEntryPoint {
                path: node.path.clone(),
                dotted_module: dotted,
                source: e.source.clone(),
            })
        })
        .collect();
    inventory_entries.sort_by(|a, b| a.path.cmp(&b.path));

    let mut inventory_files: Vec<InventoryFile> = registry
        .all_ids()
        .filter_map(|id| {
            let node = registry.get(id)?;
            let dotted = registry.dotted_of(id).unwrap_or("").to_string();
            Some(InventoryFile {
                path: node.path.clone(),
                dotted_module: dotted,
                kind: node.kind,
            })
        })
        .collect();
    inventory_files.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(Inventory {
        entry_points: inventory_entries,
        files: inventory_files,
        plugins_run,
    })
}

fn merge_plugin_result(result: &PluginResult, entries: &mut Vec<EntryPoint>) {
    let plugin_label = result.plugin_name.clone();
    for &id in &result.entry_files {
        entries.push(EntryPoint {
            file: id,
            source: EntryPointSource::Plugin(plugin_label.clone()),
        });
    }
}

pub fn resolve_package_roots(config: &ResolvedConfig) -> Vec<PathBuf> {
    let raw: Vec<PathBuf> = if !config.package_roots.is_empty() {
        config
            .package_roots
            .iter()
            .map(|r| {
                if r.is_absolute() {
                    r.clone()
                } else {
                    config.project_root.join(r)
                }
            })
            .collect()
    } else {
        let src = config.project_root.join("src");
        if src.is_dir() && !src.join("__init__.py").is_file() {
            vec![src]
        } else {
            vec![config.project_root.clone()]
        }
    };
    raw.into_iter()
        .map(|p| p.canonicalize().unwrap_or(p))
        .collect()
}

pub fn discover_python_files(
    project_root: &Path,
    package_roots: &[PathBuf],
    config: &ResolvedConfig,
) -> Vec<PathBuf> {
    let ignore_set = build_ignore_set(&config.ignore_patterns);
    let mut out = Vec::new();
    let mut seen: rustc_hash::FxHashSet<PathBuf> = rustc_hash::FxHashSet::default();
    for root in package_roots {
        let walker = WalkBuilder::new(root)
            .hidden(false)
            .git_ignore(true)
            .git_global(false)
            .build();
        for entry in walker.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("py") {
                continue;
            }
            if let Some(set) = &ignore_set {
                let rel = path.strip_prefix(project_root).unwrap_or(path);
                if set.is_match(rel) {
                    continue;
                }
            }
            let canonical = path.to_path_buf();
            if seen.insert(canonical.clone()) {
                out.push(canonical);
            }
        }
    }
    out
}

fn build_ignore_set(patterns: &[String]) -> Option<globset::GlobSet> {
    if patterns.is_empty() {
        return None;
    }
    let mut builder = globset::GlobSetBuilder::new();
    for pat in patterns.iter().filter_map(|p| globset::Glob::new(p).ok()) {
        builder.add(pat);
    }
    builder.build().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_config::ResolvedConfig;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn flags_orphan_when_main_is_explicit_entry() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::create_dir_all(dir.join("app")).unwrap();
        fs::write(
            dir.join("app/main.py"),
            "from app.helper import work\nwork()\n",
        )
        .unwrap();
        fs::write(dir.join("app/helper.py"), "def work():\n    pass\n").unwrap();
        fs::write(
            dir.join("app/orphan.py"),
            "def never_called():\n    pass\n",
        )
        .unwrap();

        let mut cfg = ResolvedConfig {
            project_root: dir.clone(),
            package_roots: vec![dir.clone()],
            entry_points: vec![dir.join("app/main.py")],
            ..Default::default()
        };
        cfg.plugins
            .entry("fastapi".into())
            .and_modify(|p| p.enabled = false);

        let result = analyze(&cfg).unwrap();
        let orphans: Vec<_> = result
            .issues
            .iter()
            .filter_map(|i| match i {
                Issue::UnusedFile { path } => path.file_name().and_then(|s| s.to_str()),
                _ => None,
            })
            .collect();
        assert_eq!(orphans, vec!["orphan.py"]);
        assert_eq!(result.stats.files_scanned, 3);
    }

    #[test]
    fn auto_detects_src_layout_without_init() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.py"), "pass\n").unwrap();
        let cfg = ResolvedConfig {
            project_root: dir.clone(),
            ..Default::default()
        };
        let roots = resolve_package_roots(&cfg);
        assert_eq!(roots, vec![dir.join("src").canonicalize().unwrap()]);
    }

    #[test]
    fn dunder_main_py_treated_as_entry() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::create_dir_all(dir.join("pkg")).unwrap();
        fs::write(dir.join("pkg/__init__.py"), "").unwrap();
        fs::write(
            dir.join("pkg/__main__.py"),
            "from pkg.lib import work\nwork()\n",
        )
        .unwrap();
        fs::write(dir.join("pkg/lib.py"), "def work():\n    pass\n").unwrap();
        fs::write(dir.join("pkg/orphan.py"), "def x():\n    pass\n").unwrap();

        let mut cfg = ResolvedConfig {
            project_root: dir.clone(),
            package_roots: vec![dir.clone()],
            ..Default::default()
        };
        cfg.plugins
            .entry("fastapi".into())
            .and_modify(|p| p.enabled = false);

        let result = analyze(&cfg).unwrap();
        let flagged: Vec<_> = result
            .issues
            .iter()
            .filter_map(|i| match i {
                Issue::UnusedFile { path } => path.file_name().and_then(|s| s.to_str()),
                _ => None,
            })
            .collect();
        assert_eq!(flagged, vec!["orphan.py"]);
    }

    #[test]
    fn if_name_main_script_treated_as_entry() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::write(
            dir.join("script.py"),
            "from helper import work\nif __name__ == \"__main__\":\n    work()\n",
        )
        .unwrap();
        fs::write(dir.join("helper.py"), "def work():\n    pass\n").unwrap();
        fs::write(dir.join("orphan.py"), "def x():\n    pass\n").unwrap();

        let mut cfg = ResolvedConfig {
            project_root: dir.clone(),
            package_roots: vec![dir.clone()],
            ..Default::default()
        };
        cfg.plugins
            .entry("fastapi".into())
            .and_modify(|p| p.enabled = false);

        let result = analyze(&cfg).unwrap();
        let flagged: Vec<_> = result
            .issues
            .iter()
            .filter_map(|i| match i {
                Issue::UnusedFile { path } => path.file_name().and_then(|s| s.to_str()),
                _ => None,
            })
            .collect();
        assert_eq!(flagged, vec!["orphan.py"]);
    }

    #[test]
    fn flags_unused_dependency() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::write(
            dir.join("pyproject.toml"),
            "[project]\nname=\"x\"\ndependencies = [\"requests\", \"unused-pkg\"]\n",
        )
        .unwrap();
        fs::write(
            dir.join("main.py"),
            "import requests\nprint(requests.get(\"https://x\"))\n",
        )
        .unwrap();

        let mut cfg = ResolvedConfig {
            project_root: dir.clone(),
            package_roots: vec![dir.clone()],
            entry_points: vec![dir.join("main.py")],
            ..Default::default()
        };
        cfg.plugins
            .entry("fastapi".into())
            .and_modify(|p| p.enabled = false);

        let result = analyze(&cfg).unwrap();
        let unused_deps: Vec<_> = result
            .issues
            .iter()
            .filter_map(|i| match i {
                Issue::UnusedDep { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(unused_deps, vec!["unused-pkg"]);
    }

    #[test]
    fn auto_detects_project_root_when_src_is_a_package() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/__init__.py"), "").unwrap();
        fs::write(dir.join("src/main.py"), "pass\n").unwrap();
        let cfg = ResolvedConfig {
            project_root: dir.clone(),
            ..Default::default()
        };
        let roots = resolve_package_roots(&cfg);
        assert_eq!(roots, vec![dir.canonicalize().unwrap()]);
    }
}
