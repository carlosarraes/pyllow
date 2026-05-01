use pyllow_extract::walker::{body_contains_yield, stmt_range_start, walk_stmts};
use pyllow_extract::ast::{ExceptHandler, Stmt};
use pyllow_extract::line_at_offset;
use pyllow_types::{Issue, SmellRule};
use std::path::Path;

pub(in crate::smells) fn check(stmts: &[Stmt], source: &str, path: &Path, out: &mut Vec<Issue>) {
    scan_block(stmts, source, path, out);
    let mut visit = |stmt: &Stmt| match stmt {
        // For functions, skip the scan if the body contains `yield` — generator
        // functions legitimately follow `raise` with `yield` to declare async
        // generator-ness without ever yielding at runtime.
        Stmt::FunctionDef(f) => {
            if !body_contains_yield(&f.body) {
                scan_block(&f.body, source, path, out);
            }
        }
        Stmt::AsyncFunctionDef(f) => {
            if !body_contains_yield(&f.body) {
                scan_block(&f.body, source, path, out);
            }
        }
        Stmt::ClassDef(c) => scan_block(&c.body, source, path, out),
        Stmt::If(s) => {
            scan_block(&s.body, source, path, out);
            scan_block(&s.orelse, source, path, out);
        }
        Stmt::While(s) => scan_block(&s.body, source, path, out),
        Stmt::For(s) => scan_block(&s.body, source, path, out),
        Stmt::AsyncFor(s) => scan_block(&s.body, source, path, out),
        Stmt::Try(s) => {
            scan_block(&s.body, source, path, out);
            for ExceptHandler::ExceptHandler(h) in &s.handlers {
                scan_block(&h.body, source, path, out);
            }
            scan_block(&s.orelse, source, path, out);
            scan_block(&s.finalbody, source, path, out);
        }
        Stmt::With(s) => scan_block(&s.body, source, path, out),
        Stmt::AsyncWith(s) => scan_block(&s.body, source, path, out),
        Stmt::Match(s) => {
            for case in &s.cases {
                scan_block(&case.body, source, path, out);
            }
        }
        _ => {}
    };
    walk_stmts(stmts, &mut visit);
}

fn scan_block(block: &[Stmt], source: &str, path: &Path, out: &mut Vec<Issue>) {
    if block.len() < 2 {
        return;
    }
    for i in 0..block.len() - 1 {
        if is_terminator(&block[i]) {
            let next = &block[i + 1];
            // Allow a trailing single `pass` (some linters require it).
            if matches!(next, Stmt::Pass(_)) && i + 1 == block.len() - 1 {
                return;
            }
            let line = line_at_offset(source, stmt_range_start(next));
            out.push(Issue::Smell {
                path: path.to_path_buf(),
                line,
                rule: SmellRule::UnreachableAfterExit,
                detail: "code after return/raise/break/continue is unreachable".to_string(),
            });
            return; // only flag first dead statement per block
        }
    }
}

fn is_terminator(stmt: &Stmt) -> bool {
    matches!(
        stmt,
        Stmt::Return(_) | Stmt::Raise(_) | Stmt::Break(_) | Stmt::Continue(_)
    )
}
