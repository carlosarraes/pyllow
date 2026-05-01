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

    pub fn resolve_dotted(&self, dotted: &str) -> Option<FileId> {
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
                    // `if TYPE_CHECKING:` imports never run, so they
                    // mustn't keep a module reachable. Without this guard,
                    // `if TYPE_CHECKING: import orphan` would mask
                    // `orphan.py` as a live file. Try/except-fallback
                    // imports stay because they do execute when the
                    // primary import fails.
                    if spec.is_type_only {
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

    /// Returns strongly connected components of size ≥ 2.
    ///
    /// Self-loops are not reported because [`ModuleGraph::build`] discards
    /// `from == to` edges. Each returned `Vec<FileId>` is one cycle, ordered
    /// by traversal — call sites that need stable output should sort the
    /// component before display.
    ///
    /// Implementation: iterative Tarjan to avoid recursion depth limits on
    /// large codebases (~20k+ files).
    pub fn strongly_connected_components(&self, registry: &FileRegistry) -> Vec<Vec<FileId>> {
        // No edges → no cycles. Skip Tarjan setup entirely; on a single-file
        // package this avoids ~200KB of upfront allocation.
        if self.edges_by_source.is_empty() {
            return Vec::new();
        }
        let mut state = TarjanState::new(registry.len());
        for id in registry.all_ids() {
            if !state.indices.contains_key(&id) {
                state.run_from(id, &self.edges_by_source);
            }
        }
        state
            .components
            .into_iter()
            .filter(|c| c.len() >= 2)
            .collect()
    }
}

/// Iterative Tarjan SCC. Visits nodes via an explicit work stack so deep
/// import chains don't overflow the call stack.
struct TarjanState {
    next_index: u32,
    indices: FxHashMap<FileId, u32>,
    lowlinks: FxHashMap<FileId, u32>,
    on_stack: FxHashSet<FileId>,
    stack: Vec<FileId>,
    components: Vec<Vec<FileId>>,
}

impl TarjanState {
    fn new(capacity: usize) -> Self {
        Self {
            next_index: 0,
            indices: FxHashMap::with_capacity_and_hasher(capacity, Default::default()),
            lowlinks: FxHashMap::with_capacity_and_hasher(capacity, Default::default()),
            on_stack: FxHashSet::with_capacity_and_hasher(capacity, Default::default()),
            stack: Vec::with_capacity(capacity),
            components: Vec::new(),
        }
    }

    fn run_from(&mut self, start: FileId, edges: &FxHashMap<FileId, Vec<FileId>>) {
        // Each work-stack frame remembers the node and its next-neighbor cursor.
        let mut work: Vec<(FileId, usize)> = Vec::new();
        self.assign_index(start);
        work.push((start, 0));

        while let Some(&(v, cursor)) = work.last() {
            let neighbors = edges.get(&v).map(|n| n.as_slice()).unwrap_or(&[]);
            if cursor < neighbors.len() {
                // Advance cursor before recursing so the resume step picks up the next neighbor.
                work.last_mut().unwrap().1 = cursor + 1;
                let w = neighbors[cursor];
                if !self.indices.contains_key(&w) {
                    self.assign_index(w);
                    work.push((w, 0));
                } else if self.on_stack.contains(&w) {
                    let w_index = self.indices[&w];
                    let v_low = self.lowlinks.get_mut(&v).unwrap();
                    if w_index < *v_low {
                        *v_low = w_index;
                    }
                }
            } else {
                // Finished v: if it's an SCC root, pop the component.
                if self.lowlinks[&v] == self.indices[&v] {
                    let mut component = Vec::new();
                    while let Some(node) = self.stack.pop() {
                        self.on_stack.remove(&node);
                        component.push(node);
                        if node == v {
                            break;
                        }
                    }
                    self.components.push(component);
                }
                work.pop();
                // Propagate v's lowlink to its parent (the new top of the work stack).
                if let Some(&(parent, _)) = work.last() {
                    let v_low = self.lowlinks[&v];
                    let parent_low = self.lowlinks.get_mut(&parent).unwrap();
                    if v_low < *parent_low {
                        *parent_low = v_low;
                    }
                }
            }
        }
    }

    fn assign_index(&mut self, v: FileId) {
        let i = self.next_index;
        self.next_index += 1;
        self.indices.insert(v, i);
        self.lowlinks.insert(v, i);
        self.stack.push(v);
        self.on_stack.insert(v);
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
    // Reject any segment that isn't a valid Python identifier prefix —
    // hidden dirs (`.github`), digits-first names, etc. would produce
    // module paths Python itself would never accept.
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
    if segments.iter().any(|s| !is_python_identifier(s)) {
        return String::new();
    }
    segments.join(".")
}

fn is_python_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
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
            is_type_only: false,
        }
    }

    fn module(path: &str, imports: Vec<ImportSpecifier>) -> ParsedModule {
        ParsedModule {
            path: PathBuf::from(path),
            imports,
            exports: vec![],
            suite: Vec::new(),
            is_script_entry: false,
            has_module_getattr: false,
            unused_imports: Vec::new(),
            source: String::new(),
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
    fn dotted_rejects_segments_that_arent_python_identifiers() {
        // `.github/actions/people/people.py` would otherwise produce the
        // invalid module path `.github.actions.people.people`.
        let dotted = dotted_module_for(
            &PathBuf::from(".github/actions/people/people.py"),
            &[PathBuf::from("")],
        )
        .unwrap();
        assert!(dotted.is_empty());

        // Digits-first segments (`12factor.py`) are also invalid identifiers.
        let dotted =
            dotted_module_for(&PathBuf::from("src/12factor.py"), &[PathBuf::from("src")]).unwrap();
        assert!(dotted.is_empty());
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
        let helper = reg.register(PathBuf::from("src/myapp/helper.py"), "myapp.helper".into());
        let resolver = ModuleResolver::build(&reg);
        let s = spec("myapp.helper", ImportKind::Absolute);
        assert_eq!(resolver.resolve(&s, main), Some(helper));
    }

    #[test]
    fn resolves_relative_import_from_module() {
        let mut reg = FileRegistry::default();
        let main = reg.register(PathBuf::from("src/myapp/main.py"), "myapp.main".into());
        let helper = reg.register(PathBuf::from("src/myapp/helper.py"), "myapp.helper".into());
        let resolver = ModuleResolver::build(&reg);
        let s = spec("helper", ImportKind::Relative { level: 1 });
        assert_eq!(resolver.resolve(&s, main), Some(helper));
    }

    #[test]
    fn resolves_relative_import_from_init() {
        let mut reg = FileRegistry::default();
        let init = reg.register(PathBuf::from("src/myapp/__init__.py"), "myapp".into());
        let helper = reg.register(PathBuf::from("src/myapp/helper.py"), "myapp.helper".into());
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
        let sibling = reg.register(PathBuf::from("src/myapp/db.py"), "myapp.db".into());
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
        let used = reg.register(PathBuf::from("src/app/helper.py"), "app.helper".into());
        let orphan = reg.register(PathBuf::from("src/app/orphan.py"), "app.orphan".into());

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
    fn scc_empty_graph_is_empty() {
        let reg = FileRegistry::default();
        let resolver = ModuleResolver::build(&reg);
        let graph = ModuleGraph::build(&resolver, &FxHashMap::default(), vec![]);
        assert!(graph.strongly_connected_components(&reg).is_empty());
    }

    #[test]
    fn scc_acyclic_chain_returns_no_cycles() {
        let mut reg = FileRegistry::default();
        let a = reg.register(PathBuf::from("a.py"), "a".into());
        let b = reg.register(PathBuf::from("b.py"), "b".into());
        let _c = reg.register(PathBuf::from("c.py"), "c".into());
        let resolver = ModuleResolver::build(&reg);
        let mut parsed = FxHashMap::default();
        parsed.insert(a, module("a.py", vec![spec("b", ImportKind::Absolute)]));
        parsed.insert(b, module("b.py", vec![spec("c", ImportKind::Absolute)]));
        parsed.insert(_c, module("c.py", vec![]));
        let graph = ModuleGraph::build(&resolver, &parsed, vec![]);
        assert!(graph.strongly_connected_components(&reg).is_empty());
    }

    #[test]
    fn scc_two_node_cycle_detected() {
        let mut reg = FileRegistry::default();
        let a = reg.register(PathBuf::from("a.py"), "a".into());
        let b = reg.register(PathBuf::from("b.py"), "b".into());
        let resolver = ModuleResolver::build(&reg);
        let mut parsed = FxHashMap::default();
        parsed.insert(a, module("a.py", vec![spec("b", ImportKind::Absolute)]));
        parsed.insert(b, module("b.py", vec![spec("a", ImportKind::Absolute)]));
        let graph = ModuleGraph::build(&resolver, &parsed, vec![]);
        let sccs = graph.strongly_connected_components(&reg);
        assert_eq!(sccs.len(), 1);
        let component: FxHashSet<_> = sccs[0].iter().copied().collect();
        assert_eq!(component, FxHashSet::from_iter([a, b]));
    }

    #[test]
    fn scc_three_node_cycle_detected() {
        let mut reg = FileRegistry::default();
        let a = reg.register(PathBuf::from("a.py"), "a".into());
        let b = reg.register(PathBuf::from("b.py"), "b".into());
        let c = reg.register(PathBuf::from("c.py"), "c".into());
        let resolver = ModuleResolver::build(&reg);
        let mut parsed = FxHashMap::default();
        parsed.insert(a, module("a.py", vec![spec("b", ImportKind::Absolute)]));
        parsed.insert(b, module("b.py", vec![spec("c", ImportKind::Absolute)]));
        parsed.insert(c, module("c.py", vec![spec("a", ImportKind::Absolute)]));
        let graph = ModuleGraph::build(&resolver, &parsed, vec![]);
        let sccs = graph.strongly_connected_components(&reg);
        assert_eq!(sccs.len(), 1);
        assert_eq!(sccs[0].len(), 3);
    }

    #[test]
    fn scc_disjoint_cycles_each_reported() {
        let mut reg = FileRegistry::default();
        let a = reg.register(PathBuf::from("a.py"), "a".into());
        let b = reg.register(PathBuf::from("b.py"), "b".into());
        let c = reg.register(PathBuf::from("c.py"), "c".into());
        let d = reg.register(PathBuf::from("d.py"), "d".into());
        let resolver = ModuleResolver::build(&reg);
        let mut parsed = FxHashMap::default();
        parsed.insert(a, module("a.py", vec![spec("b", ImportKind::Absolute)]));
        parsed.insert(b, module("b.py", vec![spec("a", ImportKind::Absolute)]));
        parsed.insert(c, module("c.py", vec![spec("d", ImportKind::Absolute)]));
        parsed.insert(d, module("d.py", vec![spec("c", ImportKind::Absolute)]));
        let graph = ModuleGraph::build(&resolver, &parsed, vec![]);
        let sccs = graph.strongly_connected_components(&reg);
        assert_eq!(sccs.len(), 2);
        assert!(sccs.iter().all(|c| c.len() == 2));
    }

    #[test]
    fn scc_acyclic_neighbor_does_not_pollute_cycle() {
        // Only {b, c} form a cycle; a points into the cycle but isn't part of it.
        let mut reg = FileRegistry::default();
        let a = reg.register(PathBuf::from("a.py"), "a".into());
        let b = reg.register(PathBuf::from("b.py"), "b".into());
        let c = reg.register(PathBuf::from("c.py"), "c".into());
        let resolver = ModuleResolver::build(&reg);
        let mut parsed = FxHashMap::default();
        parsed.insert(a, module("a.py", vec![spec("b", ImportKind::Absolute)]));
        parsed.insert(b, module("b.py", vec![spec("c", ImportKind::Absolute)]));
        parsed.insert(c, module("c.py", vec![spec("b", ImportKind::Absolute)]));
        let graph = ModuleGraph::build(&resolver, &parsed, vec![]);
        let sccs = graph.strongly_connected_components(&reg);
        assert_eq!(sccs.len(), 1);
        let component: FxHashSet<_> = sccs[0].iter().copied().collect();
        assert_eq!(component, FxHashSet::from_iter([b, c]));
    }

    fn type_only_spec(raw: &str) -> ImportSpecifier {
        ImportSpecifier {
            raw: raw.to_string(),
            kind: ImportKind::Absolute,
            is_conditional: true,
            is_type_only: true,
        }
    }

    #[test]
    fn type_only_imports_do_not_keep_modules_reachable() {
        // `if TYPE_CHECKING: import orphan` must NOT make `orphan.py`
        // reachable — the import literally never runs at runtime.
        let mut reg = FileRegistry::default();
        let a = reg.register(PathBuf::from("a.py"), "a".into());
        let orphan = reg.register(PathBuf::from("orphan.py"), "orphan".into());
        let resolver = ModuleResolver::build(&reg);
        let mut parsed = FxHashMap::default();
        parsed.insert(a, module("a.py", vec![type_only_spec("orphan")]));
        parsed.insert(orphan, module("orphan.py", vec![]));
        let entries = vec![EntryPoint {
            file: a,
            source: pyllow_types::EntryPointSource::Config,
        }];
        let graph = ModuleGraph::build(&resolver, &parsed, entries);
        let unreachable = graph.unreachable_files(&reg, &resolver);
        assert!(unreachable.contains(&orphan));
    }

    #[test]
    fn try_fallback_imports_still_keep_modules_reachable() {
        // `try: import optional except ImportError: ...` is_conditional
        // but NOT type_only — the import does run at runtime when the
        // module exists, so `optional.py` must stay reachable.
        let mut reg = FileRegistry::default();
        let a = reg.register(PathBuf::from("a.py"), "a".into());
        let optional = reg.register(PathBuf::from("optional.py"), "optional".into());
        let resolver = ModuleResolver::build(&reg);
        let conditional_runtime = ImportSpecifier {
            raw: "optional".to_string(),
            kind: ImportKind::Absolute,
            is_conditional: true,
            is_type_only: false,
        };
        let mut parsed = FxHashMap::default();
        parsed.insert(a, module("a.py", vec![conditional_runtime]));
        parsed.insert(optional, module("optional.py", vec![]));
        let entries = vec![EntryPoint {
            file: a,
            source: pyllow_types::EntryPointSource::Config,
        }];
        let graph = ModuleGraph::build(&resolver, &parsed, entries);
        let unreachable = graph.unreachable_files(&reg, &resolver);
        assert!(!unreachable.contains(&optional));
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
