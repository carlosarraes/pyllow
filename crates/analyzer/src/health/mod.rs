//! Function/file complexity, maintainability index, and hotspot scoring.
//!
//! Submodules:
//! - `options` — public `HealthOptions` config + effort classification
//! - `ast` — per-function complexity collection from the AST
//! - `metrics` — Halstead volume, maintainability index, LOC counter
//! - `churn` — git history → file change-count map for hotspot scoring

mod ast;
mod churn;
mod metrics;
mod options;

use ast::{compute_file_health, FileHealth, FunctionHealth};
use options::classify_effort;
pub use options::HealthOptions;

use pyllow_extract::ParsedModule;
use pyllow_types::{FileId, Issue};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};

pub fn analyze(
    parsed: &FxHashMap<FileId, ParsedModule>,
    project_root: &Path,
    opts: HealthOptions,
) -> Vec<Issue> {
    let per_file: Vec<FileHealth> = parsed
        .values()
        .par_bridge()
        .map(compute_file_health)
        .collect();

    let mut issues = Vec::new();
    emit_complexity(&per_file, &opts, &mut issues);
    emit_low_maintainability(&per_file, &opts, &mut issues);
    emit_refactor_targets(&per_file, &opts, &mut issues);
    emit_hotspots(&per_file, project_root, &opts, &mut issues);

    issues.sort_by(|a, b| {
        (a.path(), a.line().unwrap_or(0)).cmp(&(b.path(), b.line().unwrap_or(0)))
    });
    issues
}

/// Pytest test files have inflated LOC (fixtures, parametrize, assertions)
/// paired with low average cyclomatic — exactly the shape MI penalizes,
/// producing systematic false positives. Skip them.
fn is_test_file(fh: &FileHealth) -> bool {
    pyllow_plugin_pytest::is_test_adjacent_path(&fh.path)
}

fn emit_complexity(per_file: &[FileHealth], opts: &HealthOptions, out: &mut Vec<Issue>) {
    if let Some(n) = opts.top {
        let mut ranked: Vec<(&FileHealth, &FunctionHealth)> = per_file
            .iter()
            .flat_map(|fh| fh.functions.iter().map(move |f| (fh, f)))
            .collect();
        ranked.sort_by_key(|(_, f)| std::cmp::Reverse(f.cyclomatic + f.cognitive));
        for (fh, f) in ranked.into_iter().take(n) {
            out.push(complexity_issue(fh, f));
        }
        return;
    }
    for fh in per_file {
        for f in &fh.functions {
            if f.cyclomatic > opts.cyclomatic_threshold || f.cognitive > opts.cognitive_threshold {
                out.push(complexity_issue(fh, f));
            }
        }
    }
}

fn complexity_issue(fh: &FileHealth, f: &FunctionHealth) -> Issue {
    Issue::Complexity {
        path: fh.path.clone(),
        line: f.line,
        function: f.name.clone(),
        cyclomatic: f.cyclomatic,
        cognitive: f.cognitive,
    }
}

fn emit_low_maintainability(
    per_file: &[FileHealth],
    opts: &HealthOptions,
    out: &mut Vec<Issue>,
) {
    for fh in per_file {
        if fh.loc < opts.min_loc_for_mi {
            continue;
        }
        if is_test_file(fh) {
            continue;
        }
        if let Some(mi) = fh.maintainability {
            if mi < opts.maintainability_threshold {
                out.push(Issue::LowMaintainability {
                    path: fh.path.clone(),
                    score: mi,
                    avg_cyclomatic: fh.avg_cyclomatic(),
                    loc: fh.loc,
                });
            }
        }
    }
}

fn emit_refactor_targets(per_file: &[FileHealth], opts: &HealthOptions, out: &mut Vec<Issue>) {
    if !opts.targets {
        return;
    }
    for fh in per_file {
        for f in &fh.functions {
            let Some(effort) = classify_effort(f.cyclomatic, f.cognitive) else {
                continue;
            };
            if let Some(filter) = opts.target_effort {
                if filter != effort {
                    continue;
                }
            }
            out.push(Issue::RefactorTarget {
                path: fh.path.clone(),
                line: f.line,
                function: f.name.clone(),
                cyclomatic: f.cyclomatic,
                cognitive: f.cognitive,
                effort,
            });
        }
    }
}

