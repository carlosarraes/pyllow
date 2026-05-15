//! Architecture-boundary enforcement.
//!
//! Classifies each `.py` file against the user-declared `[[boundaries.zones]]`
//! (via glob patterns on project-root-relative paths), then walks every import
//! edge in the module graph. For each cross-zone edge, the configured
//! `[[boundaries.rules]]` decide whether the import is permitted; if not,
//! a `Issue::BoundaryViolation` is emitted.
//!
//! Files not matching any zone are **unclassified** and never participate in
//! violations (either as source or target). This intentional carve-out keeps
//! "barrel" `__init__.py` files free to re-export across zone boundaries.

use pyllow_config::{BoundaryConfig, ResolvedRule, ResolvedZone};
use pyllow_graph::{FileRegistry, ModuleGraph};
use pyllow_types::Issue;
use std::path::Path;

pub fn analyze(
    config: &BoundaryConfig,
    graph: &ModuleGraph,
    registry: &FileRegistry,
    project_root: &Path,
) -> Vec<Issue> {
    if config.zones.is_empty() || config.rules.is_empty() {
        return Vec::new();
    }
    let mut issues = Vec::new();
    for edge in &graph.edges {
        let Some(from_node) = registry.get(edge.from) else {
            continue;
        };
        let Some(to_node) = registry.get(edge.to) else {
            continue;
        };
        let Some(from_zone) = classify_zone(&from_node.path, &config.zones, project_root) else {
            continue;
        };
        let Some(to_zone) = classify_zone(&to_node.path, &config.zones, project_root) else {
            continue;
        };
        if from_zone == to_zone {
            // Intra-zone imports are always permitted.
            continue;
        }
        if edge_denied(&config.rules, from_zone, to_zone) {
            issues.push(Issue::BoundaryViolation {
                from_path: from_node.path.clone(),
                from_line: edge.specifier.line,
                from_zone: from_zone.to_string(),
                to_path: to_node.path.clone(),
                to_zone: to_zone.to_string(),
            });
        }
    }
    issues
}

/// First zone whose pattern matches the project-root-relative path. Returns
/// the zone name as a `&str` (no allocation in the hot path).
fn classify_zone<'a>(
    path: &Path,
    zones: &'a [ResolvedZone],
    project_root: &Path,
) -> Option<&'a str> {
    let rel = path.strip_prefix(project_root).unwrap_or(path);
    zones
        .iter()
        .find(|z| z.patterns.iter().any(|p| p.is_match(rel)))
        .map(|z| z.name.as_str())
}

