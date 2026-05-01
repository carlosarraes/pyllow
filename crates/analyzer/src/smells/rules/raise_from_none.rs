use pyllow_extract::ast::{self, ExceptHandler, Expr, Stmt};
use pyllow_extract::line_at_offset;
use pyllow_extract::walker::walk_stmts;
use pyllow_types::{Issue, SmellRule};
use std::path::Path;

pub(in crate::smells) fn check(stmts: &[Stmt], source: &str, path: &Path, out: &mut Vec<Issue>) {
    let mut visit = |stmt: &Stmt| {
        let Stmt::Try(t) = stmt else { return };
        for handler in &t.handlers {
            let ExceptHandler::ExceptHandler(h) = handler;
            for inner in &h.body {
                let Stmt::Raise(r) = inner else { continue };
                let Some(cause) = &r.cause else { continue };
                let Expr::Constant(c) = cause.as_ref() else {
                    continue;
                };
                if matches!(c.value, ast::Constant::None) {
                    let line = line_at_offset(source, r.range.start().to_usize());
                    out.push(Issue::Smell {
                        path: path.to_path_buf(),
                        line,
                        rule: SmellRule::RaiseFromNone,
                        detail: "`raise ... from None` discards the original exception cause"
                            .to_string(),
                    });
                }
            }
        }
    };
    walk_stmts(stmts, &mut visit);
}
