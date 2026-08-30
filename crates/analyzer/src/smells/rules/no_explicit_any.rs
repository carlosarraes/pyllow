//! `no-explicit-any`: flag explicit `typing.Any` in annotations.
//!
//! Syntactic only. Detects `Any` wherever an annotation can appear —
//! parameters, returns, variables, class attributes, type aliases, generic
//! arguments, `Callable` signatures — in direct, qualified, and aliased
//! forms, resolved through the module's imports. Does not follow alias
//! chains across modules, evaluate string annotations, or inspect generated
//! stubs; those need a type checker (see README).

use crate::smells::imports::ImportBindings;
use pyllow_extract::ast::{Expr, Stmt};
use pyllow_extract::line_at_offset;
use pyllow_extract::walker::{walk_annotations, walk_stmts};
use pyllow_types::{Issue, SmellRule};
use std::path::Path;

const ANY: &str = "typing.Any";
const TYPE_ALIAS: &str = "typing.TypeAlias";

pub(in crate::smells) fn check(stmts: &[Stmt], source: &str, path: &Path, out: &mut Vec<Issue>) {
    let bindings = ImportBindings::collect(stmts, |_, _| {});
    let mut report = |expr: &Expr| {
        let range = match expr {
            Expr::Name(n) => n.range,
            Expr::Attribute(a) => a.range,
            _ => return,
        };
        if bindings.resolve(expr).as_deref() == Some(ANY) {
            out.push(Issue::Smell {
                path: path.to_path_buf(),
                line: line_at_offset(source, range.start().to_usize()),
                rule: SmellRule::NoExplicitAny,
                detail: "explicit `Any` annotation discards type evidence".to_string(),
            });
        }
    };

    // Parameters, returns, and annotated assignments.
    walk_annotations(stmts, &mut report);

    // Type alias *values* are annotations in disguise: `X: TypeAlias = ...`
    // and the 3.12 `type X = ...` statement.
    let mut on_stmt = |stmt: &Stmt| match stmt {
        Stmt::AnnAssign(a) => {
            if bindings.resolve(&a.annotation).as_deref() == Some(TYPE_ALIAS) {
                if let Some(value) = &a.value {
                    walk_type_expr(value, &mut report);
                }
            }
        }
        Stmt::TypeAlias(t) => walk_type_expr(&t.value, &mut report),
        _ => {}
    };
    walk_stmts(stmts, &mut on_stmt);
}

/// Recurse through the expression shapes a type expression can take.
fn walk_type_expr(expr: &Expr, visit: &mut impl FnMut(&Expr)) {
    visit(expr);
    match expr {
        Expr::Subscript(s) => {
            walk_type_expr(&s.value, visit);
            walk_type_expr(&s.slice, visit);
        }
        Expr::Tuple(t) => t.elts.iter().for_each(|e| walk_type_expr(e, visit)),
        Expr::List(l) => l.elts.iter().for_each(|e| walk_type_expr(e, visit)),
        Expr::BinOp(b) => {
            walk_type_expr(&b.left, visit);
            walk_type_expr(&b.right, visit);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_extract::parse_source;
    use pyllow_types::SmellRule;
    use std::path::PathBuf;

    fn lines(src: &str) -> Vec<u32> {
        let path = PathBuf::from("/tmp/t.py");
        let module = parse_source(&path, src).unwrap();
        let mut out = Vec::new();
        check(&module.suite, src, &path, &mut out);
        out.iter()
            .map(|i| match i {
                Issue::Smell { line, rule, .. } => {
                    assert_eq!(*rule, SmellRule::NoExplicitAny);
                    *line
                }
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    const IMP: &str = "from typing import Any\n";

    #[test]
    fn parameter_and_return_annotations() {
        let src = format!("{IMP}\ndef f(x: Any) -> Any:\n    return x\n");
        assert_eq!(lines(&src), vec![3, 3]);
    }

    #[test]
    fn variable_and_class_attribute_annotations() {
        let src = format!("{IMP}\nx: Any = 1\n\nclass C:\n    y: Any\n");
        assert_eq!(lines(&src), vec![3, 6]);
    }

    #[test]
    fn generic_arguments_and_callable_signatures() {
        let src = "from typing import Any, Callable\n\ndef f(a: list[Any], b: dict[str, Any]) -> Callable[[Any], Any]:\n    ...\n";
        assert_eq!(lines(src), vec![3, 3, 3, 3]);
    }

    #[test]
    fn protocol_members_are_checked() {
        let src = "from typing import Any, Protocol\n\nclass P(Protocol):\n    def run(self, x: Any) -> None: ...\n";
        assert_eq!(lines(src), vec![4]);
    }

    #[test]
    fn type_alias_annotation_form() {
        let src = "from typing import Any, TypeAlias\n\nJson: TypeAlias = dict[str, Any]\n";
        assert_eq!(lines(src), vec![3]);
    }

    #[test]
    fn type_alias_statement_form_py312() {
        let src = format!("{IMP}\ntype Json = dict[str, Any]\n");
        assert_eq!(lines(&src), vec![3]);
    }

    #[test]
    fn qualified_and_aliased_forms() {
        let src = "import typing\nimport typing as t\nfrom typing import Any as Dynamic\n\ndef f(a: typing.Any, b: t.Any, c: Dynamic): ...\n";
        assert_eq!(lines(src), vec![5, 5, 5]);
    }

    #[test]
    fn union_syntax_py310() {
        let src = format!("{IMP}\ndef f(x: int | Any) -> None: ...\n");
        assert_eq!(lines(&src), vec![3]);
    }

    // ---- negative ----

    #[test]
    fn object_unions_protocols_and_typevars_are_not_flagged() {
        let src = "from typing import Protocol, TypeVar, Union, Optional\n\nT = TypeVar('T')\n\nclass P(Protocol):\n    def run(self) -> object: ...\n\ndef f(a: object, b: int | str, c: Union[int, str], d: Optional[int], e: T) -> T:\n    return e\n";
        assert!(lines(src).is_empty());
    }

    #[test]
    fn unannotated_code_is_not_flagged() {
        let src = "def f(x):\n    y = x\n    return y\n";
        assert!(lines(src).is_empty());
    }

    #[test]
    fn local_name_any_without_import_is_not_flagged() {
        let src = "class Any: ...\n\ndef f(x: Any) -> Any: ...\n";
        assert!(lines(src).is_empty());
    }

    #[test]
    fn any_used_as_a_value_not_an_annotation_is_not_flagged() {
        let src = format!("{IMP}\nprint(Any)\nx = [Any]\n");
        assert!(lines(&src).is_empty());
    }

    #[test]
    fn import_line_itself_is_not_flagged() {
        assert!(lines(IMP).is_empty());
    }
}