/// Decide whether an edge from `from_zone` to `to_zone` is forbidden by the
/// configured rules. Operates over the *union* of every rule whose `from`
/// matches `from_zone` so that user-declared rules genuinely extend a
/// preset's allowlist rather than being silently overridden by it.
///
/// Semantics:
///   * The edge passes when both checks succeed.
///   * Allow union: if any matching rule has `allow` entries, the target
///     must match at least one allow entry from *any* of those rules.
///   * Deny union: if any matching rule has `deny` entries, the target
///     must not match any deny entry across *all* of those rules.
///   * No matching rules → no constraint → not denied.
fn edge_denied(rules: &[ResolvedRule], from_zone: &str, to_zone: &str) -> bool {
    let matching: Vec<&ResolvedRule> = rules
        .iter()
        .filter(|r| r.from.is_match(from_zone))
        .collect();
    if matching.is_empty() {
        return false;
    }

    let has_any_allow = matching.iter().any(|r| !r.allow.is_empty());
    let allow_ok = !has_any_allow
        || matching
            .iter()
            .flat_map(|r| r.allow.iter())
            .any(|g| g.is_match(to_zone));

    let deny_ok = !matching
        .iter()
        .flat_map(|r| r.deny.iter())
        .any(|g| g.is_match(to_zone));

    !(allow_ok && deny_ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use globset::Glob;
    use pyllow_config::{BoundaryConfig, ResolvedRule, ResolvedZone};
    use std::path::PathBuf;

    fn zone(name: &str, pattern: &str) -> ResolvedZone {
        ResolvedZone {
            name: name.to_string(),
            patterns: vec![Glob::new(pattern).unwrap().compile_matcher()],
        }
    }

    fn rule_allow(from: &str, allow: &[&str]) -> ResolvedRule {
        ResolvedRule {
            from: Glob::new(from).unwrap().compile_matcher(),
            allow: allow
                .iter()
                .map(|p| Glob::new(p).unwrap().compile_matcher())
                .collect(),
            deny: vec![],
        }
    }

    fn rule_deny(from: &str, deny: &[&str]) -> ResolvedRule {
        ResolvedRule {
            from: Glob::new(from).unwrap().compile_matcher(),
            allow: vec![],
            deny: deny
                .iter()
                .map(|p| Glob::new(p).unwrap().compile_matcher())
                .collect(),
        }
    }

    #[test]
    fn classify_returns_matching_zone_name() {
        let zones = vec![
            zone("core", "src/core/**"),
            zone("features", "src/features/**"),
        ];
        let project = PathBuf::from("/proj");
        let core_file = project.join("src/core/foo.py");
        assert_eq!(classify_zone(&core_file, &zones, &project), Some("core"));
        let feat_file = project.join("src/features/auth.py");
        assert_eq!(
            classify_zone(&feat_file, &zones, &project),
            Some("features")
        );
    }

    #[test]
    fn classify_returns_none_for_unclassified() {
        let zones = vec![zone("core", "src/core/**")];
        let project = PathBuf::from("/proj");
        let other = project.join("src/utils/foo.py");
        assert_eq!(classify_zone(&other, &zones, &project), None);
    }

    #[test]
    fn deny_rule_denies_matching_target() {
        let rules = vec![rule_deny("features/*", &["features/*"])];
        assert!(edge_denied(&rules, "features/auth", "features/billing"));
        assert!(!edge_denied(&rules, "features/auth", "shared"));
    }

    #[test]
    fn allow_rule_denies_anything_not_in_whitelist() {
        let rules = vec![rule_allow("features/*", &["shared", "core"])];
        assert!(edge_denied(&rules, "features/auth", "features/billing"));
        assert!(!edge_denied(&rules, "features/auth", "shared"));
        assert!(!edge_denied(&rules, "features/auth", "core"));
    }

    #[test]
    fn empty_rule_constraints_never_deny() {
        let rules = vec![ResolvedRule {
            from: Glob::new("*").unwrap().compile_matcher(),
            allow: vec![],
            deny: vec![],
        }];
        assert!(!edge_denied(&rules, "anything", "anywhere"));
    }

    #[test]
    fn user_allow_rule_extends_preset_allow_rule_via_union() {
        // Pi P2 regression: when a preset declares `allow("entities/*",
        // ["shared"])` and the user adds `allow("entities/*", ["legacy"])`
        // to extend it, both targets must be permitted. The old
        // first-matching-rule logic rejected `entities/* -> legacy`
        // because it tested only the preset rule's whitelist.
        let rules = vec![
            rule_allow("entities/*", &["shared"]),
            rule_allow("entities/*", &["legacy"]),
        ];
        assert!(
            !edge_denied(&rules, "entities/auth", "shared"),
            "preset-allowed target must still pass"
        );
        assert!(
            !edge_denied(&rules, "entities/auth", "legacy"),
            "user-added allow rule must extend the preset's allowlist"
        );
        // And a zone NEITHER rule allows is still rejected.
        assert!(
            edge_denied(&rules, "entities/auth", "forbidden"),
            "zones outside the unified allowlist remain denied"
        );
    }

    #[test]
    fn deny_rule_overrides_allow_when_both_apply() {
        // If matching rules combine allow + deny semantics, denials still
        // win — the union check requires BOTH (in-allow AND not-in-deny).
        let rules = vec![
            rule_allow("entities/*", &["shared"]),
            rule_deny("entities/*", &["shared"]),
        ];
        assert!(edge_denied(&rules, "entities/auth", "shared"));
    }

    #[test]
    fn analyze_returns_empty_when_no_zones_or_rules_configured() {
        // No need to build a real graph — short-circuit at the top.
        let config = BoundaryConfig::default();
        let graph = empty_graph();
        let registry = pyllow_graph::FileRegistry::default();
        let issues = analyze(&config, &graph, &registry, Path::new("/proj"));
        assert!(issues.is_empty());
    }

    /// Smaller integration-style test: build a real graph with two files in
    /// different zones and verify a deny rule emits a violation.
    #[test]
    fn analyze_emits_boundary_violation_for_denied_edge() {
        let project = tempfile::tempdir().unwrap();
        let auth_dir = project.path().join("src/features/auth");
        let billing_dir = project.path().join("src/features/billing");
        std::fs::create_dir_all(&auth_dir).unwrap();
        std::fs::create_dir_all(&billing_dir).unwrap();
        let auth_file = auth_dir.join("main.py");
        let billing_file = billing_dir.join("api.py");
        std::fs::write(&auth_file, "from src.features.billing import api\n").unwrap();
        std::fs::write(&billing_file, "def thing(): pass\n").unwrap();

        let config = BoundaryConfig {
            zones: vec![
                zone("features/auth", "src/features/auth/**"),
                zone("features/billing", "src/features/billing/**"),
            ],
            rules: vec![rule_deny("features/*", &["features/*"])],
        };

        // Build the graph against these two files.
        let (graph, registry) = build_minimal_graph(&[
            (auth_file.clone(), "src.features.auth.main"),
            (billing_file.clone(), "src.features.billing.api"),
        ]);

        let issues = analyze(&config, &graph, &registry, project.path());
        assert_eq!(issues.len(), 1, "got {issues:?}");
        let Issue::BoundaryViolation {
            from_zone,
            to_zone,
            from_line,
            ..
        } = &issues[0]
        else {
            panic!("expected BoundaryViolation, got {:?}", issues[0]);
        };
        assert_eq!(from_zone, "features/auth");
        assert_eq!(to_zone, "features/billing");
        // The import is the first statement of `main.py`, so it lands on line 1.
        assert_eq!(*from_line, 1, "violation should carry the import line");
    }

    #[test]
    fn analyze_does_not_flag_intra_zone_imports() {
        let project = tempfile::tempdir().unwrap();
        let auth_dir = project.path().join("src/features/auth");
        std::fs::create_dir_all(&auth_dir).unwrap();
        let a = auth_dir.join("a.py");
        let b = auth_dir.join("b.py");
        std::fs::write(&a, "from src.features.auth import b\n").unwrap();
        std::fs::write(&b, "def thing(): pass\n").unwrap();

        let config = BoundaryConfig {
            zones: vec![zone("auth", "src/features/auth/**")],
            rules: vec![rule_deny("*", &["*"])],
        };
        let (graph, registry) = build_minimal_graph(&[
            (a.clone(), "src.features.auth.a"),
            (b.clone(), "src.features.auth.b"),
        ]);
        let issues = analyze(&config, &graph, &registry, project.path());
        assert!(issues.is_empty(), "intra-zone import should not violate");
    }

    #[test]
    fn analyze_skips_edges_where_either_end_is_unclassified() {
        let project = tempfile::tempdir().unwrap();
        let auth_dir = project.path().join("src/features/auth");
        let utils_dir = project.path().join("src/utils");
        std::fs::create_dir_all(&auth_dir).unwrap();
        std::fs::create_dir_all(&utils_dir).unwrap();
        let auth = auth_dir.join("main.py");
        let util = utils_dir.join("helper.py");
        std::fs::write(&auth, "from src.utils import helper\n").unwrap();
        std::fs::write(&util, "def f(): pass\n").unwrap();

        // utils/ is unclassified — only `features/auth` zone defined.
        let config = BoundaryConfig {
            zones: vec![zone("features/auth", "src/features/auth/**")],
            rules: vec![rule_deny("*", &["*"])],
        };
        let (graph, registry) = build_minimal_graph(&[
            (auth.clone(), "src.features.auth.main"),
            (util.clone(), "src.utils.helper"),
        ]);
        let issues = analyze(&config, &graph, &registry, project.path());
        assert!(
            issues.is_empty(),
            "edge to unclassified target should be ignored"
        );
    }

    // --- helpers for graph-based tests ---

    fn empty_graph() -> ModuleGraph {
        let registry = pyllow_graph::FileRegistry::default();
        let resolver = pyllow_graph::ModuleResolver::build(&registry);
        ModuleGraph::build(&resolver, &Default::default(), Vec::new())
    }

    fn build_minimal_graph(files: &[(PathBuf, &str)]) -> (ModuleGraph, pyllow_graph::FileRegistry) {
        use rustc_hash::FxHashMap;
        let mut registry = pyllow_graph::FileRegistry::default();
        let mut parsed: FxHashMap<pyllow_types::FileId, pyllow_extract::ParsedModule> =
            FxHashMap::default();
        for (path, dotted) in files {
            let id = registry.register(path.clone(), dotted.to_string());
            let module = pyllow_extract::parse_file(path).expect("parse fixture file");
            parsed.insert(id, module);
        }
        let resolver = pyllow_graph::ModuleResolver::build(&registry);
        // No entry points — the analysis is over the raw edge set.
        let graph = ModuleGraph::build(&resolver, &parsed, Vec::new());
        (graph, registry)
    }
}
