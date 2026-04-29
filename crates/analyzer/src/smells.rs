use pyllow_extract::ast::{self, ExceptHandler, Expr, Ranged, Stmt};
use pyllow_extract::{line_at_offset, ParsedModule};
use pyllow_types::{FileId, Issue, SmellRule};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SmellsOptions {
    pub disabled: FxHashSet<SmellRule>,
    pub todo_density_threshold: u32,
}

impl Default for SmellsOptions {
    fn default() -> Self {
        Self {
            disabled: FxHashSet::default(),
            todo_density_threshold: 5,
        }
    }
}

pub fn analyze(
    parsed: &FxHashMap<FileId, ParsedModule>,
    opts: &SmellsOptions,
) -> Vec<Issue> {
    parsed
        .values()
        .par_bridge()
        .flat_map(|m| analyze_module(m, opts))
        .collect()
}

fn analyze_module(module: &ParsedModule, opts: &SmellsOptions) -> Vec<Issue> {
    let source = module.source.as_str();
    let path = module.path.clone();
    let mut issues = Vec::new();
    let enabled = |r: SmellRule| !opts.disabled.contains(&r);

    if enabled(SmellRule::MutableDefault) {
        check_mutable_defaults(&module.suite, source, &path, &mut issues);
    }
    if enabled(SmellRule::BroadExcept) {
        check_broad_except(&module.suite, source, &path, &mut issues);
    }
    if enabled(SmellRule::SentinelEquality) {
        check_sentinel_equality(&module.suite, source, &path, &mut issues);
    }
    if enabled(SmellRule::TruthyLengthCheck) {
        check_truthy_length(&module.suite, source, &path, &mut issues);
    }
    if enabled(SmellRule::UnreachableAfterExit) {
        check_unreachable(&module.suite, source, &path, &mut issues);
    }
    if enabled(SmellRule::PassthroughFunction) {
        check_passthrough(&module.suite, source, &path, &mut issues);
    }
    if enabled(SmellRule::StrayPrint) && !module.is_script_entry {
        check_stray_print(&module.suite, source, &path, &mut issues);
    }
    if enabled(SmellRule::SingleMethodClass) {
        check_single_method_class(&module.suite, source, &path, &mut issues);
    }
    if enabled(SmellRule::HighTodoDensity) {
        check_todo_density(source, &path, opts.todo_density_threshold, &mut issues);
    }
    if enabled(SmellRule::RaiseFromNone) {
        check_raise_from_none(&module.suite, source, &path, &mut issues);
    }
    issues
}

// ============================================================================
// Rule 1: mutable default arguments
// ============================================================================

