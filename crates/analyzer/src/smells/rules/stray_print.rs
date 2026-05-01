use pyllow_extract::walker::walk_stmts_for_exprs;
use pyllow_extract::ast::{Expr, Stmt};
use pyllow_extract::line_at_offset;
use pyllow_types::{Issue, SmellRule};
use std::path::Path;

pub(in crate::smells) fn check(stmts: &[Stmt], source: &str, path: &Path, out: &mut Vec<Issue>) {
    let mut visit_expr = |expr: &Expr| {
        let Expr::Call(c) = expr else { return };
        let Expr::Name(n) = c.func.as_ref() else { return };
        if n.id.as_str() != "print" {
            return;
        }
        let line = line_at_offset(source, c.range.start().to_usize());
        out.push(Issue::Smell {
            path: path.to_path_buf(),
            line,
            rule: SmellRule::StrayPrint,
            detail: "stray print() outside __main__ guard; use logging instead".to_string(),
        });
    };
    walk_stmts_for_exprs(stmts, &mut visit_expr);
}
