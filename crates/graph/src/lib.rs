use pyllow_extract::ParsedModule;
use pyllow_types::{Edge, EntryPoint, FileId, ImportKind, ImportSpecifier, ModuleKind, ModuleNode};
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GraphError {
    #[error("file path is not under any package root: {0}")]
    PathOutsideRoots(PathBuf),
}

#[derive(Debug, Default)]
pub struct FileRegistry {
    next_id: u32,
    by_path: FxHashMap<PathBuf, FileId>,
    nodes: FxHashMap<FileId, ModuleNode>,
    dotted: FxHashMap<FileId, String>,
}

impl FileRegistry {
    pub fn register(&mut self, path: PathBuf, dotted_module: String) -> FileId {
        if let Some(&id) = self.by_path.get(&path) {
            return id;
        }
        let id = FileId(self.next_id);
        self.next_id += 1;
        let kind = if path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s == "__init__.py")
            .unwrap_or(false)
        {
            ModuleKind::PackageInit
        } else {
            ModuleKind::Module
        };
        self.nodes.insert(
            id,
            ModuleNode {
                id,
                path: path.clone(),
                kind,
            },
        );
        self.dotted.insert(id, dotted_module);
        self.by_path.insert(path, id);
        id
    }

    pub fn get(&self, id: FileId) -> Option<&ModuleNode> {
        self.nodes.get(&id)
    }

    pub fn dotted_of(&self, id: FileId) -> Option<&str> {
        self.dotted.get(&id).map(|s| s.as_str())
    }

    pub fn id_for(&self, path: &Path) -> Option<FileId> {
        self.by_path.get(path).copied()
    }

    pub fn all_ids(&self) -> impl Iterator<Item = FileId> + '_ {
        self.nodes.keys().copied()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

pub struct ModuleResolver<'a> {
    registry: &'a FileRegistry,
    by_dotted: FxHashMap<String, FileId>,
}

impl<'a> ModuleResolver<'a> {
    pub fn build(registry: &'a FileRegistry) -> Self {
        let mut by_dotted = FxHashMap::default();
        for (id, dotted) in &registry.dotted {
            if !dotted.is_empty() {
                by_dotted.insert(dotted.clone(), *id);
            }
        }
        Self {
            registry,
            by_dotted,
        }
    }

    pub fn resolve(&self, spec: &ImportSpecifier, from: FileId) -> Option<FileId> {
        match spec.kind {
            ImportKind::Absolute => self.resolve_dotted(&spec.raw),
            ImportKind::Relative { level } => {
                let from_dotted = self.registry.dotted_of(from)?;
                let from_kind = self.registry.get(from)?.kind;
                let base = relative_base(from_dotted, from_kind, level)?;
                let candidate = if spec.raw.is_empty() {
                    base
                } else if base.is_empty() {
                    spec.raw.clone()
                } else {
                    format!("{}.{}", base, spec.raw)
                };
                self.resolve_dotted(&candidate)
            }
            ImportKind::DynamicLiteral => self.resolve_dotted(&spec.raw),
            ImportKind::DynamicOpaque => None,
        }
    }

    fn resolve_dotted(&self, dotted: &str) -> Option<FileId> {
        self.by_dotted.get(dotted).copied()
    }
}

fn mark_parent_packages(
    id: FileId,
    registry: &FileRegistry,
    resolver: &ModuleResolver<'_>,
    reached: &mut FxHashSet<FileId>,
) {
    let Some(dotted) = registry.dotted_of(id) else {
        return;
    };
    let mut prefix = dotted;
    while let Some(idx) = prefix.rfind('.') {
        prefix = &prefix[..idx];
        if let Some(pkg_id) = resolver.resolve_dotted(prefix) {
            reached.insert(pkg_id);
        }
    }
}

fn relative_base(from_dotted: &str, from_kind: ModuleKind, level: u32) -> Option<String> {
    let segments: Vec<&str> = from_dotted.split('.').filter(|s| !s.is_empty()).collect();
    let package_segments: Vec<&str> = match from_kind {
        ModuleKind::PackageInit | ModuleKind::NamespacePackage => segments.clone(),
        ModuleKind::Module => {
            if segments.is_empty() {
                return None;
            }
            segments[..segments.len() - 1].to_vec()
        }
    };
    let drop = (level as usize).saturating_sub(1);
    if drop > package_segments.len() {
        return None;
    }
    let kept = &package_segments[..package_segments.len() - drop];
    Some(kept.join("."))
}