fn check_mutable_defaults(stmts: &[Stmt], source: &str, path: &Path, out: &mut Vec<Issue>) {
    let mut visit = |stmt: &Stmt| {
        let args = match stmt {
            Stmt::FunctionDef(f) => Some(&f.args),
            Stmt::AsyncFunctionDef(f) => Some(&f.args),
            _ => None,
        };
        let Some(args) = args else { return };
        for arg in args.posonlyargs.iter().chain(args.args.iter()).chain(args.kwonlyargs.iter()) {
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
    use ast::Expr::*;
    match expr {
        List(_) | Dict(_) | Set(_) => true,
        Call(c) => matches!(
            c.func.as_ref(),
            Name(n) if matches!(n.id.as_str(), "list" | "dict" | "set")
        ),
        _ => false,
    }
}

// ============================================================================
// Rule 2: bare or broad except clauses without re-raise
// ============================================================================

fn check_broad_except(stmts: &[Stmt], source: &str, path: &Path, out: &mut Vec<Issue>) {
    let mut visit = |stmt: &Stmt| {
        let Stmt::Try(t) = stmt else { return };
        for handler in &t.handlers {
            let ExceptHandler::ExceptHandler(h) = handler;
            let is_broad = match &h.type_ {
                None => true, // bare except:
                Some(e) => matches!(
                    e.as_ref(),
                    Expr::Name(n) if n.id.as_str() == "Exception" || n.id.as_str() == "BaseException"
                ),
            };
            if !is_broad {
                continue;
            }
            if handler_reraises(&h.body) {
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

// ============================================================================
// Rule 3: sentinel equality (== True / == False / == None / != None)
// ============================================================================

fn check_sentinel_equality(stmts: &[Stmt], source: &str, path: &Path, out: &mut Vec<Issue>) {
    let mut visit_expr = |expr: &Expr| {
        let Expr::Compare(cmp) = expr else { return };
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
                ast::Constant::None => (true, "use `is None` / `is not None` for None checks".to_string()),
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

// ============================================================================
// Rule 4: truthy-length check (len(x) > 0 / == 0)
// ============================================================================

fn check_truthy_length(stmts: &[Stmt], source: &str, path: &Path, out: &mut Vec<Issue>) {
    let mut visit_expr = |expr: &Expr| {
        let Expr::Compare(cmp) = expr else { return };
        if cmp.ops.len() != 1 || cmp.comparators.len() != 1 {
            return;
        }
        let op = &cmp.ops[0];
        if !matches!(op, ast::CmpOp::Gt | ast::CmpOp::GtE | ast::CmpOp::Eq | ast::CmpOp::NotEq) {
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

// ============================================================================
// Rule 5: unreachable code after return / raise / break / continue
// ============================================================================

fn check_unreachable(stmts: &[Stmt], source: &str, path: &Path, out: &mut Vec<Issue>) {
    scan_block_for_unreachable(stmts, source, path, out);
    let mut visit = |stmt: &Stmt| match stmt {
        Stmt::FunctionDef(f) => scan_block_for_unreachable(&f.body, source, path, out),
        Stmt::AsyncFunctionDef(f) => scan_block_for_unreachable(&f.body, source, path, out),
        Stmt::ClassDef(c) => scan_block_for_unreachable(&c.body, source, path, out),
        Stmt::If(s) => {
            scan_block_for_unreachable(&s.body, source, path, out);
            scan_block_for_unreachable(&s.orelse, source, path, out);
        }
        Stmt::While(s) => scan_block_for_unreachable(&s.body, source, path, out),
        Stmt::For(s) => scan_block_for_unreachable(&s.body, source, path, out),
        Stmt::AsyncFor(s) => scan_block_for_unreachable(&s.body, source, path, out),
        Stmt::Try(s) => {
            scan_block_for_unreachable(&s.body, source, path, out);
            for ExceptHandler::ExceptHandler(h) in &s.handlers {
                scan_block_for_unreachable(&h.body, source, path, out);
            }
            scan_block_for_unreachable(&s.orelse, source, path, out);
            scan_block_for_unreachable(&s.finalbody, source, path, out);
        }
        Stmt::With(s) => scan_block_for_unreachable(&s.body, source, path, out),
        Stmt::AsyncWith(s) => scan_block_for_unreachable(&s.body, source, path, out),
        Stmt::Match(s) => {
            for case in &s.cases {
                scan_block_for_unreachable(&case.body, source, path, out);
            }
        }
        _ => {}
    };
    walk_stmts(stmts, &mut visit);
}

fn scan_block_for_unreachable(block: &[Stmt], source: &str, path: &Path, out: &mut Vec<Issue>) {
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

// ============================================================================
// Rule 6: passthrough functions (def f(*a, **k): return g(*a, **k))
// ============================================================================

fn check_passthrough(stmts: &[Stmt], source: &str, path: &Path, out: &mut Vec<Issue>) {
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
        let Expr::Call(call) = value.as_ref() else { return };
        // Skip method-style calls (self.foo / module.foo) — likely intentional.
        if matches!(call.func.as_ref(), Expr::Attribute(_)) {
            return;
        }
        // Sanity: arity must match.
        let func_arity = args.posonlyargs.len() + args.args.len() + args.kwonlyargs.len();
        let call_arity = call.args.len() + call.keywords.len();
        if func_arity != call_arity || func_arity == 0 {
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

// ============================================================================
// Rule 7: stray print() in non-CLI modules
// ============================================================================

fn check_stray_print(stmts: &[Stmt], source: &str, path: &Path, out: &mut Vec<Issue>) {
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

// ============================================================================
// Rule 8: single-method class (no instance state) — should be a free function
// ============================================================================

fn check_single_method_class(stmts: &[Stmt], source: &str, path: &Path, out: &mut Vec<Issue>) {
    let mut visit = |stmt: &Stmt| {
        let Stmt::ClassDef(c) = stmt else { return };
        // Skip classes with non-trivial bases (likely framework subclasses).
        if !c.bases.is_empty() || !c.keywords.is_empty() || !c.decorator_list.is_empty() {
            return;
        }
        let mut method_count = 0u32;
        let mut has_state = false;
        let mut sole_method: Option<String> = None;
        let mut bail = false;
        for inner in &c.body {
            let fname = match inner {
                Stmt::FunctionDef(f) => Some(f.name.as_str()),
                Stmt::AsyncFunctionDef(f) => Some(f.name.as_str()),
                Stmt::Assign(_) | Stmt::AnnAssign(_) => {
                    has_state = true;
                    None
                }
                _ => None,
            };
            let Some(fname) = fname else { continue };
            if matches!(fname, "__init__" | "__new__") {
                has_state = true;
                continue;
            }
            if fname.starts_with("__") && fname.ends_with("__") {
                bail = true;
                break;
            }
            method_count += 1;
            sole_method = Some(fname.to_string());
        }
        if bail {
            return;
        }
        if has_state || method_count != 1 {
            return;
        }
        let line = line_at_offset(source, c.range.start().to_usize());
        let method = sole_method.as_deref().unwrap_or("<method>");
        out.push(Issue::Smell {
            path: path.to_path_buf(),
            line,
            rule: SmellRule::SingleMethodClass,
            detail: format!(
                "class `{}` has only `{}` and no state; consider a free function",
                c.name.as_str(),
                method
            ),
        });
    };
    walk_stmts(stmts, &mut visit);
}

// ============================================================================
// Rule 9: high TODO/FIXME density (file-level)
// ============================================================================

fn check_todo_density(source: &str, path: &Path, threshold: u32, out: &mut Vec<Issue>) {
    let mut count = 0u32;
    for line in source.lines() {
        // Only count lines that contain a comment marker after a `#`.
        let Some(comment) = line.split_once('#') else { continue };
        let body = comment.1;
        for marker in &["TODO", "FIXME", "XXX", "HACK"] {
            if body.contains(marker) {
                count += 1;
                break;
            }
        }
    }
    if count >= threshold {
        out.push(Issue::Smell {
            path: path.to_path_buf(),
            line: 1,
            rule: SmellRule::HighTodoDensity,
            detail: format!("{count} TODO/FIXME markers in this file (threshold {threshold})"),
        });
    }
}

// ============================================================================
// Rule 10: raise ... from None inside an except handler (loses cause)
// ============================================================================

fn check_raise_from_none(stmts: &[Stmt], source: &str, path: &Path, out: &mut Vec<Issue>) {
    let mut visit = |stmt: &Stmt| {
        let Stmt::Try(t) = stmt else { return };
        for handler in &t.handlers {
            let ExceptHandler::ExceptHandler(h) = handler;
            for inner in &h.body {
                let Stmt::Raise(r) = inner else { continue };
                let Some(cause) = &r.cause else { continue };
                let Expr::Constant(c) = cause.as_ref() else { continue };
                if matches!(c.value, ast::Constant::None) {
                    let line = line_at_offset(source, r.range.start().to_usize());
                    out.push(Issue::Smell {
                        path: path.to_path_buf(),
                        line,
                        rule: SmellRule::RaiseFromNone,
                        detail: "`raise ... from None` discards the original exception cause".to_string(),
                    });
                }
            }
        }
    };
    walk_stmts(stmts, &mut visit);
}

// ============================================================================
// Traversal helpers
// ============================================================================

fn walk_stmts(stmts: &[Stmt], visit: &mut impl FnMut(&Stmt)) {
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

fn walk_stmts_for_exprs(stmts: &[Stmt], visit: &mut impl FnMut(&Expr)) {
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
        _ => {}
    }
}

fn stmt_range_start(stmt: &Stmt) -> usize {
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

pub fn run_with_files(files: &[PathBuf], opts: &SmellsOptions) -> Vec<Issue> {
    use pyllow_extract::parse_file;
    let parsed: Vec<ParsedModule> = files
        .par_iter()
        .filter_map(|p| parse_file(p).ok())
        .collect();
    let map: FxHashMap<FileId, ParsedModule> = parsed
        .into_iter()
        .enumerate()
        .map(|(i, m)| (FileId(i as u32), m))
        .collect();
    analyze(&map, opts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_extract::parse_source;
    use std::path::PathBuf;

    fn run(source: &str) -> Vec<Issue> {
        let path = PathBuf::from("/tmp/test.py");
        let module = parse_source(&path, source).expect("parse");
        analyze_module(&module, &SmellsOptions::default())
    }

    fn rules(issues: &[Issue]) -> Vec<SmellRule> {
        issues
            .iter()
            .filter_map(|i| match i {
                Issue::Smell { rule, .. } => Some(*rule),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn detects_mutable_default() {
        let src = "def f(x=[]):\n    return x\n";
        assert!(rules(&run(src)).contains(&SmellRule::MutableDefault));
    }

    #[test]
    fn ignores_immutable_default() {
        let src = "def f(x=()):\n    return x\n";
        assert!(!rules(&run(src)).contains(&SmellRule::MutableDefault));
    }

    #[test]
    fn detects_bare_except() {
        let src = "try:\n    pass\nexcept:\n    pass\n";
        assert!(rules(&run(src)).contains(&SmellRule::BroadExcept));
    }

    #[test]
    fn ignores_specific_except() {
        let src = "try:\n    pass\nexcept ValueError:\n    pass\n";
        assert!(!rules(&run(src)).contains(&SmellRule::BroadExcept));
    }

    #[test]
    fn detects_sentinel_equality() {
        let src = "x = 1\nif x == None: pass\nif x != True: pass\n";
        let r = rules(&run(src));
        assert!(r.iter().filter(|x| **x == SmellRule::SentinelEquality).count() >= 2);
    }

    #[test]
    fn detects_truthy_length() {
        let src = "x = []\nif len(x) > 0: pass\n";
        assert!(rules(&run(src)).contains(&SmellRule::TruthyLengthCheck));
    }

    #[test]
    fn detects_unreachable_after_return() {
        let src = "def f():\n    return 1\n    print(\"dead\")\n";
        assert!(rules(&run(src)).contains(&SmellRule::UnreachableAfterExit));
    }

    #[test]
    fn detects_passthrough() {
        let src = "def wrap(a, b):\n    return inner(a, b)\n";
        assert!(rules(&run(src)).contains(&SmellRule::PassthroughFunction));
    }

    #[test]
    fn skips_method_passthrough() {
        let src = "def wrap(a, b):\n    return self.inner(a, b)\n";
        assert!(!rules(&run(src)).contains(&SmellRule::PassthroughFunction));
    }

    #[test]
    fn detects_stray_print() {
        let src = "def f():\n    print(\"hi\")\n";
        assert!(rules(&run(src)).contains(&SmellRule::StrayPrint));
    }

    #[test]
    fn skips_print_under_main_guard() {
        let src = "if __name__ == \"__main__\":\n    print(\"hi\")\n";
        assert!(!rules(&run(src)).contains(&SmellRule::StrayPrint));
    }

    #[test]
    fn detects_single_method_class() {
        let src = "class Helper:\n    def run(self, x):\n        return x + 1\n";
        assert!(rules(&run(src)).contains(&SmellRule::SingleMethodClass));
    }

    #[test]
    fn skips_class_with_state() {
        let src = "class State:\n    counter = 0\n    def run(self):\n        return self.counter\n";
        assert!(!rules(&run(src)).contains(&SmellRule::SingleMethodClass));
    }

    #[test]
    fn detects_high_todo_density() {
        let mut src = String::new();
        for i in 0..6 {
            src.push_str(&format!("# TODO: thing {i}\n"));
        }
        src.push_str("x = 1\n");
        assert!(rules(&run(&src)).contains(&SmellRule::HighTodoDensity));
    }

    #[test]
    fn detects_raise_from_none() {
        let src = "try:\n    pass\nexcept ValueError:\n    raise RuntimeError() from None\n";
        assert!(rules(&run(src)).contains(&SmellRule::RaiseFromNone));
    }

    #[test]
    fn disabled_rule_is_skipped() {
        let src = "def f(x=[]):\n    return x\n";
        let mut disabled = FxHashSet::default();
        disabled.insert(SmellRule::MutableDefault);
        let opts = SmellsOptions {
            disabled,
            ..SmellsOptions::default()
        };
        let path = PathBuf::from("/tmp/test.py");
        let module = parse_source(&path, src).unwrap();
        let issues = analyze_module(&module, &opts);
        assert!(!rules(&issues).contains(&SmellRule::MutableDefault));
    }
}
