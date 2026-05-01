use pyllow_extract::walker::walk_stmts_for_exprs;
use pyllow_extract::ast::{self, Expr, Ranged, Stmt};
use pyllow_extract::line_at_offset;
use pyllow_types::{Issue, SmellRule};
use std::path::Path;

pub(in crate::smells) fn check(stmts: &[Stmt], source: &str, path: &Path, out: &mut Vec<Issue>) {
    let mut visit_expr = |expr: &Expr| {
        let Expr::Compare(cmp) = expr else { return };
        // Skip ORM filter expressions like `Model.attr == None`. SQLAlchemy /
        // Beanie / Django ORM all overload `==` to build query predicates, so
        // `is None` is not a valid replacement there. Heuristic: LHS is an
        // attribute access on a Name whose first letter is uppercase (PEP 8
        // class-name convention).
        if let Expr::Attribute(attr) = &cmp.left.as_ref() {
            if let Expr::Name(base) = attr.value.as_ref() {
                let first = base.id.as_str().chars().next();
                if matches!(first, Some(c) if c.is_ascii_uppercase()) {
                    return;
                }
            }
        }
        for (op, comparator) in cmp.ops.iter().zip(cmp.comparators.iter()) {
            if !matches!(op, ast::CmpOp::Eq | ast::CmpOp::NotEq) {
                continue;
            }
            let Expr::Constant(c) = comparator else { continue };
            let (matched, hint) = match &c.value {
                ast::Constant::Bool(b) => (
                    true,
                    format!("compare against {} via truthy/falsy: `if x` or `if not x`", b),
                ),
                ast::Constant::None => (
                    true,
                    "use `is None` / `is not None` for None checks".to_string(),
                ),
                _ => (false, String::new()),
            };
            if matched {
                let line = line_at_offset(source, comparator.range().start().to_usize());
                out.push(Issue::Smell {
                    path: path.to_path_buf(),
                    line,
                    rule: SmellRule::SentinelEquality,
                    detail: hint,
                });
            }
        }
    };
    walk_stmts_for_exprs(stmts, &mut visit_expr);
}
