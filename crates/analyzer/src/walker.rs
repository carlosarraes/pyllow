//! Generic AST traversal helpers shared by analyzers.
//!
//! Each caller supplies a closure that runs against every statement (or every
//! expression) and decides whether to flag the node. The walkers handle the
//! recursion through Python's compound statement forms (functions, classes,
//! control flow, try/except, with, match) so call sites stay leaf-only.

use pyllow_extract::ast::{self, ExceptHandler, Expr, Stmt};

pub(crate) fn walk_stmts(stmts: &[Stmt], visit: &mut impl FnMut(&Stmt)) {
    for s in stmts {
        visit(s);
        match s {
            Stmt::FunctionDef(f) => walk_stmts(&f.body, visit),
            Stmt::AsyncFunctionDef(f) => walk_stmts(&f.body, visit),
            Stmt::ClassDef(c) => walk_stmts(&c.body, visit),
            Stmt::If(s) => {
                walk_stmts(&s.body, visit);
                walk_stmts(&s.orelse, visit);
            }
            Stmt::While(s) => {
                walk_stmts(&s.body, visit);
                walk_stmts(&s.orelse, visit);
            }
            Stmt::For(s) => {
                walk_stmts(&s.body, visit);
                walk_stmts(&s.orelse, visit);
            }
            Stmt::AsyncFor(s) => {
                walk_stmts(&s.body, visit);
                walk_stmts(&s.orelse, visit);
            }
            Stmt::Try(s) => {
                walk_stmts(&s.body, visit);
                for ExceptHandler::ExceptHandler(h) in &s.handlers {
                    walk_stmts(&h.body, visit);
                }
                walk_stmts(&s.orelse, visit);
                walk_stmts(&s.finalbody, visit);
            }
            Stmt::With(s) => walk_stmts(&s.body, visit),
            Stmt::AsyncWith(s) => walk_stmts(&s.body, visit),
            Stmt::Match(s) => {
                for case in &s.cases {
                    walk_stmts(&case.body, visit);
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn walk_stmts_for_exprs(stmts: &[Stmt], visit: &mut impl FnMut(&Expr)) {
    let mut on_stmt = |s: &Stmt| {
        for_each_expr_in_stmt(s, visit);
    };
    walk_stmts(stmts, &mut on_stmt);
}

fn for_each_expr_in_stmt(stmt: &Stmt, visit: &mut impl FnMut(&Expr)) {
    match stmt {
        Stmt::Expr(e) => walk_expr(&e.value, visit),
        Stmt::Assign(a) => {
            for t in &a.targets {
                walk_expr(t, visit);
            }
            walk_expr(&a.value, visit);
        }
        Stmt::AnnAssign(a) => {
            walk_expr(&a.target, visit);
            walk_expr(&a.annotation, visit);
            if let Some(v) = &a.value {
                walk_expr(v, visit);
            }
        }
        Stmt::AugAssign(a) => {
            walk_expr(&a.target, visit);
            walk_expr(&a.value, visit);
        }
        Stmt::Return(r) => {
            if let Some(v) = &r.value {
                walk_expr(v, visit);
            }
        }
        Stmt::If(s) => walk_expr(&s.test, visit),
        Stmt::While(s) => walk_expr(&s.test, visit),
        Stmt::For(s) => {
            walk_expr(&s.iter, visit);
            walk_expr(&s.target, visit);
        }
        Stmt::AsyncFor(s) => {
            walk_expr(&s.iter, visit);
            walk_expr(&s.target, visit);
        }
        Stmt::Raise(r) => {
            if let Some(e) = &r.exc {
                walk_expr(e, visit);
            }
            if let Some(c) = &r.cause {
                walk_expr(c, visit);
            }
        }
        Stmt::Assert(a) => {
            walk_expr(&a.test, visit);
            if let Some(m) = &a.msg {
                walk_expr(m, visit);
            }
        }
        _ => {}
    }
}

fn walk_expr(expr: &Expr, visit: &mut impl FnMut(&Expr)) {
    visit(expr);
    use ast::Expr::*;
    match expr {
        BoolOp(b) => {
            for v in &b.values {
                walk_expr(v, visit);
            }
        }
        BinOp(b) => {
            walk_expr(&b.left, visit);
            walk_expr(&b.right, visit);
        }
        UnaryOp(u) => walk_expr(&u.operand, visit),
        Lambda(l) => walk_expr(&l.body, visit),
        IfExp(i) => {
            walk_expr(&i.test, visit);
            walk_expr(&i.body, visit);
            walk_expr(&i.orelse, visit);
        }
        Compare(c) => {
            walk_expr(&c.left, visit);
            for r in &c.comparators {
                walk_expr(r, visit);
            }
        }
        Call(c) => {
            walk_expr(&c.func, visit);
            for a in &c.args {
                walk_expr(a, visit);
            }
            for kw in &c.keywords {
                walk_expr(&kw.value, visit);
            }
        }
        Attribute(a) => walk_expr(&a.value, visit),
        Subscript(s) => {
            walk_expr(&s.value, visit);
            walk_expr(&s.slice, visit);
        }
        Starred(s) => walk_expr(&s.value, visit),
        Tuple(t) => {
            for e in &t.elts {
                walk_expr(e, visit);
            }
        }
        List(l) => {
            for e in &l.elts {
                walk_expr(e, visit);
            }
        }
        Set(s) => {
            for e in &s.elts {
                walk_expr(e, visit);
            }
        }
        Dict(d) => {
            for k in d.keys.iter().flatten() {
                walk_expr(k, visit);
            }
            for v in &d.values {
                walk_expr(v, visit);
            }
        }
        ListComp(c) => {
            walk_expr(&c.elt, visit);
            walk_comprehensions(&c.generators, visit);
        }
        SetComp(c) => {
            walk_expr(&c.elt, visit);
            walk_comprehensions(&c.generators, visit);
        }
        DictComp(c) => {
            walk_expr(&c.key, visit);
            walk_expr(&c.value, visit);
            walk_comprehensions(&c.generators, visit);
        }
        GeneratorExp(c) => {
            walk_expr(&c.elt, visit);
            walk_comprehensions(&c.generators, visit);
        }
        Await(a) => walk_expr(&a.value, visit),
        Yield(y) => {
            if let Some(v) = &y.value {
                walk_expr(v, visit);
            }
        }
        YieldFrom(y) => walk_expr(&y.value, visit),
        NamedExpr(n) => {
            walk_expr(&n.target, visit);
            walk_expr(&n.value, visit);
        }
        FormattedValue(f) => walk_expr(&f.value, visit),
        JoinedStr(j) => {
            for v in &j.values {
                walk_expr(v, visit);
            }
        }
        Slice(s) => {
            if let Some(e) = &s.lower {
                walk_expr(e, visit);
            }
            if let Some(e) = &s.upper {
                walk_expr(e, visit);
            }
            if let Some(e) = &s.step {
                walk_expr(e, visit);
            }
        }
        _ => {}
    }
}

fn walk_comprehensions(
    generators: &[ast::Comprehension],
    visit: &mut impl FnMut(&Expr),
) {
    for gen in generators {
        walk_expr(&gen.iter, visit);
        walk_expr(&gen.target, visit);
        for cond in &gen.ifs {
            walk_expr(cond, visit);
        }
    }
}

pub(crate) fn stmt_range_start(stmt: &Stmt) -> usize {
    use Stmt::*;
    match stmt {
        FunctionDef(s) => s.range.start().to_usize(),
        AsyncFunctionDef(s) => s.range.start().to_usize(),
        ClassDef(s) => s.range.start().to_usize(),
        Return(s) => s.range.start().to_usize(),
        Delete(s) => s.range.start().to_usize(),
        Assign(s) => s.range.start().to_usize(),
        AugAssign(s) => s.range.start().to_usize(),
        AnnAssign(s) => s.range.start().to_usize(),
        For(s) => s.range.start().to_usize(),
        AsyncFor(s) => s.range.start().to_usize(),
        While(s) => s.range.start().to_usize(),
        If(s) => s.range.start().to_usize(),
        With(s) => s.range.start().to_usize(),
        AsyncWith(s) => s.range.start().to_usize(),
        Match(s) => s.range.start().to_usize(),
        Raise(s) => s.range.start().to_usize(),
        Try(s) => s.range.start().to_usize(),
        TryStar(s) => s.range.start().to_usize(),
        Assert(s) => s.range.start().to_usize(),
        Import(s) => s.range.start().to_usize(),
        ImportFrom(s) => s.range.start().to_usize(),
        Global(s) => s.range.start().to_usize(),
        Nonlocal(s) => s.range.start().to_usize(),
        Expr(s) => s.range.start().to_usize(),
        Pass(s) => s.range.start().to_usize(),
        Break(s) => s.range.start().to_usize(),
        Continue(s) => s.range.start().to_usize(),
        TypeAlias(s) => s.range.start().to_usize(),
    }
}

pub(crate) fn body_contains_yield(body: &[Stmt]) -> bool {
    let mut found = false;
    let mut on_expr = |e: &Expr| {
        if matches!(e, Expr::Yield(_) | Expr::YieldFrom(_)) {
            found = true;
        }
    };
    walk_stmts_for_exprs(body, &mut on_expr);
    found
}
