//! `[[smells.banned_api]]`: flag uses of fully qualified Python APIs a
//! project prohibits.
//!
//! Resolution is syntactic and import-based: every name bound by an
//! `import` / `from … import` in the module is mapped back to its qualified
//! origin, then every `Name` and `Attribute` chain is resolved through that
//! map and compared for an exact match. No inference is attempted for names
//! that are not import-bound, so a local `def cast()` never matches
//! `typing.cast`, and relative imports (whose absolute target depends on the
//! package layout) are deliberately left unresolved.

use crate::smells::imports::ImportBindings;
use pyllow_extract::ast::{Expr, Stmt};
use pyllow_extract::line_at_offset;
use pyllow_extract::walker::walk_stmts_for_exprs;
use pyllow_types::{BannedApi, Issue};
use std::path::Path;

pub(in crate::smells) fn check(
    stmts: &[Stmt],
    source: &str,
    path: &Path,
    banned: &[BannedApi],
    out: &mut Vec<Issue>,
) {
    if banned.is_empty() {
        return;
    }
    let start = out.len();

    // Pass 1: bindings, flagging any import that names a banned path outright.
    let bindings = ImportBindings::collect(stmts, |qualified, stmt| {
        let range = match stmt {
            Stmt::Import(s) => s.range,
            Stmt::ImportFrom(s) => s.range,
            _ => return,
        };
        report_if_banned(qualified, range, source, path, banned, out);
    });

    // Pass 2: resolve every Name / Attribute chain through the map.
    let mut on_expr = |expr: &Expr| {
        let Some(resolved) = bindings.resolve(expr) else {
            return;
        };
        let range = match expr {
            Expr::Name(n) => n.range,
            Expr::Attribute(a) => a.range,
            _ => return,
        };
        report_if_banned(&resolved, range, source, path, banned, out);
    };
    walk_stmts_for_exprs(stmts, &mut on_expr);

    // A `Name` that also appears as the root of an `Attribute` is visited
    // twice by the walker (once for the chain, once for the leaf). Keep the
    // first finding per (id, range) so each site reports once.
    let mut seen = rustc_hash::FxHashSet::default();
    let tail: Vec<Issue> = out.drain(start..).collect();
    for issue in tail {
        let key = match &issue {
            Issue::BannedApi {
                id, line, end_line, ..
            } => (id.clone(), *line, *end_line),
            _ => unreachable!("banned_api only emits BannedApi"),
        };
        if seen.insert(key) {
            out.push(issue);
        }
    }
}

