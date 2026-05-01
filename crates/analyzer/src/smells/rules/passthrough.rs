use pyllow_extract::ast::{self, Expr, Stmt};
use pyllow_extract::line_at_offset;
use pyllow_extract::walker::walk_stmts;
use pyllow_types::{Issue, SmellRule};
use std::path::Path;

pub(in crate::smells) fn check(stmts: &[Stmt], source: &str, path: &Path, out: &mut Vec<Issue>) {
    let mut visit = |stmt: &Stmt| {
        let (name, args, body, range_start) = match stmt {
            Stmt::FunctionDef(f) => (
                f.name.as_str(),
                &f.args,
                &f.body,
                f.range.start().to_usize(),
            ),
            Stmt::AsyncFunctionDef(f) => (
                f.name.as_str(),
                &f.args,
                &f.body,
                f.range.start().to_usize(),
            ),
            _ => return,
        };
        if body.len() != 1 {
            return;
        }
        let Stmt::Return(r) = &body[0] else { return };
        let Some(value) = &r.value else { return };
        let Expr::Call(call) = value.as_ref() else {
            return;
        };
        // Skip method-style calls (self.foo / module.foo) — likely intentional.
        if matches!(call.func.as_ref(), Expr::Attribute(_)) {
            return;
        }
        if !args_match(args, call) {
            return;
        }
        let line = line_at_offset(source, range_start);
        out.push(Issue::Smell {
            path: path.to_path_buf(),
            line,
            rule: SmellRule::PassthroughFunction,
            detail: format!("`{name}` only forwards arguments; consider removing the wrapper"),
        });
    };
    walk_stmts(stmts, &mut visit);
}

fn args_match(func_args: &ast::Arguments, call: &ast::ExprCall) -> bool {
    // Conservative: only flag plain positional passthrough. Reject if the
    // function or the call use *args, **kwargs, or keyword-only args.
    if func_args.vararg.is_some() || func_args.kwarg.is_some() {
        return false;
    }
    if !func_args.kwonlyargs.is_empty() || !call.keywords.is_empty() {
        return false;
    }
    let positional: Vec<&str> = func_args
        .posonlyargs
        .iter()
        .chain(func_args.args.iter())
        .map(|a| a.def.arg.as_str())
        .collect();
    if positional.is_empty() || positional.len() != call.args.len() {
        return false;
    }
    for (i, arg) in call.args.iter().enumerate() {
        let Expr::Name(n) = arg else { return false };
        if n.id.as_str() != positional[i] {
            return false;
        }
    }
    true
}