#[derive(Debug, Default)]
pub struct ModuleGraph {
    pub edges: Vec<Edge>,
    pub entry_points: FxHashSet<FileId>,
    pub entry_point_meta: Vec<EntryPoint>,
    edges_by_source: FxHashMap<FileId, Vec<FileId>>,
}

impl ModuleGraph {
    pub fn build(
        resolver: &ModuleResolver<'_>,
        parsed: &FxHashMap<FileId, ParsedModule>,
        entries: Vec<EntryPoint>,
    ) -> Self {
        let mut edges = Vec::new();
        let mut edges_by_source: FxHashMap<FileId, Vec<FileId>> = FxHashMap::default();

        for (&from_id, module) in parsed {
            for spec in &module.imports {
                if let Some(to_id) = resolver.resolve(spec, from_id) {
                    if to_id == from_id {
                        continue;
                    }
                    edges.push(Edge {
                        from: from_id,
                        to: to_id,
                        specifier: spec.clone(),
                    });
                    edges_by_source.entry(from_id).or_default().push(to_id);
                }
            }
        }

        let entry_points: FxHashSet<FileId> = entries.iter().map(|e| e.file).collect();

        Self {
            edges,
            entry_points,
            entry_point_meta: entries,
            edges_by_source,
        }
    }

    pub fn reachable_from_entries(&self) -> FxHashSet<FileId> {
        let mut reached = FxHashSet::default();
        let mut frontier: Vec<FileId> = self.entry_points.iter().copied().collect();
        while let Some(id) = frontier.pop() {
            if !reached.insert(id) {
                continue;
            }
            if let Some(neighbors) = self.edges_by_source.get(&id) {
                for &n in neighbors {
                    if !reached.contains(&n) {
                        frontier.push(n);
                    }
                }
            }
        }
        reached
    }

    pub fn unreachable_files(
        &self,
        registry: &FileRegistry,
        resolver: &ModuleResolver<'_>,
    ) -> Vec<FileId> {
        let mut reached = self.reachable_from_entries();
        let initial: Vec<FileId> = reached.iter().copied().collect();
        for id in initial {
            mark_parent_packages(id, registry, resolver, &mut reached);
        }
        registry
            .all_ids()
            .filter(|id| !reached.contains(id))
            .collect()
    }
}

pub fn dotted_module_for(path: &Path, package_roots: &[PathBuf]) -> Option<String> {
    for root in package_roots {
        if let Ok(rel) = path.strip_prefix(root) {
            return Some(rel_to_dotted(rel));
        }
    }
    None
}

