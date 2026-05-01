use ignore::WalkBuilder;
use pyllow_config::ResolvedConfig;
use pyllow_extract::{parse_file, ParsedModule};
use pyllow_graph::{
    dotted_module_for, is_python_identifier, FileRegistry, ModuleGraph, ModuleResolver,
};
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
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AnalyzerError {
    #[error("config error: {0}")]
    Config(#[from] pyllow_config::ConfigError),
    #[error("parse error: {0}")]
    Extract(#[from] pyllow_extract::ExtractError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Auto-detected workspace members would collide on a top-level
    /// dotted name; the single-graph resolver can't disambiguate them.
    #[error("workspace error: {0}")]
    Workspace(String),
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
    (
        pyllow_plugin_sqlmodel::PLUGIN_NAME,
        pyllow_plugin_sqlmodel::discover,
    ),
    (
        pyllow_plugin_marshmallow::PLUGIN_NAME,
        pyllow_plugin_marshmallow::discover,
    ),
    (
        pyllow_plugin_starlette::PLUGIN_NAME,
        pyllow_plugin_starlette::discover,
    ),
    (
        pyllow_plugin_aiohttp::PLUGIN_NAME,
        pyllow_plugin_aiohttp::discover,
    ),
    (
        pyllow_plugin_flask::PLUGIN_NAME,
        pyllow_plugin_flask::discover,
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
            config
                .plugins
                .get(*name)
                .map(|c| c.enabled)
                .unwrap_or(false)
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
    registry: &FileRegistry,
    resolver: &ModuleResolver<'_>,
    parsed: &FxHashMap<FileId, ParsedModule>,
    pyproject_entries: &[deps::PyprojectEntry],
    project_names: &[String],
) -> Vec<EntryPoint> {
    let mut entries = Vec::new();

    for ep_path in &config.entry_points {
        let abs = if ep_path.is_absolute() {
            ep_path.clone()
        } else {
            config.project_root.join(ep_path)
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
                source: EntryPointSource::PyprojectEntryPoint(entry.group.clone()),
            });
        }
    }

    // Library-mode public-API entry per `[project] name`. Without this,
    // a library's `__init__.py` is unreachable from internal call sites
    // and the public API would falsely report as unused.
    let mut already: FxHashSet<FileId> = entries.iter().map(|e| e.file).collect();
    for name in project_names {
        for candidate in deps::library_init_candidates(name) {
            if let Some(id) = resolver.resolve_dotted(&candidate) {
                if already.insert(id) {
                    entries.push(EntryPoint {
                        file: id,
                        source: EntryPointSource::LibraryPublicApi,
                    });
                }
                break;
            }
        }
    }

    entries
}

/// Parse a list of files in parallel and return them keyed by synthetic
/// `FileId`s (assigned by enumeration order). Suitable for CLI commands
/// like `health`, `flags`, and `smells` that don't need a `FileRegistry`
/// because their analyses only consume the parsed AST map.
///
/// Files that fail to parse are returned as `Issue::ParseError` entries
/// so callers can fold them into their issue list — otherwise `pyllow
/// health/flags/smells` would silently exclude broken files and pass.
pub fn parse_files_into_map(files: &[PathBuf]) -> (FxHashMap<FileId, ParsedModule>, Vec<Issue>) {
    let outcomes: Vec<Result<ParsedModule, Issue>> = files
        .par_iter()
        .map(|p| {
            parse_file(p).map_err(|e| Issue::ParseError {
                path: p.clone(),
                message: e.to_string(),
            })
        })
        .collect();
    let mut parsed = FxHashMap::default();
    let mut errors = Vec::new();
    let mut next_id = 0u32;
    for outcome in outcomes {
        match outcome {
            Ok(m) => {
                parsed.insert(FileId(next_id), m);
                next_id += 1;
            }
            Err(issue) => errors.push(issue),
        }
    }
    (parsed, errors)
}

/// Parse files in parallel and register each one in a `FileRegistry`.
/// Used by the analyzer pipeline (which needs real file→FileId lookups
/// for graph traversal) — distinct from `parse_files_into_map`, which is
/// for CLI commands that only consume ASTs.
///
/// Files that fail to parse are returned as `parse_errors` so callers can
/// promote them to first-class `Issue::ParseError` entries. Silently
/// dropping them would let CI pass while entire files were excluded from
/// every other check.
fn parse_and_register(
    files: &[PathBuf],
    package_roots: &[PathBuf],
) -> (
    FileRegistry,
    FxHashMap<FileId, ParsedModule>,
    Vec<(PathBuf, String)>,
) {
    let outcomes: Vec<Result<(PathBuf, ParsedModule), (PathBuf, String)>> = files
        .par_iter()
        .map(|path| match parse_file(path) {
            Ok(m) => Ok((path.clone(), m)),
            Err(e) => Err((path.clone(), e.to_string())),
        })
        .collect();

    let mut registry = FileRegistry::default();
    let mut parsed: FxHashMap<FileId, ParsedModule> = FxHashMap::default();
    let mut parse_errors = Vec::new();
    for outcome in outcomes {
        match outcome {
            Ok((path, module)) => {
                let dotted = dotted_module_for(&path, package_roots).unwrap_or_default();
                let id = registry.register(path, dotted);
                parsed.insert(id, module);
            }
            Err(e) => parse_errors.push(e),
        }
    }
    (registry, parsed, parse_errors)
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
    let package_roots = resolve_package_roots(config)?;

    let files = discover_python_files(project_root, &package_roots, config);
    let (registry, parsed, parse_errors) = parse_and_register(&files, &package_roots);

    let resolver = ModuleResolver::build(&registry);
    let pyproject = deps::read_pyprojects(&pyproject_search_roots(project_root, &package_roots));

    let mut entries = seed_static_entries(
        config,
        &registry,
        &resolver,
        &parsed,
        &pyproject.entries,
        &pyproject.project_names,
    );
    let mut plugins_run = Vec::new();
    run_enabled_plugins(config, &parsed, &mut entries, &mut plugins_run);

    let graph = ModuleGraph::build(&resolver, &parsed, entries);

    let mut issues: Vec<Issue> = parse_errors
        .into_iter()
        .map(|(path, message)| Issue::ParseError { path, message })
        .collect();
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

    // Per-package scoping: a workspace where pkg_a and pkg_b both list
    // `requests` but only pkg_b imports it must still flag pkg_a's
    // declaration. Bucket reachable absolute imports by owning root.
    let owner_of = |dep: &deps::DeclaredDep| -> PathBuf {
        dep.source_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| project_root.clone())
    };
    let mut owners: Vec<PathBuf> = pyproject.deps.iter().map(&owner_of).collect();
    owners.sort_by_key(|p| std::cmp::Reverse(p.as_os_str().len()));
    owners.dedup();

    let mut imports_by_owner: FxHashMap<PathBuf, FxHashSet<String>> = owners
        .iter()
        .map(|o| (o.clone(), FxHashSet::default()))
        .collect();
    for (id, module) in &parsed {
        if unreachable.contains(id) {
            continue;
        }
        // Longest-prefix wins (workspace-member > workspace-root).
        let Some(owner) = owners.iter().find(|o| module.path.starts_with(o)) else {
            continue;
        };
        let bucket = imports_by_owner
            .get_mut(owner)
            .expect("owners pre-populated");
        for import in &module.imports {
            if !matches!(import.kind, ImportKind::Absolute) || import.is_type_only {
                continue;
            }
            if let Some(top) = import.raw.split('.').next().filter(|s| !s.is_empty()) {
                bucket.insert(top.to_string());
            }
        }
    }

    for dep in &pyproject.deps {
        if deps::is_implicit_runtime(&dep.name) {
            continue;
        }
        let bucket = imports_by_owner.get(&owner_of(dep));
        let candidates = deps::dist_to_import_names(&dep.name);
        let used = bucket.is_some_and(|set| candidates.iter().any(|c| set.contains(c)));
        if !used {
            issues.push(Issue::UnusedDep {
                path: dep.source_path.clone(),
                name: dep.name.clone(),
                source: dep.source.clone(),
            });
        }
    }

    issues
        .sort_by(|a, b| (a.path(), a.line().unwrap_or(0)).cmp(&(b.path(), b.line().unwrap_or(0))));

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
    let package_roots = resolve_package_roots(config)?;

    let files = discover_python_files(project_root, &package_roots, config);
    // `list` only inventories what we found; parse failures aren't issues
    // here, just files we couldn't dig into. Drop the error list.
    let (registry, parsed, _parse_errors) = parse_and_register(&files, &package_roots);

    let resolver = ModuleResolver::build(&registry);
    let pyproject = deps::read_pyprojects(&pyproject_search_roots(project_root, &package_roots));

    let mut entries = seed_static_entries(
        config,
        &registry,
        &resolver,
        &parsed,
        &pyproject.entries,
        &pyproject.project_names,
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

pub fn resolve_package_roots(config: &ResolvedConfig) -> Result<Vec<PathBuf>, AnalyzerError> {
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
        auto_detect_package_roots(config)?
    };
    Ok(raw
        .into_iter()
        .map(|p| p.canonicalize().unwrap_or(p))
        .collect())
}

/// Build the deduped list of directories to scan for `pyproject.toml`.
/// In a workspace layout the marker pyproject lives at `project_root`
/// while real metadata lives in member roots — both must be visited.
fn pyproject_search_roots(project_root: &Path, package_roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = vec![project_root.to_path_buf()];
    for r in package_roots {
        if !out.iter().any(|p| p == r) {
            out.push(r.clone());
        }
    }
    out
}

fn auto_detect_package_roots(config: &ResolvedConfig) -> Result<Vec<PathBuf>, AnalyzerError> {
    let project_root = &config.project_root;

    // 1. Python `src/` layout wins.
    let src = project_root.join("src");
    if src.is_dir() && !src.join("__init__.py").is_file() {
        return Ok(vec![src]);
    }

    let monorepo_roots: Vec<PathBuf> = std::fs::read_dir(project_root)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter(|e| e.path().join("pyproject.toml").is_file())
        .map(|e| e.path())
        .collect();

    // 2. Single-project layouts (no member pyprojects, top-level project
    //    metadata, or top-level pyllow config) take the project root.
    if monorepo_roots.is_empty()
        || top_level_is_python_project(project_root)
        || top_level_has_pyllow_config(project_root)
    {
        return Ok(vec![project_root.to_path_buf()]);
    }

    // 3. Real workspace: refuse if members would collide on a dotted
    //    name (the single-graph resolver can't disambiguate them, and
    //    falling back to project-root mode drops member pyprojects).
    let ignore_set = build_ignore_set(&config.ignore_patterns);
    if let Some((name, a, b)) =
        colliding_top_level_module(&monorepo_roots, project_root, ignore_set.as_ref())
    {
        return Err(AnalyzerError::Workspace(format!(
            "auto-detected monorepo members would collide on top-level module `{name}` ({} and {}). \
             Set `packageRoots` in pyllow.toml to pick one member, rename one of the colliding packages, \
             or run `pyllow check` from inside each member directory.",
            a.display(),
            b.display()
        )));
    }
    Ok(monorepo_roots)
}

/// First module name (and the two roots) where two candidate roots
/// would register the same dotted name in `dotted_module_for`.
fn colliding_top_level_module(
    roots: &[PathBuf],
    project_root: &Path,
    ignore_set: Option<&globset::GlobSet>,
) -> Option<(String, PathBuf, PathBuf)> {
    let mut seen: FxHashMap<String, PathBuf> = FxHashMap::default();
    for root in roots {
        for name in top_level_module_names(root, project_root, ignore_set) {
            if let Some(prev) = seen.get(&name) {
                return Some((name, prev.clone(), root.clone()));
            }
            seen.insert(name, root.clone());
        }
    }
    None
}

fn top_level_module_names(
    root: &Path,
    project_root: &Path,
    ignore_set: Option<&globset::GlobSet>,
) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| module_name_for_entry(&e.path(), project_root, ignore_set))
        .collect()
}

