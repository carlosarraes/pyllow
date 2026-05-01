use pyllow_extract::walker::walk_stmts_for_exprs;
use pyllow_extract::ast::{self, Expr, Stmt};
use pyllow_extract::line_at_offset;
use pyllow_types::{Issue, SmellRule};
use std::path::Path;

pub(in crate::smells) fn check(stmts: &[Stmt], source: &str, path: &Path, out: &mut Vec<Issue>) {
    let mut visit_expr = |expr: &Expr| {
        let Expr::Compare(cmp) = expr else { return };
        if cmp.ops.len() != 1 || cmp.comparators.len() != 1 {
            return;
        }
        let op = &cmp.ops[0];
        if !matches!(
            op,
            ast::CmpOp::Gt | ast::CmpOp::GtE | ast::CmpOp::Eq | ast::CmpOp::NotEq
        ) {
            return;
        }
        if !is_len_call(&cmp.left) {
            return;
        }
        let Expr::Constant(c) = &cmp.comparators[0] else { return };
        let ast::Constant::Int(n) = &c.value else { return };
        if n.to_string() != "0" {
            return;
        }
        let line = line_at_offset(source, cmp.range.start().to_usize());
        out.push(Issue::Smell {
            path: path.to_path_buf(),
            line,
            rule: SmellRule::TruthyLengthCheck,
            detail: "use `if x:` / `if not x:` instead of `len(x) > 0` / `len(x) == 0`".to_string(),
        });
    };
    walk_stmts_for_exprs(stmts, &mut visit_expr);
}

fn is_len_call(expr: &Expr) -> bool {
    let Expr::Call(c) = expr else { return false };
    matches!(c.func.as_ref(), Expr::Name(n) if n.id.as_str() == "len")
}
