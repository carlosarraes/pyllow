//! Surface module-import cycles as `Issue::CircularDependency`.
//!
//! Reuses [`pyllow_graph::ModuleGraph::strongly_connected_components`] which
//! returns SCCs of size ≥ 2. Each component becomes one issue listing every
//! file in the cycle.

use pyllow_graph::{FileRegistry, ModuleGraph};
use pyllow_types::Issue;

pub fn analyze(graph: &ModuleGraph, registry: &FileRegistry) -> Vec<Issue> {
    let mut issues = Vec::new();
    for component in graph.strongly_connected_components(registry) {
        let mut cycle: Vec<_> = component
            .iter()
            .filter_map(|id| registry.get(*id).map(|n| n.path.clone()))
            .collect();
        // Sort for stable output; the original Tarjan ordering reflects DFS,
        // which is harder to reason about across runs.
        cycle.sort();
        if cycle.len() < 2 {
            continue;
        }
        issues.push(Issue::CircularDependency { cycle });
    }
    issues
}