/// Name a child entry contributes to dotted-module resolution, or `None`
/// if `dotted_module_for` would never register it. PEP 420 namespace
/// packages (no `__init__.py`) still register, so any directory carrying
/// `.py` descendants counts.
fn module_name_for_entry(
    path: &Path,
    project_root: &Path,
    ignore_set: Option<&globset::GlobSet>,
) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    if name.starts_with('.') || name.starts_with("__") {
        return None;
    }
    let module = if path.is_dir() && dir_carries_python_module(path, project_root, ignore_set) {
        name.to_string()
    } else if path.is_file()
        && name != "setup.py"
        && py_file_is_analyzed(path, project_root, ignore_set)
    {
        name.trim_end_matches(".py").to_string()
    } else {
        return None;
    };
    // `dotted_module_for` rejects segments that aren't valid Python
    // identifiers (e.g. `service-a`, `12factor`), so they can't actually
    // collide. Filtering here keeps the detector aligned with the
    // resolver and avoids phantom collisions on unconventional dirnames.
    is_python_identifier(&module).then_some(module)
}

fn dir_carries_python_module(
    dir: &Path,
    project_root: &Path,
    ignore_set: Option<&globset::GlobSet>,
) -> bool {
    python_walker(dir).flatten().any(|e| {
        let path = e.path();
        path.is_file() && py_file_is_analyzed(path, project_root, ignore_set)
    })
}