fn report_if_banned(
    qualified: &str,
    range: pyllow_extract::ast::text_size::TextRange,
    source: &str,
    path: &Path,
    banned: &[BannedApi],
    out: &mut Vec<Issue>,
) {
    for rule in banned {
        if rule.path == qualified {
            out.push(Issue::BannedApi {
                path: path.to_path_buf(),
                line: line_at_offset(source, range.start().to_usize()),
                end_line: line_at_offset(source, range.end().to_usize()),
                id: rule.id.clone(),
                api: qualified.to_string(),
                message: rule.message.clone(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_extract::parse_source;
    use std::path::PathBuf;

    fn ban(id: &str, path: &str) -> BannedApi {
        BannedApi {
            id: id.into(),
            path: path.into(),
            message: format!("{id} is banned"),
        }
    }

    /// (id, api, start_line, end_line) for every finding.
    fn findings(src: &str, banned: &[BannedApi]) -> Vec<(String, String, u32, u32)> {
        let path = PathBuf::from("/tmp/t.py");
        let module = parse_source(&path, src).unwrap();
        let mut out = Vec::new();
        check(&module.suite, src, &path, banned, &mut out);
        out.iter()
            .map(|i| match i {
                Issue::BannedApi {
                    id,
                    api,
                    line,
                    end_line,
                    ..
                } => (id.clone(), api.clone(), *line, *end_line),
                other => panic!("unexpected issue {other:?}"),
            })
            .collect()
    }

    fn lines(src: &str, banned: &[BannedApi]) -> Vec<u32> {
        findings(src, banned).into_iter().map(|f| f.2).collect()
    }

    const CAST: &str = "typing.cast";
    const PATCH: &str = "unittest.mock.patch";

    // ---- positive: typing.cast ----

    #[test]
    fn direct_import_flags_import_and_usage() {
        let src = "from typing import cast\n\ny = cast(int, x)\n";
        assert_eq!(lines(src, &[ban("no-cast", CAST)]), vec![1, 3]);
    }

    #[test]
    fn qualified_access_flags_usage_not_the_module_import() {
        let src = "import typing\n\ny = typing.cast(int, x)\n";
        assert_eq!(lines(src, &[ban("no-cast", CAST)]), vec![3]);
    }

    #[test]
    fn aliased_from_import_is_resolved() {
        let src = "from typing import cast as c\n\ny = c(int, x)\n";
        assert_eq!(lines(src, &[ban("no-cast", CAST)]), vec![1, 3]);
    }

    #[test]
    fn aliased_module_import_is_resolved() {
        let src = "import typing as t\n\ny = t.cast(int, x)\n";
        assert_eq!(lines(src, &[ban("no-cast", CAST)]), vec![3]);
    }

    // ---- positive: unittest.mock.patch ----

    #[test]
    fn dotted_module_import_resolves_qualified_call() {
        let src = "import unittest.mock\n\nwith unittest.mock.patch('a.b'):\n    pass\n";
        assert_eq!(lines(src, &[ban("no-patch", PATCH)]), vec![3]);
    }

    #[test]
    fn submodule_from_import_resolves_attribute_access() {
        let src = "from unittest import mock\n\nmock.patch('a.b')\n";
        assert_eq!(lines(src, &[ban("no-patch", PATCH)]), vec![3]);
    }

    #[test]
    fn decorator_usage_is_flagged() {
        let src = "from unittest.mock import patch\n\n@patch('a.b')\ndef test_x(m):\n    pass\n";
        assert_eq!(lines(src, &[ban("no-patch", PATCH)]), vec![1, 3]);
    }

    #[test]
    fn attribute_on_banned_name_still_flags_the_banned_root() {
        // `patch.object` is not the banned path, but `patch` itself is.
        let src = "from unittest.mock import patch\n\npatch.object(X, 'y')\n";
        assert_eq!(lines(src, &[ban("no-patch", PATCH)]), vec![1, 3]);
    }

    // ---- reporting ----

    #[test]
    fn reports_configured_id_and_resolved_api() {
        let src = "import typing as t\ny = t.cast(int, x)\n";
        let f = findings(src, &[ban("no-typing-cast", CAST)]);
        assert_eq!(f, vec![("no-typing-cast".into(), CAST.into(), 2, 2)]);
    }

    #[test]
    fn message_is_carried_from_config() {
        let src = "from typing import cast\n";
        let path = PathBuf::from("/tmp/t.py");
        let module = parse_source(&path, src).unwrap();
        let mut out = Vec::new();
        let mut b = ban("no-cast", CAST);
        b.message = "Prefer parsing.".into();
        check(&module.suite, src, &path, &[b], &mut out);
        assert!(
            matches!(&out[0], Issue::BannedApi { message, .. } if message == "Prefer parsing.")
        );
    }

    #[test]
    fn multiline_call_range_covers_only_the_referencing_expression() {
        let src = "import typing\ny = typing.cast(\n    int,\n    x,\n)\n";
        let f = findings(src, &[ban("no-cast", CAST)]);
        assert_eq!((f[0].2, f[0].3), (2, 2));
    }

    #[test]
    fn multiple_rules_report_independently() {
        let src = "from typing import cast\nfrom unittest.mock import patch\n";
        let ids: Vec<String> = findings(src, &[ban("a", CAST), ban("b", PATCH)])
            .into_iter()
            .map(|f| f.0)
            .collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    // ---- negative ----

    #[test]
    fn sibling_api_from_same_module_is_not_flagged() {
        let src = "from unittest.mock import create_autospec\n\nm = create_autospec(Svc)\n";
        assert!(lines(src, &[ban("no-patch", PATCH)]).is_empty());
    }

    #[test]
    fn local_function_with_same_name_is_not_flagged() {
        let src = "def cast(t, v):\n    return v\n\ny = cast(int, x)\n";
        assert!(lines(src, &[ban("no-cast", CAST)]).is_empty());
    }

    #[test]
    fn unrelated_attribute_with_same_name_is_not_flagged() {
        let src = "import typing\nobj.cast(int, x)\nself.patch()\n";
        assert!(lines(src, &[ban("no-cast", CAST), ban("no-patch", PATCH)]).is_empty());
    }

    #[test]
    fn environment_monkeypatch_is_not_flagged() {
        let src = "def test_x(monkeypatch):\n    monkeypatch.setenv('A', '1')\n    monkeypatch.setattr(obj, 'x', 1)\n";
        assert!(lines(src, &[ban("no-patch", PATCH)]).is_empty());
    }

    #[test]
    fn dependency_injection_and_protocol_fakes_are_not_flagged() {
        let src = "from typing import Protocol\n\nclass Clock(Protocol):\n    def now(self) -> int: ...\n\nclass FakeClock:\n    def now(self) -> int:\n        return 0\n\ndef run(clock: Clock) -> int:\n    return clock.now()\n";
        assert!(lines(src, &[ban("no-cast", CAST), ban("no-patch", PATCH)]).is_empty());
    }

    #[test]
    fn typed_narrowing_is_not_flagged() {
        let src = "def f(x: object) -> int:\n    if isinstance(x, int):\n        return x\n    raise TypeError\n";
        assert!(lines(src, &[ban("no-cast", CAST)]).is_empty());
    }

    #[test]
    fn relative_imports_are_left_unresolved() {
        // `.typing.cast` could be anything; refusing to guess avoids a false
        // positive on a project-local `typing` package.
        let src = "from .typing import cast\ny = cast(int, x)\n";
        assert!(lines(src, &[ban("no-cast", CAST)]).is_empty());
    }

    #[test]
    fn module_import_alone_is_not_a_use_of_a_member() {
        let src = "import typing\nimport unittest.mock\n";
        assert!(lines(src, &[ban("no-cast", CAST), ban("no-patch", PATCH)]).is_empty());
    }
}
