use pyllow_extract::walker::walk_stmts;
use pyllow_extract::ast::{Expr, Ranged, Stmt};
use pyllow_extract::line_at_offset;
use pyllow_types::{Issue, SmellRule};
use std::path::Path;

pub(in crate::smells) fn check(stmts: &[Stmt], source: &str, path: &Path, out: &mut Vec<Issue>) {
    let mut visit = |stmt: &Stmt| {
        let args = match stmt {
            Stmt::FunctionDef(f) => Some(&f.args),
            Stmt::AsyncFunctionDef(f) => Some(&f.args),
            _ => None,
        };
        let Some(args) = args else { return };
        for arg in args
            .posonlyargs
            .iter()
            .chain(args.args.iter())
            .chain(args.kwonlyargs.iter())
        {
            let Some(default) = &arg.default else { continue };
            if is_mutable_literal(default) {
                let line = line_at_offset(source, default.range().start().to_usize());
                out.push(Issue::Smell {
                    path: path.to_path_buf(),
                    line,
                    rule: SmellRule::MutableDefault,
                    detail: format!(
                        "argument `{}` has mutable default; use None and assign in body",
                        arg.def.arg.as_str()
                    ),
                });
            }
        }
    };
    walk_stmts(stmts, &mut visit);
}

fn is_mutable_literal(expr: &Expr) -> bool {
    use Expr::*;
    match expr {
        List(_) | Dict(_) | Set(_) => true,
        Call(c) => matches!(
            c.func.as_ref(),
            Name(n) if matches!(n.id.as_str(), "list" | "dict" | "set")
        ),
        _ => false,
    }
}