fn rel_to_dotted(rel: &Path) -> String {
    let mut segments: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => s.to_str().map(String::from),
            _ => None,
        })
        .collect();
    if let Some(last) = segments.last_mut() {
        if let Some(stripped) = last.strip_suffix(".py") {
            *last = stripped.to_string();
        }
    }
    if segments.last().map(|s| s.as_str()) == Some("__init__") {
        segments.pop();
    }
    segments.join(".")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_extract::ParsedModule;

    fn spec(raw: &str, kind: ImportKind) -> ImportSpecifier {
        ImportSpecifier {
            raw: raw.to_string(),
            kind,
            is_conditional: false,
        }
    }

    fn module(path: &str, imports: Vec<ImportSpecifier>) -> ParsedModule {
        ParsedModule {
            path: PathBuf::from(path),
            imports,
            exports: vec![],
            suite: Vec::new(),
            is_script_entry: false,
        }
    }

    #[test]
    fn dotted_for_module_file() {
        let dotted =
            dotted_module_for(&PathBuf::from("src/myapp/foo.py"), &[PathBuf::from("src")]).unwrap();
        assert_eq!(dotted, "myapp.foo");
    }

    #[test]
    fn dotted_for_init_file() {
        let dotted = dotted_module_for(
            &PathBuf::from("src/myapp/__init__.py"),
            &[PathBuf::from("src")],
        )
        .unwrap();
        assert_eq!(dotted, "myapp");
    }

    #[test]
    fn dotted_returns_none_outside_roots() {
        assert!(dotted_module_for(
            &PathBuf::from("vendor/external.py"),
            &[PathBuf::from("src")]
        )
        .is_none());
    }

    #[test]
    fn resolves_absolute_import() {
        let mut reg = FileRegistry::default();
        let main = reg.register(PathBuf::from("src/myapp/main.py"), "myapp.main".into());
        let helper =
            reg.register(PathBuf::from("src/myapp/helper.py"), "myapp.helper".into());
        let resolver = ModuleResolver::build(&reg);
        let s = spec("myapp.helper", ImportKind::Absolute);
        assert_eq!(resolver.resolve(&s, main), Some(helper));
    }

    #[test]
    fn resolves_relative_import_from_module() {
        let mut reg = FileRegistry::default();
        let main = reg.register(PathBuf::from("src/myapp/main.py"), "myapp.main".into());
        let helper =
            reg.register(PathBuf::from("src/myapp/helper.py"), "myapp.helper".into());
        let resolver = ModuleResolver::build(&reg);
        let s = spec("helper", ImportKind::Relative { level: 1 });
        assert_eq!(resolver.resolve(&s, main), Some(helper));
    }

    #[test]
    fn resolves_relative_import_from_init() {
        let mut reg = FileRegistry::default();
        let init = reg.register(
            PathBuf::from("src/myapp/__init__.py"),
            "myapp".into(),
        );
        let helper =
            reg.register(PathBuf::from("src/myapp/helper.py"), "myapp.helper".into());
        let resolver = ModuleResolver::build(&reg);
        let s = spec("helper", ImportKind::Relative { level: 1 });
        assert_eq!(resolver.resolve(&s, init), Some(helper));
    }

    #[test]
    fn resolves_double_dot_relative() {
        let mut reg = FileRegistry::default();
        let inner = reg.register(
            PathBuf::from("src/myapp/api/users.py"),
            "myapp.api.users".into(),
        );
        let sibling = reg.register(
            PathBuf::from("src/myapp/db.py"),
            "myapp.db".into(),
        );
        let resolver = ModuleResolver::build(&reg);
        let s = spec("db", ImportKind::Relative { level: 2 });
        assert_eq!(resolver.resolve(&s, inner), Some(sibling));
    }

    #[test]
    fn unresolved_imports_skipped() {
        let mut reg = FileRegistry::default();
        let main = reg.register(PathBuf::from("a.py"), "a".into());
        let resolver = ModuleResolver::build(&reg);
        let s = spec("not.real", ImportKind::Absolute);
        assert_eq!(resolver.resolve(&s, main), None);
    }

    #[test]
    fn reachability_marks_orphan_unreachable() {
        let mut reg = FileRegistry::default();
        let main = reg.register(PathBuf::from("src/app/main.py"), "app.main".into());
        let used =
            reg.register(PathBuf::from("src/app/helper.py"), "app.helper".into());
        let orphan =
            reg.register(PathBuf::from("src/app/orphan.py"), "app.orphan".into());

        let resolver = ModuleResolver::build(&reg);
        let mut parsed = FxHashMap::default();
        parsed.insert(
            main,
            module(
                "src/app/main.py",
                vec![spec("app.helper", ImportKind::Absolute)],
            ),
        );
        parsed.insert(used, module("src/app/helper.py", vec![]));
        parsed.insert(orphan, module("src/app/orphan.py", vec![]));

        let entries = vec![EntryPoint {
            file: main,
            source: pyllow_types::EntryPointSource::Config,
        }];
        let graph = ModuleGraph::build(&resolver, &parsed, entries);

        let unreachable = graph.unreachable_files(&reg, &resolver);
        assert_eq!(unreachable, vec![orphan]);
    }

    #[test]
    fn reachability_traverses_transitively() {
        let mut reg = FileRegistry::default();
        let a = reg.register(PathBuf::from("a.py"), "a".into());
        let b = reg.register(PathBuf::from("b.py"), "b".into());
        let c = reg.register(PathBuf::from("c.py"), "c".into());
        let resolver = ModuleResolver::build(&reg);
        let mut parsed = FxHashMap::default();
        parsed.insert(a, module("a.py", vec![spec("b", ImportKind::Absolute)]));
        parsed.insert(b, module("b.py", vec![spec("c", ImportKind::Absolute)]));
        parsed.insert(c, module("c.py", vec![]));
        let entries = vec![EntryPoint {
            file: a,
            source: pyllow_types::EntryPointSource::Config,
        }];
        let graph = ModuleGraph::build(&resolver, &parsed, entries);
        assert!(graph.unreachable_files(&reg, &resolver).is_empty());
    }
}
