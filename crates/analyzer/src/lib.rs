use ignore::WalkBuilder;
use pyllow_config::ResolvedConfig;
use pyllow_extract::{parse_file, ParsedModule};
use pyllow_graph::{dotted_module_for, FileRegistry, ModuleGraph, ModuleResolver};
use pyllow_types::{
    AnalysisResults, AnalysisStats, EntryPoint, EntryPointSource, FileId, Issue, PluginResult,
};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;
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

pub fn analyze(config: &ResolvedConfig) -> Result<AnalysisResults, AnalyzerError> {
    let started = Instant::now();
    let project_root = &config.project_root;
    let package_roots = resolve_package_roots(config);

    let files = discover_python_files(project_root, &package_roots, config);

    let parsed_per_path: Vec<(PathBuf, ParsedModule)> = files
        .par_iter()
        .filter_map(|path| match parse_file(path) {
            Ok(m) => Some((path.clone(), m)),
            Err(e) => {
                eprintln!("warning: skipping {}: {}", path.display(), e);
                None
            }
        })
        .collect();

    let mut registry = FileRegistry::default();
    let mut parsed: FxHashMap<FileId, ParsedModule> = FxHashMap::default();
    for (path, module) in parsed_per_path {
        let dotted = dotted_module_for(&path, &package_roots).unwrap_or_default();
        let id = registry.register(path, dotted);
        parsed.insert(id, module);
    }

    let resolver = ModuleResolver::build(&registry);

    let mut entries = Vec::new();
    let mut plugins_run = Vec::new();

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

    for (id, module) in &parsed {
        let is_dunder_main = registry
            .get(*id)
            .and_then(|n| n.path.file_name())
            .and_then(|s| s.to_str())
            .map(|s| s == "__main__.py")
            .unwrap_or(false);
        if is_dunder_main || module.is_script_entry {
            entries.push(EntryPoint {
                file: *id,
                source: EntryPointSource::ScriptEntryPoint,
            });
        }
    }

    if config
        .plugins
        .get(pyllow_plugin_fastapi::PLUGIN_NAME)
        .map(|c| c.enabled)
        .unwrap_or(false)
    {
        let result = pyllow_plugin_fastapi::discover(&parsed);
        merge_plugin_result(&result, &mut entries);
        plugins_run.push(result.plugin_name);
    }

    if config
        .plugins
        .get(pyllow_plugin_fastmcp::PLUGIN_NAME)
        .map(|c| c.enabled)
        .unwrap_or(false)
    {
        let result = pyllow_plugin_fastmcp::discover(&parsed);
        merge_plugin_result(&result, &mut entries);
        plugins_run.push(result.plugin_name);
    }

    for module in parsed.values_mut() {
        module.suite.clear();
    }

    let graph = ModuleGraph::build(&resolver, &parsed, entries);

    let mut issues = Vec::new();
    for id in graph.unreachable_files(&registry, &resolver) {
        if let Some(node) = registry.get(id) {
            issues.push(Issue::UnusedFile {
                path: node.path.clone(),
            });
        }
    }
    issues.sort_by(|a, b| a.path().cmp(b.path()));

    Ok(AnalysisResults {
        issues,
        stats: AnalysisStats {
            files_scanned: registry.len(),
            entry_points: graph.entry_points.len(),
            plugins_run,
            elapsed_ms: started.elapsed().as_millis() as u64,
        },
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
            .map(|i| match i {
                Issue::UnusedFile { path } => path.file_name().unwrap().to_str().unwrap(),
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
            .map(|i| match i {
                Issue::UnusedFile { path } => path.file_name().unwrap().to_str().unwrap(),
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
            .map(|i| match i {
                Issue::UnusedFile { path } => path.file_name().unwrap().to_str().unwrap(),
            })
            .collect();
        assert_eq!(flagged, vec!["orphan.py"]);
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
