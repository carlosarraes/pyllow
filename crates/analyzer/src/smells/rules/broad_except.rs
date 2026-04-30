use crate::walker::walk_stmts;
use pyllow_extract::ast::{ExceptHandler, Expr, Stmt};
use pyllow_extract::line_at_offset;
use pyllow_types::{Issue, SmellRule};
use std::path::Path;

pub(in crate::smells) fn check(stmts: &[Stmt], source: &str, path: &Path, out: &mut Vec<Issue>) {
    let mut visit = |stmt: &Stmt| {
        let Stmt::Try(t) = stmt else { return };
        for handler in &t.handlers {
            let ExceptHandler::ExceptHandler(h) = handler;
            let is_broad = match &h.type_ {
                None => true,
                Some(e) => matches!(
                    e.as_ref(),
                    Expr::Name(n) if n.id.as_str() == "Exception" || n.id.as_str() == "BaseException"
                ),
            };
            if !is_broad || handler_reraises(&h.body) {
                continue;
            }
            let line = line_at_offset(source, h.range.start().to_usize());
            let kind = if h.type_.is_none() { "bare except" } else { "except Exception" };
            out.push(Issue::Smell {
                path: path.to_path_buf(),
                line,
                rule: SmellRule::BroadExcept,
                detail: format!("{kind} swallows errors silently; catch a specific exception or re-raise"),
            });
        }
    };
    walk_stmts(stmts, &mut visit);
}

fn handler_reraises(body: &[Stmt]) -> bool {
    body.iter().any(|s| matches!(s, Stmt::Raise(_)))
}