fn emit_hotspots(
    per_file: &[FileHealth],
    project_root: &Path,
    opts: &HealthOptions,
    out: &mut Vec<Issue>,
) {
    let churn = churn::compute_churn(project_root, per_file);
    let mut hotspots: Vec<(PathBuf, u32, u32, f32)> = per_file
        .iter()
        .filter_map(|fh| {
            let cc = fh.total_cyclomatic;
            if cc == 0 {
                return None;
            }
            let c = *churn.get(fh.path.as_path()).unwrap_or(&0);
            if c < opts.hotspot_min_churn.max(1) {
                return None;
            }
            let score = cc as f32 * ((c as f32 + 1.0).ln());
            Some((fh.path.clone(), cc, c, score))
        })
        .collect();
    hotspots.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
    for (path, cc, c, score) in hotspots.into_iter().take(opts.hotspot_top_n) {
        out.push(Issue::Hotspot {
            path,
            cyclomatic: cc,
            churn: c,
            score,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ast::collect_functions;
    use metrics::{count_loc, maintainability_index};
    use pyllow_extract::parse_source;
    use pyllow_types::Effort;
    use std::path::Path;

    fn module_with(src: &str) -> ParsedModule {
        parse_source(Path::new("/tmp/dummy.py"), src).unwrap()
    }

    #[test]
    fn cyclomatic_simple_function_is_one() {
        let m = module_with("def f():\n    return 1\n");
        let mut funcs = Vec::new();
        for s in &m.suite {
            collect_functions(s, 0, "def f():\n    return 1\n", &mut funcs);
        }
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].cyclomatic, 1);
    }

    #[test]
    fn cyclomatic_counts_decisions() {
        let src = "def f(x):\n    if x > 0:\n        return 1\n    elif x == 0:\n        return 0\n    else:\n        return -1\n";
        let m = module_with(src);
        let mut funcs = Vec::new();
        for s in &m.suite {
            collect_functions(s, 0, src, &mut funcs);
        }
        // 1 (base) + 1 (if) + 1 (elif=else-with-if) = 3
        assert!(funcs[0].cyclomatic >= 3);
    }

    #[test]
    fn cognitive_penalizes_nesting() {
        let src = "def f(x):\n    if x:\n        for i in range(10):\n            if i:\n                pass\n";
        let m = module_with(src);
        let mut funcs = Vec::new();
        for s in &m.suite {
            collect_functions(s, 0, src, &mut funcs);
        }
        // outer if depth 0 (+1), for depth 1 (+2), inner if depth 2 (+3) = cognitive 6
        assert!(funcs[0].cognitive >= 6);
    }

    #[test]
    fn loc_excludes_blanks_and_comments() {
        let src = "# header\n\ndef f():\n    pass\n\n# trailing comment\n";
        assert_eq!(count_loc(src), 2);
    }

    #[test]
    fn mi_clamped_in_range() {
        let mi = maintainability_index("def f(): pass\n", 1.0, 1);
        assert!(mi <= 100);
    }

    fn parsed_map(modules: &[(&str, &str)]) -> FxHashMap<FileId, ParsedModule> {
        modules
            .iter()
            .enumerate()
            .map(|(i, (name, src))| {
                let mut m = parse_source(Path::new(name), src).unwrap();
                m.path = PathBuf::from(name);
                (FileId(i as u32), m)
            })
            .collect()
    }

    #[test]
    fn top_n_returns_n_most_complex_functions_regardless_of_threshold() {
        let parsed = parsed_map(&[
            ("simple.py", "def f():\n    return 1\n"),
            (
                "medium.py",
                "def g(x):\n    if x:\n        return 1\n    elif x == 0:\n        return 0\n    else:\n        return -1\n",
            ),
            (
                "complex.py",
                "def h(x):\n    if x:\n        for i in range(x):\n            if i > 0:\n                if i > 5:\n                    return i\n                else:\n                    return -i\n    return 0\n",
            ),
        ]);
        let opts = HealthOptions {
            top: Some(2),
            ..HealthOptions::default()
        };
        let issues = analyze(&parsed, Path::new("/tmp"), opts);
        let mut complexities: Vec<_> = issues
            .iter()
            .filter_map(|i| match i {
                Issue::Complexity { function, cyclomatic, cognitive, .. } => {
                    Some((function.clone(), *cyclomatic, *cognitive))
                }
                _ => None,
            })
            .collect();
        assert_eq!(complexities.len(), 2);
        complexities.sort_by_key(|(_, cc, cog)| std::cmp::Reverse(*cc + *cog));
        assert_eq!(complexities[0].0, "h");
        assert_eq!(complexities[1].0, "g");
    }

    #[test]
    fn top_n_unset_uses_threshold_filtering() {
        let parsed = parsed_map(&[("a.py", "def f(): return 1\ndef g(): return 2\n")]);
        let issues = analyze(&parsed, Path::new("/tmp"), HealthOptions::default());
        assert!(!issues.iter().any(|i| matches!(i, Issue::Complexity { .. })));
    }

    #[test]
    fn targets_emits_refactor_targets_skipping_trivial_functions() {
        let medium = "def m(x):\n    if x == 0:\n        return 0\n    elif x == 1:\n        return 1\n    elif x == 2:\n        return 2\n    elif x == 3:\n        return 3\n    elif x == 4:\n        return 4\n    elif x == 5:\n        return 5\n    elif x == 6:\n        return 6\n    return -1\n";
        let parsed = parsed_map(&[
            ("simple.py", "def s(): return 1\n"),
            ("medium.py", medium),
        ]);
        let opts = HealthOptions {
            targets: true,
            ..HealthOptions::default()
        };
        let issues = analyze(&parsed, Path::new("/tmp"), opts);
        let targets: Vec<_> = issues
            .iter()
            .filter_map(|i| match i {
                Issue::RefactorTarget { function, .. } => Some(function.clone()),
                _ => None,
            })
            .collect();
        assert!(!targets.is_empty());
        assert!(!targets.iter().any(|name| name == "s"));
        assert!(targets.iter().any(|name| name == "m"));
    }

    #[test]
    fn targets_effort_filter_excludes_other_buckets() {
        let medium = "def m(x):\n    if x == 0:\n        return 0\n    elif x == 1:\n        return 1\n    elif x == 2:\n        return 2\n    elif x == 3:\n        return 3\n    elif x == 4:\n        return 4\n    elif x == 5:\n        return 5\n    elif x == 6:\n        return 6\n    return -1\n";
        let parsed = parsed_map(&[("medium.py", medium)]);
        let opts = HealthOptions {
            targets: true,
            target_effort: Some(Effort::High),
            ..HealthOptions::default()
        };
        let issues = analyze(&parsed, Path::new("/tmp"), opts);
        let high_targets = issues
            .iter()
            .filter(|i| matches!(i, Issue::RefactorTarget { effort: Effort::High, .. }))
            .count();
        let other_targets = issues
            .iter()
            .filter(|i| matches!(i, Issue::RefactorTarget { .. }))
            .count();
        assert_eq!(other_targets, high_targets);
    }
}