/// Walker config shared by `discover_python_files` (the analyzer's main
/// file scanner) and `dir_carries_python_module` (the collision check).
/// Both must use the same gitignore + visibility rules so they agree on
/// what counts as a Python file pyllow would actually analyze.
fn python_walker(root: &Path) -> ignore::Walk {
    WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .build()
}

/// Single source of truth for "is this `.py` file analyzed by pyllow?"
/// Shared between the collision check and `discover_python_files` so
/// they can never disagree on what counts.
fn py_file_is_analyzed(
    path: &Path,
    project_root: &Path,
    ignore_set: Option<&globset::GlobSet>,
) -> bool {
    if path.extension().and_then(|s| s.to_str()) != Some("py") {
        return false;
    }
    if let Some(set) = ignore_set {
        let rel = path.strip_prefix(project_root).unwrap_or(path);
        if set.is_match(rel) {
            return false;
        }
    }
    true
}

fn top_level_is_python_project(project_root: &Path) -> bool {
    deps::pyproject_tables(project_root).has_project
}

fn top_level_has_pyllow_config(project_root: &Path) -> bool {
    project_root.join("pyllow.toml").is_file()
        || deps::pyproject_tables(project_root).has_tool_pyllow
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
        for entry in python_walker(root).flatten() {
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
    fn parse_files_into_map_surfaces_parse_errors() {
        // The AST-only entry point used by health/flags/smells used to
        // silently drop unparseable files via `.ok()`. Now it returns
        // them as `Issue::ParseError` so those CLI commands can fold them
        // into their issue list and exit non-zero.
        let tmp = tempdir().unwrap();
        let good = tmp.path().join("good.py");
        let bad = tmp.path().join("bad.py");
        fs::write(&good, "def f():\n    return 1\n").unwrap();
        fs::write(&bad, "def\n").unwrap();
        let (parsed, errors) = parse_files_into_map(&[good.clone(), bad.clone()]);
        assert_eq!(parsed.len(), 1);
        let bad_paths: Vec<_> = errors
            .iter()
            .filter_map(|i| match i {
                Issue::ParseError { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(bad_paths, vec![bad]);
    }

    #[test]
    fn library_pyproject_init_is_treated_as_entry() {
        // Library-mode regression: without `[project] name`-driven entry
        // detection, `mylib/__init__.py` and everything it re-exports
        // would all show up as unused-file even though the public API
        // lives there. With the auto-detection, they're reachable.
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::create_dir_all(dir.join("mylib")).unwrap();
        fs::write(dir.join("pyproject.toml"), "[project]\nname = \"mylib\"\n").unwrap();
        fs::write(
            dir.join("mylib/__init__.py"),
            "from .core import work as work\n",
        )
        .unwrap();
        fs::write(dir.join("mylib/core.py"), "def work():\n    pass\n").unwrap();

        let mut cfg = ResolvedConfig {
            project_root: dir.clone(),
            package_roots: vec![dir.clone()],
            ..Default::default()
        };
        cfg.plugins
            .entry("fastapi".into())
            .and_modify(|p| p.enabled = false);

        let result = analyze(&cfg).unwrap();
        let unused: Vec<_> = result
            .issues
            .iter()
            .filter_map(|i| match i {
                Issue::UnusedFile { path } => path.file_name().and_then(|s| s.to_str()),
                _ => None,
            })
            .collect();
        assert!(
            unused.is_empty(),
            "library mode must reach __init__.py and re-exports, got unused: {unused:?}"
        );
    }

    #[test]
    fn library_pyproject_with_dashed_name_resolves_to_underscored_module() {
        // PEP 503 normalization: dist `scikit-learn` → module `scikit_learn`.
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::create_dir_all(dir.join("my_lib")).unwrap();
        fs::write(dir.join("pyproject.toml"), "[project]\nname = \"my-lib\"\n").unwrap();
        fs::write(dir.join("my_lib/__init__.py"), "from .core import work\n").unwrap();
        fs::write(dir.join("my_lib/core.py"), "def work():\n    pass\n").unwrap();

        let mut cfg = ResolvedConfig {
            project_root: dir.clone(),
            package_roots: vec![dir.clone()],
            ..Default::default()
        };
        cfg.plugins
            .entry("fastapi".into())
            .and_modify(|p| p.enabled = false);

        let result = analyze(&cfg).unwrap();
        let unused: Vec<_> = result
            .issues
            .iter()
            .filter_map(|i| match i {
                Issue::UnusedFile { path } => path.file_name().and_then(|s| s.to_str()),
                _ => None,
            })
            .collect();
        assert!(unused.is_empty(), "got unused: {unused:?}");
    }

    #[test]
    fn parse_failures_surface_as_first_class_issues() {
        // Without this, a syntax error would silently exclude the file
        // from every check (graph, unused-import, etc.) and CI would
        // report "no issues" while losing visibility into the file.
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::create_dir_all(dir.join("app")).unwrap();
        fs::write(dir.join("app/main.py"), "import sys\nprint(sys.argv)\n").unwrap();
        // Genuinely unparseable: dangling `def` with no body.
        fs::write(dir.join("app/broken.py"), "def\n").unwrap();
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
        let parse_errors: Vec<_> = result
            .issues
            .iter()
            .filter_map(|i| match i {
                Issue::ParseError { path, .. } => path.file_name().and_then(|s| s.to_str()),
                _ => None,
            })
            .collect();
        assert_eq!(parse_errors, vec!["broken.py"]);
    }

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
        fs::write(dir.join("app/orphan.py"), "def never_called():\n    pass\n").unwrap();

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
        let roots = resolve_package_roots(&cfg).unwrap();
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
        let roots = resolve_package_roots(&cfg).unwrap();
        assert_eq!(roots, vec![dir.canonicalize().unwrap()]);
    }

    #[test]
    fn auto_detects_monorepo_subdirs_with_pyproject() {
        // Polyglot monorepo: `backend/pyproject.toml` next to `frontend/`
        // (Node project, no pyproject). Pyllow should pick `backend/` as
        // the package root, not the parent — otherwise `from app.routers
        // import checkins` would fail to resolve and flag every file
        // under `backend/app/` as orphaned.
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::create_dir_all(dir.join("backend")).unwrap();
        fs::create_dir_all(dir.join("frontend")).unwrap();
        fs::write(
            dir.join("backend/pyproject.toml"),
            "[project]\nname=\"x\"\n",
        )
        .unwrap();
        fs::write(dir.join("frontend/package.json"), "{}").unwrap();
        let cfg = ResolvedConfig {
            project_root: dir.clone(),
            ..Default::default()
        };
        let roots = resolve_package_roots(&cfg).unwrap();
        assert_eq!(roots, vec![dir.join("backend").canonicalize().unwrap()]);
    }

    #[test]
    fn top_level_python_project_overrides_monorepo_detection() {
        // If the top pyproject has `[project]`, treat it as a single
        // project even if subdirs also have pyprojects.
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::write(dir.join("pyproject.toml"), "[project]\nname=\"top\"\n").unwrap();
        fs::create_dir_all(dir.join("subpkg")).unwrap();
        fs::write(
            dir.join("subpkg/pyproject.toml"),
            "[project]\nname=\"sub\"\n",
        )
        .unwrap();
        let cfg = ResolvedConfig {
            project_root: dir.clone(),
            ..Default::default()
        };
        let roots = resolve_package_roots(&cfg).unwrap();
        assert_eq!(roots, vec![dir.canonicalize().unwrap()]);
    }

    #[test]
    fn workspace_with_colliding_top_level_packages_fails_fast() {
        // Two services both contain a top-level `app/` package. The
        // single-resolver architecture can't distinguish them, AND
        // falling back to project-root mode drops every member's
        // pyproject (so script entries disappear and files look unused).
        // Refuse instead, with an actionable error.
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        for svc in ["service_a", "service_b"] {
            fs::create_dir_all(dir.join(svc).join("app")).unwrap();
            fs::write(
                dir.join(svc).join("pyproject.toml"),
                "[project]\nname=\"x\"\n",
            )
            .unwrap();
            fs::write(dir.join(svc).join("app/__init__.py"), "").unwrap();
            fs::write(dir.join(svc).join("app/main.py"), "pass\n").unwrap();
        }
        let cfg = ResolvedConfig {
            project_root: dir.clone(),
            ..Default::default()
        };
        let err = resolve_package_roots(&cfg).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("`app`"),
            "should name the colliding module: {msg}"
        );
        assert!(msg.contains("packageRoots"), "should suggest a fix: {msg}");
    }

    #[test]
    fn workspace_ignored_top_level_py_files_dont_trigger_false_collision() {
        // Both members ship a top-level ignored `generated.py` directly
        // under the member root (NOT inside a subdir). The file branch
        // of top_level_module_names must apply the same ignore filter
        // as the directory branch, otherwise these collide on
        // `generated` even though discover_python_files skips them.
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        // Use a project-level .pyllowignore so the pattern flows through
        // the same path users would configure.
        fs::write(dir.join(".pyllowignore"), "**/generated.py\n").unwrap();
        for (svc, pkg) in [("service_a", "alpha"), ("service_b", "beta")] {
            fs::create_dir_all(dir.join(svc).join(pkg)).unwrap();
            fs::write(
                dir.join(svc).join("pyproject.toml"),
                "[project]\nname=\"x\"\n",
            )
            .unwrap();
            fs::write(dir.join(svc).join(pkg).join("__init__.py"), "").unwrap();
            fs::write(dir.join(svc).join("generated.py"), "X = 1\n").unwrap();
        }
        // Load config so `.pyllowignore` populates ignore_patterns.
        let cfg = pyllow_config::ResolvedConfig::load(&dir).unwrap();
        let mut roots =
            resolve_package_roots(&cfg).expect("ignored top-level .py files must not collide");
        roots.sort();
        let mut expected = vec![
            dir.join("service_a").canonicalize().unwrap(),
            dir.join("service_b").canonicalize().unwrap(),
        ];
        expected.sort();
        assert_eq!(roots, expected);
    }

    #[test]
    fn workspace_ignored_python_files_dont_trigger_false_collision() {
        // Both members ship a `build/` directory containing
        // generated/compiled .py files. `build/` is in pyllow's default
        // ignore patterns so those files are never analyzed —
        // collision detection must respect the same ignore set or
        // legitimate workspaces with shared output dirs would fail fast
        // for nothing.
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        for (svc, pkg) in [("service_a", "alpha"), ("service_b", "beta")] {
            fs::create_dir_all(dir.join(svc).join(pkg)).unwrap();
            fs::create_dir_all(dir.join(svc).join("build")).unwrap();
            fs::write(
                dir.join(svc).join("pyproject.toml"),
                "[project]\nname=\"x\"\n",
            )
            .unwrap();
            fs::write(dir.join(svc).join(pkg).join("__init__.py"), "").unwrap();
            // Generated build artifact — would collide on `build` if we
            // didn't apply ignore patterns.
            fs::write(dir.join(svc).join("build/generated.py"), "GENERATED = 1\n").unwrap();
        }
        let cfg = ResolvedConfig {
            project_root: dir.clone(),
            ..Default::default()
        };
        let mut roots = resolve_package_roots(&cfg).expect("ignored files must not collide");
        roots.sort();
        let mut expected = vec![
            dir.join("service_a").canonicalize().unwrap(),
            dir.join("service_b").canonicalize().unwrap(),
        ];
        expected.sort();
        assert_eq!(roots, expected);
    }

    #[test]
    fn workspace_non_python_sibling_dirs_dont_trigger_false_collision() {
        // Both services have a `docs/assets/` and an `images/` directory
        // — neither contains any `.py` file. These don't contribute to
        // `dotted_module_for` registration, so they must NOT be treated
        // as colliding modules. Otherwise legitimate monorepos with
        // shared documentation layouts can't be auto-detected.
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        for (svc, pkg) in [("service_a", "alpha"), ("service_b", "beta")] {
            fs::create_dir_all(dir.join(svc).join(pkg)).unwrap();
            fs::create_dir_all(dir.join(svc).join("docs/assets")).unwrap();
            fs::create_dir_all(dir.join(svc).join("images")).unwrap();
            fs::write(
                dir.join(svc).join("pyproject.toml"),
                "[project]\nname=\"x\"\n",
            )
            .unwrap();
            fs::write(dir.join(svc).join(pkg).join("__init__.py"), "").unwrap();
            // No Python files in docs/ or images/ — they're just static.
            fs::write(dir.join(svc).join("docs/README.md"), "# docs\n").unwrap();
            fs::write(dir.join(svc).join("images/logo.png"), "").unwrap();
        }
        let cfg = ResolvedConfig {
            project_root: dir.clone(),
            ..Default::default()
        };
        let mut roots = resolve_package_roots(&cfg).expect("non-Python siblings must not fail");
        roots.sort();
        let mut expected = vec![
            dir.join("service_a").canonicalize().unwrap(),
            dir.join("service_b").canonicalize().unwrap(),
        ];
        expected.sort();
        assert_eq!(roots, expected);
    }

    #[test]
    fn workspace_with_namespace_package_collision_also_fails_fast() {
        // PEP 420: namespace packages have no `__init__.py` but
        // `dotted_module_for` still registers their files, so the
        // collision is just as harmful even when neither `app/` carries
        // an `__init__.py` marker.
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        for svc in ["service_a", "service_b"] {
            fs::create_dir_all(dir.join(svc).join("app")).unwrap();
            fs::write(
                dir.join(svc).join("pyproject.toml"),
                "[project]\nname=\"x\"\n",
            )
            .unwrap();
            // Note: NO __init__.py — namespace package.
            fs::write(dir.join(svc).join("app/main.py"), "pass\n").unwrap();
        }
        let cfg = ResolvedConfig {
            project_root: dir.clone(),
            ..Default::default()
        };
        let err = resolve_package_roots(&cfg).unwrap_err();
        assert!(err.to_string().contains("`app`"));
    }

    #[test]
    fn workspace_with_distinct_top_level_packages_uses_monorepo_detection() {
        // Sanity: when each member has its OWN top-level package name
        // (the common library workspace case), monorepo detection should
        // still kick in.
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        for (svc, pkg) in [("service_a", "alpha"), ("service_b", "beta")] {
            fs::create_dir_all(dir.join(svc).join(pkg)).unwrap();
            fs::write(
                dir.join(svc).join("pyproject.toml"),
                "[project]\nname=\"x\"\n",
            )
            .unwrap();
            fs::write(dir.join(svc).join(pkg).join("__init__.py"), "").unwrap();
        }
        let cfg = ResolvedConfig {
            project_root: dir.clone(),
            ..Default::default()
        };
        let mut roots = resolve_package_roots(&cfg).unwrap();
        roots.sort();
        let mut expected = vec![
            dir.join("service_a").canonicalize().unwrap(),
            dir.join("service_b").canonicalize().unwrap(),
        ];
        expected.sort();
        assert_eq!(roots, expected);
    }

    #[test]
    fn workspace_unused_dep_is_scoped_per_member() {
        // Both `pkg_a` and `pkg_b` declare `requests`, but only `pkg_b`
        // imports it. With a global imported-set the `pkg_a` declaration
        // would falsely look "used"; per-package scoping correctly flags
        // only `pkg_a`'s pyproject.
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::write(
            dir.join("pyproject.toml"),
            "[tool.uv.workspace]\nmembers = [\"pkg_a\", \"pkg_b\"]\n",
        )
        .unwrap();
        for pkg in ["pkg_a", "pkg_b"] {
            fs::create_dir_all(dir.join(pkg).join(pkg)).unwrap();
            fs::write(
                dir.join(pkg).join("pyproject.toml"),
                format!("[project]\nname=\"{pkg}\"\ndependencies = [\"requests\"]\n"),
            )
            .unwrap();
            fs::write(
                dir.join(pkg).join(pkg).join("__init__.py"),
                "from .core import work\n",
            )
            .unwrap();
        }
        // Only pkg_b actually imports requests.
        fs::write(
            dir.join("pkg_a/pkg_a/core.py"),
            "def work():\n    return 1\n",
        )
        .unwrap();
        fs::write(
            dir.join("pkg_b/pkg_b/core.py"),
            "import requests\ndef work():\n    return requests.get('x')\n",
        )
        .unwrap();
        let mut cfg = ResolvedConfig {
            project_root: dir.clone(),
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
                Issue::UnusedDep { name, path, .. } => Some((name.clone(), path.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(unused_deps.len(), 1, "got {unused_deps:?}");
        let (name, path) = &unused_deps[0];
        assert_eq!(name, "requests");
        assert!(
            path.ends_with("pkg_a/pyproject.toml"),
            "unused-dep must be attributed to pkg_a (the member that doesn't import it), got {path:?}"
        );
    }

    #[test]
    fn workspace_with_multiple_library_members_seeds_each_init_as_entry() {
        // Two sibling library packages in a uv workspace. Each has its
        // own `__init__.py` as the public API. With single-name
        // aggregation only the first would be reachable; this asserts
        // both libA/__init__.py and libB/__init__.py are entries (and
        // their imports stay reachable).
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::write(
            dir.join("pyproject.toml"),
            "[tool.uv.workspace]\nmembers = [\"liba\", \"libb\"]\n",
        )
        .unwrap();
        for member in ["liba", "libb"] {
            fs::create_dir_all(dir.join(member).join(member)).unwrap();
            fs::write(
                dir.join(member).join("pyproject.toml"),
                format!("[project]\nname=\"{member}\"\n"),
            )
            .unwrap();
            fs::write(
                dir.join(member).join(member).join("__init__.py"),
                "from .core import work\n",
            )
            .unwrap();
            fs::write(
                dir.join(member).join(member).join("core.py"),
                "def work():\n    pass\n",
            )
            .unwrap();
        }
        let mut cfg = ResolvedConfig {
            project_root: dir.clone(),
            ..Default::default()
        };
        cfg.plugins
            .entry("fastapi".into())
            .and_modify(|p| p.enabled = false);

        let result = analyze(&cfg).unwrap();
        let unused: Vec<_> = result
            .issues
            .iter()
            .filter_map(|i| match i {
                Issue::UnusedFile { path } => Some(path.display().to_string()),
                _ => None,
            })
            .collect();
        assert!(
            unused.is_empty(),
            "every member library's public API must stay reachable; got unused: {unused:?}"
        );
    }

    #[test]
    fn workspace_layout_reads_member_pyproject_for_deps_and_name() {
        // uv/hatch workspace: top pyproject is just a marker, real
        // metadata lives in `backend/pyproject.toml`. Without aggregating
        // member pyprojects, backend's deps go unchecked and its
        // `[project] name` doesn't drive library auto-entry detection.
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::write(
            dir.join("pyproject.toml"),
            "[tool.uv.workspace]\nmembers = [\"backend\"]\n",
        )
        .unwrap();
        fs::create_dir_all(dir.join("backend/app")).unwrap();
        fs::write(
            dir.join("backend/pyproject.toml"),
            "[project]\nname=\"app\"\ndependencies = [\"requests\"]\n",
        )
        .unwrap();
        // app/__init__.py is the public API; library auto-entry should
        // reach it via the member pyproject's name.
        fs::write(
            dir.join("backend/app/__init__.py"),
            "from .core import work\n",
        )
        .unwrap();
        fs::write(dir.join("backend/app/core.py"), "def work():\n    pass\n").unwrap();
        let mut cfg = ResolvedConfig {
            project_root: dir.clone(),
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
                Issue::UnusedDep { name, path, .. } => Some((name.as_str(), path.clone())),
                _ => None,
            })
            .collect();
        // `requests` declared in backend/pyproject is genuinely unused →
        // we should see exactly that finding, attributed to the member
        // pyproject (NOT the workspace root marker).
        assert_eq!(unused_deps.len(), 1, "got {unused_deps:?}");
        let (name, path) = &unused_deps[0];
        assert_eq!(*name, "requests");
        assert!(
            path.ends_with("backend/pyproject.toml"),
            "unused-dep path must point to the member pyproject, got {path:?}"
        );
        // `app/core.py` is reached via the auto-detected library entry on
        // backend/app/__init__.py — without member-pyproject aggregation
        // it would be flagged as unused-file.
        let unused_files: Vec<_> = result
            .issues
            .iter()
            .filter_map(|i| match i {
                Issue::UnusedFile { path } => path.file_name().and_then(|s| s.to_str()),
                _ => None,
            })
            .collect();
        assert!(
            unused_files.is_empty(),
            "unused files should be empty, got {unused_files:?}"
        );
    }

    #[test]
    fn workspace_only_pyproject_falls_through_to_monorepo_detection() {
        // The FastAPI full-stack-template ships `[tool.uv.workspace]
        // members = ["backend"]` at the repo root with no `[project]`.
        // That's a workspace marker, not a project — the real package
        // lives in `backend/`.
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::write(
            dir.join("pyproject.toml"),
            "[tool.uv.workspace]\nmembers = [\"backend\"]\n",
        )
        .unwrap();
        fs::create_dir_all(dir.join("backend")).unwrap();
        fs::write(
            dir.join("backend/pyproject.toml"),
            "[project]\nname=\"app\"\n",
        )
        .unwrap();
        let cfg = ResolvedConfig {
            project_root: dir.clone(),
            ..Default::default()
        };
        let roots = resolve_package_roots(&cfg).unwrap();
        assert_eq!(roots, vec![dir.join("backend").canonicalize().unwrap()]);
    }

    #[test]
    fn root_pyllow_toml_blocks_monorepo_override() {
        // When the user puts `pyllow.toml` at the project root they're
        // explicitly claiming the root IS the project pyllow should
        // analyze. A child `examples/pyproject.toml` (or any vendored
        // sub-project) must not redirect package discovery away from
        // root-level entryPoints.
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::write(dir.join("pyllow.toml"), "entryPoints = [\"main.py\"]\n").unwrap();
        fs::write(dir.join("main.py"), "pass\n").unwrap();
        fs::create_dir_all(dir.join("examples")).unwrap();
        fs::write(
            dir.join("examples/pyproject.toml"),
            "[project]\nname=\"example\"\n",
        )
        .unwrap();
        let cfg = ResolvedConfig {
            project_root: dir.clone(),
            ..Default::default()
        };
        let roots = resolve_package_roots(&cfg).unwrap();
        assert_eq!(roots, vec![dir.canonicalize().unwrap()]);
    }

    #[test]
    fn root_tool_pyllow_with_trailing_comment_still_blocks_override() {
        // TOML headers can carry trailing comments and whitespace —
        // `[tool.pyllow] # root config` is valid. A naive line-equality
        // check would miss it and silently drop root entryPoints.
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::write(
            dir.join("pyproject.toml"),
            "[tool.pyllow] # pyllow root config\nentryPoints = [\"main.py\"]\n",
        )
        .unwrap();
        fs::create_dir_all(dir.join("examples")).unwrap();
        fs::write(
            dir.join("examples/pyproject.toml"),
            "[project]\nname=\"example\"\n",
        )
        .unwrap();
        let cfg = ResolvedConfig {
            project_root: dir.clone(),
            ..Default::default()
        };
        let roots = resolve_package_roots(&cfg).unwrap();
        assert_eq!(roots, vec![dir.canonicalize().unwrap()]);
    }

    #[test]
    fn root_project_with_trailing_comment_is_recognized() {
        // Same shape, but for `[project]` — `[project] # PEP 621` should
        // still mark the parent as the canonical project so subdirs
        // don't take over.
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::write(
            dir.join("pyproject.toml"),
            "[project] # PEP 621\nname=\"top\"\n",
        )
        .unwrap();
        fs::create_dir_all(dir.join("subpkg")).unwrap();
        fs::write(
            dir.join("subpkg/pyproject.toml"),
            "[project]\nname=\"sub\"\n",
        )
        .unwrap();
        let cfg = ResolvedConfig {
            project_root: dir.clone(),
            ..Default::default()
        };
        let roots = resolve_package_roots(&cfg).unwrap();
        assert_eq!(roots, vec![dir.canonicalize().unwrap()]);
    }

    #[test]
    fn unrelated_tool_prefix_does_not_match_tool_pyllow() {
        // Defensive: `[tool.pyllowext]` (a hypothetical extension tool)
        // must not be mistaken for `[tool.pyllow]`. The dot-suffix gate
        // is what prevents this.
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::write(dir.join("pyproject.toml"), "[tool.pyllowext]\nfoo=1\n").unwrap();
        fs::create_dir_all(dir.join("backend")).unwrap();
        fs::write(
            dir.join("backend/pyproject.toml"),
            "[project]\nname=\"app\"\n",
        )
        .unwrap();
        let cfg = ResolvedConfig {
            project_root: dir.clone(),
            ..Default::default()
        };
        let roots = resolve_package_roots(&cfg).unwrap();
        // Should fall through to monorepo detection because no real
        // pyllow config at root.
        assert_eq!(roots, vec![dir.join("backend").canonicalize().unwrap()]);
    }

    #[test]
    fn root_tool_pyllow_in_pyproject_blocks_monorepo_override() {
        // Same intent as `pyllow.toml`, but expressed via
        // `[tool.pyllow]` inside the root pyproject. Even without
        // `[project]`, the presence of pyllow config means the user
        // wants the root analyzed as the project.
        let tmp = tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::write(
            dir.join("pyproject.toml"),
            "[tool.pyllow]\nentryPoints = [\"main.py\"]\n",
        )
        .unwrap();
        fs::create_dir_all(dir.join("examples")).unwrap();
        fs::write(
            dir.join("examples/pyproject.toml"),
            "[project]\nname=\"example\"\n",
        )
        .unwrap();
        let cfg = ResolvedConfig {
            project_root: dir.clone(),
            ..Default::default()
        };
        let roots = resolve_package_roots(&cfg).unwrap();
        assert_eq!(roots, vec![dir.canonicalize().unwrap()]);
    }
}
