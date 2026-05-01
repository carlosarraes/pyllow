use pyllow_types::{ImportKind, ImportSpecifier};
use rustc_hash::FxHashMap;
use rustpython_ast::{Expr, Stmt, Suite};
use rustpython_parser::Parse;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub mod walker;

#[derive(Debug, Error)]
pub enum ExtractError {
    #[error("io error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parse error in {path}: {message}")]
    Parse { path: PathBuf, message: String },
}

#[derive(Debug, Clone)]
pub struct ParsedModule {
    pub path: PathBuf,
    pub imports: Vec<ImportSpecifier>,
    pub exports: Vec<String>,
    pub suite: Suite,
    pub is_script_entry: bool,
    /// True iff the module defines `__getattr__` at top level — a deliberate
    /// dynamic-import surface (e.g., pydantic's `getattr_migration` shims for
    /// v1 backward-compat). PEP 562. Such modules are treated as live entry
    /// points so they don't get flagged as unused.
    pub has_module_getattr: bool,
    pub unused_imports: Vec<UnusedImportInfo>,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct UnusedImportInfo {
    pub name: String,
    pub module: String,
    pub line: u32,
}

pub fn parse_file(path: &Path) -> Result<ParsedModule, ExtractError> {
    let source = fs::read_to_string(path).map_err(|e| ExtractError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    parse_source(path, &source)
}

pub fn parse_source(path: &Path, source: &str) -> Result<ParsedModule, ExtractError> {
    let path_str = path.to_string_lossy();
    let suite = Suite::parse(source, &path_str).map_err(|e| ExtractError::Parse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;

    let mut visitor = Visitor::default();
    visitor.walk_top(&suite);

    let is_script_entry = suite.iter().any(is_name_eq_main_guard);
    let has_module_getattr = suite.iter().any(is_module_getattr_definition);
    let unused_imports = if is_init_py(path) {
        // `__init__.py` is by convention a re-export hub: every
        // module-level import is part of the package's public API
        // surface. Flagging them as unused produces noise on every
        // real-world library (httpx, pydantic, requests). Trade-off:
        // a genuinely unused `import os` in __init__.py won't be
        // caught — acceptable since it's rare and ruff/pyflakes still
        // flag it at single-file scope.
        Vec::new()
    } else {
        compute_unused_imports(&suite, source)
    };

    Ok(ParsedModule {
        path: path.to_path_buf(),
        imports: visitor.imports,
        exports: visitor.exports,
        suite,
        is_script_entry,
        has_module_getattr,
        unused_imports,
        source: source.to_string(),
    })
}

/// PEP 562: a module-level `__getattr__` (function def or assignment) is a
/// deliberate dynamic-attribute hook. Its presence means the module is
/// importable from outside via attributes that don't exist at static-time;
/// pyllow can't see those external consumers, so the module is live-by-design.
fn is_module_getattr_definition(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::FunctionDef(f) => f.name.as_str() == "__getattr__",
        Stmt::AsyncFunctionDef(f) => f.name.as_str() == "__getattr__",
        Stmt::Assign(a) => a
            .targets
            .iter()
            .any(|t| matches!(t, rustpython_ast::Expr::Name(n) if n.id.as_str() == "__getattr__")),
        Stmt::AnnAssign(a) => {
            matches!(a.target.as_ref(), rustpython_ast::Expr::Name(n) if n.id.as_str() == "__getattr__")
        }
        _ => false,
    }
}

pub fn line_at_offset(source: &str, offset: usize) -> u32 {
    let bounded = offset.min(source.len());
    source[..bounded].bytes().filter(|b| *b == b'\n').count() as u32 + 1
}

fn is_init_py(path: &Path) -> bool {
    path.file_name().and_then(|s| s.to_str()) == Some("__init__.py")
}

#[derive(Debug, Clone)]
struct ImportBinding {
    name: String,
    module: String,
    start_line: u32,
    end_line: u32,
    /// Byte range of the *whole* import statement. Usage detection compares
    /// each identifier reference's offset against this range so that
    /// `import os; print(os.getcwd())` correctly counts the trailing
    /// `os.getcwd()` as usage even though it shares a line with the import.
    start_offset: usize,
    end_offset: usize,
}

fn compute_unused_imports(suite: &Suite, source: &str) -> Vec<UnusedImportInfo> {
    let mut bindings = Vec::new();
    for stmt in suite {
        collect_import_bindings(stmt, source, &mut bindings);
    }
    if bindings.is_empty() {
        return Vec::new();
    }
    let identifier_offsets = collect_identifier_offsets(suite);
    let mut out = Vec::new();
    for b in &bindings {
        if line_has_noqa(source, b.start_line, b.end_line) {
            continue;
        }
        let used_outside = identifier_offsets
            .get(&b.name)
            .map(|offsets| {
                offsets
                    .iter()
                    .any(|o| *o < b.start_offset || *o >= b.end_offset)
            })
            .unwrap_or(false);
        if !used_outside {
            out.push(UnusedImportInfo {
                name: b.name.clone(),
                module: b.module.clone(),
                line: b.start_line,
            });
        }
    }
    out
}

fn collect_import_bindings(stmt: &Stmt, source: &str, out: &mut Vec<ImportBinding>) {
    match stmt {
        Stmt::Import(s) => {
            let start_offset = s.range.start().to_usize();
            let end_offset = s.range.end().to_usize();
            let start_line = line_at_offset(source, start_offset);
            let end_line = line_at_offset(source, end_offset);
            for alias in &s.names {
                let module = alias.name.as_str().to_string();
                // PEP 484 explicit re-export: `import X as X` (self-rename)
                // — recognized by mypy / pyright / ruff. The user is
                // deliberately exposing X as a public name; not "unused."
                if let Some(a) = &alias.asname {
                    if a.as_str() == module {
                        continue;
                    }
                }
                let bound = match &alias.asname {
                    Some(a) => a.as_str().to_string(),
                    None => module.split('.').next().unwrap_or(&module).to_string(),
                };
                out.push(ImportBinding {
                    name: bound,
                    module,
                    start_line,
                    end_line,
                    start_offset,
                    end_offset,
                });
            }
        }
        Stmt::ImportFrom(s) => {
            let start_offset = s.range.start().to_usize();
            let end_offset = s.range.end().to_usize();
            let start_line = line_at_offset(source, start_offset);
            let end_line = line_at_offset(source, end_offset);
            let module_prefix = s
                .module
                .as_ref()
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            if module_prefix == "__future__" {
                return;
            }
            for alias in &s.names {
                let alias_name = alias.name.as_str();
                if alias_name == "*" {
                    continue;
                }
                // PEP 484 explicit re-export: `from .x import Y as Y`.
                // The asname matching the imported name is the recognized
                // marker; skip the binding so it doesn't flag as unused.
                if let Some(a) = &alias.asname {
                    if a.as_str() == alias_name {
                        continue;
                    }
                }
                let bound = match &alias.asname {
                    Some(a) => a.as_str().to_string(),
                    None => alias_name.to_string(),
                };
                let qualified = if module_prefix.is_empty() {
                    alias_name.to_string()
                } else {
                    format!("{module_prefix}.{alias_name}")
                };
                out.push(ImportBinding {
                    name: bound,
                    module: qualified,
                    start_line,
                    end_line,
                    start_offset,
                    end_offset,
                });
            }
        }
        Stmt::If(s) => {
            recurse(&s.body, source, out);
            recurse(&s.orelse, source, out);
        }
        Stmt::Try(s) => {
            recurse(&s.body, source, out);
            for handler in &s.handlers {
                let rustpython_ast::ExceptHandler::ExceptHandler(h) = handler;
                recurse(&h.body, source, out);
            }
            recurse(&s.orelse, source, out);
            recurse(&s.finalbody, source, out);
        }
        Stmt::TryStar(s) => {
            recurse(&s.body, source, out);
            for handler in &s.handlers {
                let rustpython_ast::ExceptHandler::ExceptHandler(h) = handler;
                recurse(&h.body, source, out);
            }
            recurse(&s.orelse, source, out);
            recurse(&s.finalbody, source, out);
        }
        Stmt::With(s) => recurse(&s.body, source, out),
        Stmt::AsyncWith(s) => recurse(&s.body, source, out),
        Stmt::For(s) => {
            recurse(&s.body, source, out);
            recurse(&s.orelse, source, out);
        }
        Stmt::AsyncFor(s) => {
            recurse(&s.body, source, out);
            recurse(&s.orelse, source, out);
        }
        Stmt::While(s) => {
            recurse(&s.body, source, out);
            recurse(&s.orelse, source, out);
        }
        Stmt::Match(s) => {
            for case in &s.cases {
                recurse(&case.body, source, out);
            }
        }
        Stmt::FunctionDef(f) => recurse(&f.body, source, out),
        Stmt::AsyncFunctionDef(f) => recurse(&f.body, source, out),
        Stmt::ClassDef(c) => recurse(&c.body, source, out),
        _ => {}
    }
}

fn recurse(stmts: &[Stmt], source: &str, out: &mut Vec<ImportBinding>) {
    for inner in stmts {
        collect_import_bindings(inner, source, out);
    }
}

/// Map identifier names to the byte offsets at which they appear, walking
/// the parsed AST so identifiers inside string literals or comments don't
/// count as usage. A regex scan over the raw source previously made
/// `import os; print("os")` look like a real use of `os`, hiding genuine
/// unused-import findings. The lexer alone misses f-string interpolations
/// (`f"{os.sep}"`), which the parser exposes as nested `Expr::Name` nodes
/// inside `JoinedStr`/`FormattedValue` — so we walk expressions instead.
///
/// Annotations get a second pass: PEP 484 forward references like
/// `def f(x: "User")` carry the import name inside a string literal, so we
/// scan annotation-context strings for identifier-shaped tokens. Plain
/// strings outside annotations are still ignored.
///
/// Offsets (not lines) are recorded so callers can distinguish references
/// that share a line with their binding (e.g. `import os; os.getcwd()`)
/// from the binding statement itself.
fn collect_identifier_offsets(suite: &Suite) -> FxHashMap<String, Vec<usize>> {
    let mut out: FxHashMap<String, Vec<usize>> = FxHashMap::default();
    let mut visit = |expr: &Expr| {
        if let Expr::Name(n) = expr {
            out.entry(n.id.as_str().to_string())
                .or_default()
                .push(n.range.start().to_usize());
        }
    };
    walker::walk_stmts_for_exprs(suite, &mut visit);

    let mut visit_annotation = |expr: &Expr| {
        if let Expr::Constant(c) = expr {
            if let rustpython_ast::Constant::Str(s) = &c.value {
                let offset = c.range.start().to_usize();
                for ident in extract_identifier_tokens(s) {
                    out.entry(ident).or_default().push(offset);
                }
            }
        }
    };
    walker::walk_annotations(suite, &mut visit_annotation);

    // `__all__ = ["Foo", "Bar"]` is the canonical way for non-`__init__.py`
    // modules to declare their public API. Names listed there are real
    // exports — without crediting them, a public module that relies on
    // `__all__` would have all its imports flagged as unused.
    collect_all_exports(suite, &mut out);

    out
}

fn collect_all_exports(suite: &Suite, out: &mut FxHashMap<String, Vec<usize>>) {
    for stmt in suite {
        match stmt {
            Stmt::Assign(a)
                if a.targets
                    .iter()
                    .any(|t| matches!(t, Expr::Name(n) if n.id.as_str() == "__all__")) =>
            {
                push_string_names_in(&a.value, out);
            }
            Stmt::AnnAssign(a) if matches!(a.target.as_ref(), Expr::Name(n) if n.id.as_str() == "__all__") => {
                if let Some(v) = &a.value {
                    push_string_names_in(v, out);
                }
            }
            Stmt::AugAssign(a) if matches!(a.target.as_ref(), Expr::Name(n) if n.id.as_str() == "__all__") =>
            {
                // `__all__ += ["Foo"]` (and `extend(["Foo"])` patterns).
                push_string_names_in(&a.value, out);
            }
            _ => {}
        }
    }
}

/// Pull bare-string elements out of a list/tuple expression — `["Foo",
/// "Bar"]`, `("Foo",)`, etc. — and record each as a usage at its byte
/// offset. Non-string elements (computed expressions like
/// `__all__ = [*OTHER, "Foo"]`) are silently skipped; we'd need a more
/// sophisticated dataflow pass to credit those.
fn push_string_names_in(expr: &Expr, out: &mut FxHashMap<String, Vec<usize>>) {
    let elts = match expr {
        Expr::List(l) => &l.elts,
        Expr::Tuple(t) => &t.elts,
        _ => return,
    };
    for elt in elts {
        if let Expr::Constant(c) = elt {
            if let rustpython_ast::Constant::Str(s) = &c.value {
                let offset = c.range.start().to_usize();
                out.entry(s.to_string()).or_default().push(offset);
            }
        }
    }
}

/// Extract every Python-identifier-shaped substring from a string literal.
/// Used to mine PEP 484 forward references like `"User"`, `"List[User]"`,
/// or `"User | None"` for the names they reference. Conservative: we
/// over-count (`"True"` becomes a reference to `True`), which only risks
/// suppressing genuine unused-imports — the cost of a false negative here
/// is much lower than the false positive of failing to honor a legitimate
/// forward reference.
fn extract_identifier_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            out.push(s[start..i].to_string());
        } else {
            i += 1;
        }
    }
    out
}

fn line_has_noqa(source: &str, start_line: u32, end_line: u32) -> bool {
    source
        .lines()
        .enumerate()
        .filter(|(idx, _)| {
            let lineno = (*idx as u32) + 1;
            lineno >= start_line && lineno <= end_line
        })
        .any(|(_, line)| {
            let lower = line.to_ascii_lowercase();
            lower.contains("# noqa") || lower.contains("#noqa")
        })
}

fn is_name_eq_main_guard(stmt: &Stmt) -> bool {
    let Stmt::If(s) = stmt else {
        return false;
    };
    let rustpython_ast::Expr::Compare(cmp) = s.test.as_ref() else {
        return false;
    };
    if cmp.ops.len() != 1 || !matches!(cmp.ops[0], rustpython_ast::CmpOp::Eq) {
        return false;
    }
    let left_is_name = matches!(
        cmp.left.as_ref(),
        rustpython_ast::Expr::Name(n) if n.id.as_str() == "__name__"
    );
    let right_is_main_str = cmp
        .comparators
        .first()
        .map(is_main_string_literal)
        .unwrap_or(false);
    if left_is_name && right_is_main_str {
        return true;
    }
    let left_is_main = is_main_string_literal(cmp.left.as_ref());
    let right_is_name = cmp
        .comparators
        .first()
        .map(|e| matches!(e, rustpython_ast::Expr::Name(n) if n.id.as_str() == "__name__"))
        .unwrap_or(false);
    left_is_main && right_is_name
}

fn is_main_string_literal(expr: &rustpython_ast::Expr) -> bool {
    let rustpython_ast::Expr::Constant(c) = expr else {
        return false;
    };
    matches!(&c.value, rustpython_ast::Constant::Str(s) if s == "__main__")
}

pub use rustpython_ast as ast;

/// Return the trailing identifier of a callable expression, unwrapping the
/// outer `Call(...)` if present. Plugins use this to test decorator names
/// like `@app.task` / `@shared_task` / `@shared_task(bind=True)` against a
/// fixed list — every form collapses to `task` / `shared_task` here.
///
/// - `foo` → `Some("foo")`
/// - `mod.foo` → `Some("foo")`
/// - `mod.foo(...)` → `Some("foo")`
/// - any other shape → `None`
pub fn callable_tail_name(expr: &rustpython_ast::Expr) -> Option<&str> {
    use rustpython_ast::Expr;
    let target = match expr {
        Expr::Call(c) => c.func.as_ref(),
        other => other,
    };
    match target {
        Expr::Name(n) => Some(n.id.as_str()),
        Expr::Attribute(a) => Some(a.attr.as_str()),
        _ => None,
    }
}

/// Like [`callable_tail_name`] but also unwraps `Subscript[...]` so generic
/// base classes (`Generic[T]`, `Mapped[int]`) collapse to the constructor
/// name. Use for class-base lists.
pub fn base_class_tail_name(expr: &rustpython_ast::Expr) -> Option<&str> {
    use rustpython_ast::Expr;
    match expr {
        Expr::Subscript(s) => callable_tail_name(&s.value),
        other => callable_tail_name(other),
    }
}

/// True iff the callable's tail name is in `names`. Convenience wrapper
/// around [`callable_tail_name`] for the common plugin pattern of "is this
/// expression a call to one of these registered constructors/decorators".
pub fn callable_tail_in(expr: &rustpython_ast::Expr, names: &[&str]) -> bool {
    callable_tail_name(expr)
        .map(|n| names.contains(&n))
        .unwrap_or(false)
}

/// True iff the base-class tail name is in `names`. Convenience wrapper
/// around [`base_class_tail_name`] for the plugin pattern of "is this base
/// one of these tracked subclasses".
pub fn base_class_tail_in(expr: &rustpython_ast::Expr, names: &[&str]) -> bool {
    base_class_tail_name(expr)
        .map(|n| names.contains(&n))
        .unwrap_or(false)
}

/// True iff the module has a top-level absolute import whose dotted path
/// equals or starts with any of `prefixes` (e.g. `prefixes=["flask"]`
/// matches both `import flask` and `from flask.views import View`).
/// The standard plugin import-gate.
pub fn has_top_level_import(module: &ParsedModule, prefixes: &[&str]) -> bool {
    module.imports.iter().any(|i| {
        if !matches!(i.kind, ImportKind::Absolute) {
            return false;
        }
        prefixes
            .iter()
            .any(|p| i.raw == *p || i.raw.starts_with(&format!("{p}.")))
    })
}

#[derive(Default)]
struct Visitor {
    imports: Vec<ImportSpecifier>,
    exports: Vec<String>,
}

impl Visitor {
    fn walk_top(&mut self, body: &[Stmt]) {
        for stmt in body {
            self.walk_stmt(
                stmt, /*conditional=*/ false, /*type_only=*/ false,
                /*top_level=*/ true,
            );
        }
    }

    fn walk_stmt(&mut self, stmt: &Stmt, conditional: bool, type_only: bool, top_level: bool) {
        match stmt {
            Stmt::Import(s) => {
                for alias in &s.names {
                    self.imports.push(ImportSpecifier {
                        raw: alias.name.as_str().to_string(),
                        kind: ImportKind::Absolute,
                        is_conditional: conditional,
                        is_type_only: type_only,
                    });
                }
            }
            Stmt::ImportFrom(s) => {
                let level = s.level.map(|i| i.to_u32()).unwrap_or(0);
                let module = s.module.as_ref().map(|m| m.as_str()).unwrap_or("");
                let kind = if level > 0 {
                    ImportKind::Relative { level }
                } else {
                    ImportKind::Absolute
                };
                if !module.is_empty() {
                    self.imports.push(ImportSpecifier {
                        raw: module.to_string(),
                        kind,
                        is_conditional: conditional,
                        is_type_only: type_only,
                    });
                }
                for alias in &s.names {
                    let alias_name = alias.name.as_str();
                    if alias_name == "*" {
                        continue;
                    }
                    let raw = if module.is_empty() {
                        alias_name.to_string()
                    } else {
                        format!("{module}.{alias_name}")
                    };
                    self.imports.push(ImportSpecifier {
                        raw,
                        kind,
                        is_conditional: conditional,
                        is_type_only: type_only,
                    });
                }
            }
            Stmt::If(s) => {
                let is_typing_branch = is_type_checking_test(&s.test);
                let cond_branch = conditional || is_typing_branch;
                let type_only_branch = type_only || is_typing_branch;
                for inner in &s.body {
                    self.walk_stmt(inner, cond_branch, type_only_branch, top_level);
                }
                for inner in &s.orelse {
                    self.walk_stmt(inner, conditional, type_only, top_level);
                }
            }
            Stmt::Try(s) => {
                let handles_import_error = s.handlers.iter().any(handler_matches_import_error);
                let cond_body = conditional || handles_import_error;
                for inner in &s.body {
                    self.walk_stmt(inner, cond_body, type_only, top_level);
                }
                for handler in &s.handlers {
                    let rustpython_ast::ExceptHandler::ExceptHandler(h) = handler;
                    for inner in &h.body {
                        self.walk_stmt(inner, true, type_only, top_level);
                    }
                }
                for inner in &s.orelse {
                    self.walk_stmt(inner, conditional, type_only, top_level);
                }
                for inner in &s.finalbody {
                    self.walk_stmt(inner, conditional, type_only, top_level);
                }
            }
            // Without these arms, imports nested in for/while/match/try*
            // arms would be invisible to the module graph — `for _: import
            // helper` would make `helper.py` look like a dead file.
            Stmt::TryStar(s) => {
                for inner in &s.body {
                    self.walk_stmt(inner, conditional, type_only, top_level);
                }
                for handler in &s.handlers {
                    let rustpython_ast::ExceptHandler::ExceptHandler(h) = handler;
                    for inner in &h.body {
                        self.walk_stmt(inner, true, type_only, top_level);
                    }
                }
                for inner in &s.orelse {
                    self.walk_stmt(inner, conditional, type_only, top_level);
                }
                for inner in &s.finalbody {
                    self.walk_stmt(inner, conditional, type_only, top_level);
                }
            }
            Stmt::For(s) => {
                for inner in &s.body {
                    self.walk_stmt(inner, conditional, type_only, top_level);
                }
                for inner in &s.orelse {
                    self.walk_stmt(inner, conditional, type_only, top_level);
                }
            }
            Stmt::AsyncFor(s) => {
                for inner in &s.body {
                    self.walk_stmt(inner, conditional, type_only, top_level);
                }
                for inner in &s.orelse {
                    self.walk_stmt(inner, conditional, type_only, top_level);
                }
            }
            Stmt::While(s) => {
                for inner in &s.body {
                    self.walk_stmt(inner, conditional, type_only, top_level);
                }
                for inner in &s.orelse {
                    self.walk_stmt(inner, conditional, type_only, top_level);
                }
            }
            Stmt::Match(s) => {
                for case in &s.cases {
                    for inner in &case.body {
                        self.walk_stmt(inner, conditional, type_only, top_level);
                    }
                }
            }
            Stmt::FunctionDef(f) => {
                if top_level {
                    self.exports.push(f.name.as_str().to_string());
                }
                for inner in &f.body {
                    self.walk_stmt(inner, conditional, type_only, false);
                }
            }
            Stmt::AsyncFunctionDef(f) => {
                if top_level {
                    self.exports.push(f.name.as_str().to_string());
                }
                for inner in &f.body {
                    self.walk_stmt(inner, conditional, type_only, false);
                }
            }
            Stmt::ClassDef(c) => {
                if top_level {
                    self.exports.push(c.name.as_str().to_string());
                }
                for inner in &c.body {
                    self.walk_stmt(inner, conditional, type_only, false);
                }
            }
            Stmt::With(s) => {
                for inner in &s.body {
                    self.walk_stmt(inner, conditional, type_only, top_level);
                }
            }
            Stmt::AsyncWith(s) => {
                for inner in &s.body {
                    self.walk_stmt(inner, conditional, type_only, top_level);
                }
            }
            Stmt::Assign(a) if top_level => {
                for target in &a.targets {
                    if let rustpython_ast::Expr::Name(name) = target {
                        self.exports.push(name.id.as_str().to_string());
                    }
                }
            }
            Stmt::AnnAssign(a) if top_level => {
                if let rustpython_ast::Expr::Name(name) = a.target.as_ref() {
                    self.exports.push(name.id.as_str().to_string());
                }
            }
            _ => {}
        }
    }
}

fn is_type_checking_test(expr: &rustpython_ast::Expr) -> bool {
    match expr {
        rustpython_ast::Expr::Name(n) => n.id.as_str() == "TYPE_CHECKING",
        rustpython_ast::Expr::Attribute(a) => a.attr.as_str() == "TYPE_CHECKING",
        _ => false,
    }
}

fn handler_matches_import_error(handler: &rustpython_ast::ExceptHandler) -> bool {
    let rustpython_ast::ExceptHandler::ExceptHandler(h) = handler;
    let Some(ty) = h.type_.as_ref() else {
        return false;
    };
    expr_names_import_error(ty)
}

fn expr_names_import_error(expr: &rustpython_ast::Expr) -> bool {
    match expr {
        rustpython_ast::Expr::Name(n) => {
            matches!(n.id.as_str(), "ImportError" | "ModuleNotFoundError")
        }
        rustpython_ast::Expr::Tuple(t) => t.elts.iter().any(expr_names_import_error),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> ParsedModule {
        parse_source(Path::new("test.py"), src).unwrap()
    }

    #[test]
    fn extracts_top_level_import() {
        let m = parse("import os\nimport sys");
        assert_eq!(m.imports.len(), 2);
        assert_eq!(m.imports[0].raw, "os");
        assert!(matches!(m.imports[0].kind, ImportKind::Absolute));
        assert!(!m.imports[0].is_conditional);
    }

    #[test]
    fn extracts_from_import_emits_module_and_member_candidates() {
        let m = parse("from foo.bar import baz");
        let raws: Vec<&str> = m.imports.iter().map(|i| i.raw.as_str()).collect();
        assert!(raws.contains(&"foo.bar"));
        assert!(raws.contains(&"foo.bar.baz"));
        assert_eq!(m.imports.len(), 2);
    }

    #[test]
    fn extracts_relative_import() {
        let m = parse("from . import sibling\nfrom ..pkg import thing");
        let relatives: Vec<(&str, ImportKind)> =
            m.imports.iter().map(|i| (i.raw.as_str(), i.kind)).collect();
        assert!(relatives.contains(&("sibling", ImportKind::Relative { level: 1 })));
        assert!(relatives.contains(&("pkg", ImportKind::Relative { level: 2 })));
        assert!(relatives.contains(&("pkg.thing", ImportKind::Relative { level: 2 })));
    }

    #[test]
    fn skips_star_import() {
        let m = parse("from foo import *");
        assert_eq!(m.imports.len(), 1);
        assert_eq!(m.imports[0].raw, "foo");
    }

    #[test]
    fn marks_type_checking_imports_conditional() {
        let m = parse("from typing import TYPE_CHECKING\nif TYPE_CHECKING:\n    import foo");
        let foo = m.imports.iter().find(|i| i.raw == "foo").unwrap();
        assert!(foo.is_conditional);
        // TYPE_CHECKING imports never run at runtime — graph reachability
        // depends on this stricter flag to avoid keeping dead modules alive.
        assert!(foo.is_type_only);
        let typing = m.imports.iter().find(|i| i.raw == "typing").unwrap();
        assert!(!typing.is_conditional);
        assert!(!typing.is_type_only);
    }

    #[test]
    fn marks_try_import_error_conditional() {
        let m = parse("try:\n    import fast_thing\nexcept ImportError:\n    import slow_thing\n");
        let fast = m.imports.iter().find(|i| i.raw == "fast_thing").unwrap();
        let slow = m.imports.iter().find(|i| i.raw == "slow_thing").unwrap();
        assert!(fast.is_conditional);
        assert!(slow.is_conditional);
        // Try-fallback imports DO run at runtime — only TYPE_CHECKING is
        // type-only. Otherwise valid optional-dep patterns would have their
        // graph edges dropped and look like dead modules.
        assert!(!fast.is_type_only);
        assert!(!slow.is_type_only);
    }

    #[test]
    fn module_not_found_error_also_treated_conditional() {
        let m = parse("try:\n    import opt\nexcept ModuleNotFoundError:\n    pass\n");
        let opt = m.imports.iter().find(|i| i.raw == "opt").unwrap();
        assert!(opt.is_conditional);
    }

    #[test]
    fn extracts_exports_top_level_only() {
        let m = parse(
            "def public_fn():\n    pass\n\nclass MyClass:\n    pass\n\nCONST = 42\n\ndef _private():\n    def nested():\n        pass\n",
        );
        assert!(m.exports.contains(&"public_fn".to_string()));
        assert!(m.exports.contains(&"MyClass".to_string()));
        assert!(m.exports.contains(&"CONST".to_string()));
        assert!(m.exports.contains(&"_private".to_string()));
        assert!(!m.exports.contains(&"nested".to_string()));
    }

    #[test]
    fn extracts_imports_inside_function_body() {
        let m = parse(
            "def lazy_loader():\n    from src.foo import bar\n    import baz\n    return bar(baz)\n",
        );
        let raws: Vec<&str> = m.imports.iter().map(|i| i.raw.as_str()).collect();
        assert!(raws.contains(&"src.foo"));
        assert!(raws.contains(&"src.foo.bar"));
        assert!(raws.contains(&"baz"));
    }

    #[test]
    fn extracts_imports_inside_async_function_and_method() {
        let m = parse(
            "class Service:\n    async def handle(self):\n        from src.helper import work\n        return work()\n",
        );
        let raws: Vec<&str> = m.imports.iter().map(|i| i.raw.as_str()).collect();
        assert!(raws.contains(&"src.helper"));
        assert!(raws.contains(&"src.helper.work"));
    }

    #[test]
    fn extracts_imports_inside_with_block() {
        let m = parse("with open('x') as f:\n    import contextual\n");
        let raws: Vec<&str> = m.imports.iter().map(|i| i.raw.as_str()).collect();
        assert!(raws.contains(&"contextual"));
    }

    #[test]
    fn flags_unused_import() {
        let m = parse("import os\nprint(\"hi\")\n");
        let names: Vec<&str> = m.unused_imports.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, vec!["os"]);
    }

    #[test]
    fn init_py_does_not_flag_unused_imports() {
        // Convention: every module-level import in __init__.py is a
        // public re-export of the package's API. Flagging them as
        // unused fires on every library (httpx __version__, requests,
        // etc.) and creates more noise than signal.
        let path = Path::new("pkg/__init__.py");
        let src = "from .x import foo\nfrom .y import bar\n";
        let m = parse_source(path, src).unwrap();
        assert!(
            m.unused_imports.is_empty(),
            "__init__.py imports must not be flagged unused, got {:?}",
            m.unused_imports
        );
    }

    #[test]
    fn non_init_py_still_flags_unused_imports() {
        // Sanity: the __init__.py exemption must not leak into other
        // files in the same package.
        let path = Path::new("pkg/regular.py");
        let src = "from .x import foo\n";
        let m = parse_source(path, src).unwrap();
        let names: Vec<&str> = m.unused_imports.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, vec!["foo"]);
    }

    #[test]
    fn stringized_forward_ref_in_param_annotation_counts_as_usage() {
        let m = parse(
            "from typing import TYPE_CHECKING\nif TYPE_CHECKING:\n    from models import User\ndef f(x: \"User\") -> None:\n    pass\n",
        );
        let names: Vec<&str> = m.unused_imports.iter().map(|u| u.name.as_str()).collect();
        assert!(
            !names.contains(&"User"),
            "PEP 484 forward ref must count as usage, got unused: {names:?}"
        );
    }

    #[test]
    fn stringized_forward_ref_in_return_annotation_counts_as_usage() {
        let m = parse("from models import User\ndef f() -> \"User\":\n    pass\n");
        assert!(
            m.unused_imports.is_empty(),
            "return annotation forward ref must count, got: {:?}",
            m.unused_imports
        );
    }

    #[test]
    fn stringized_forward_ref_inside_optional_counts_as_usage() {
        let m = parse(
            "from typing import Optional\nfrom models import User\ndef f(x: Optional[\"User\"]) -> None:\n    pass\n",
        );
        let names: Vec<&str> = m.unused_imports.iter().map(|u| u.name.as_str()).collect();
        assert!(
            !names.contains(&"User"),
            "forward ref inside Optional must count, got unused: {names:?}"
        );
    }

    #[test]
    fn stringized_forward_ref_in_annassign_counts_as_usage() {
        let m = parse("from models import User\nx: \"User\"\n");
        assert!(
            m.unused_imports.is_empty(),
            "ann-assign forward ref must count, got: {:?}",
            m.unused_imports
        );
    }

    #[test]
    fn ordinary_string_with_identifier_does_not_count_as_usage() {
        // A plain string literal (not in annotation context) must NOT count
        // as usage — the whole point of the AST switch was to stop strings
        // from masking unused imports.
        let m = parse("from models import User\nprint(\"User\")\n");
        let names: Vec<&str> = m.unused_imports.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["User"],
            "plain string mention must not count as usage"
        );
    }

    #[test]
    fn import_inside_for_loop_is_visible_to_module_graph() {
        let m = parse("for _ in range(1):\n    import helper\n    helper.go()\n");
        let raws: Vec<&str> = m.imports.iter().map(|i| i.raw.as_str()).collect();
        assert!(
            raws.contains(&"helper"),
            "import nested inside for-loop must be exposed in m.imports, got: {raws:?}"
        );
    }

    #[test]
    fn import_inside_while_loop_is_visible() {
        let m = parse("while True:\n    import helper\n    break\n");
        let raws: Vec<&str> = m.imports.iter().map(|i| i.raw.as_str()).collect();
        assert!(raws.contains(&"helper"));
    }

    #[test]
    fn import_inside_match_arm_is_visible() {
        let m = parse(
            "match x:\n    case 1:\n        import helper\n        helper.go()\n    case _:\n        pass\n",
        );
        let raws: Vec<&str> = m.imports.iter().map(|i| i.raw.as_str()).collect();
        assert!(raws.contains(&"helper"));
    }

    #[test]
    fn import_inside_try_star_is_visible() {
        let m =
            parse("try:\n    pass\nexcept* RuntimeError:\n    import helper\n    helper.go()\n");
        let raws: Vec<&str> = m.imports.iter().map(|i| i.raw.as_str()).collect();
        assert!(raws.contains(&"helper"));
    }

    #[test]
    fn same_line_use_after_semicolon_counts_as_usage() {
        // `import os; print(os.getcwd())` — the second `os` reference sits
        // on the same line as the import but at a later byte offset,
        // outside the import statement's range.
        let m = parse("import os; print(os.getcwd())\n");
        assert!(
            m.unused_imports.is_empty(),
            "same-line use after `;` must count, got: {:?}",
            m.unused_imports
        );
    }

    #[test]
    fn same_line_use_before_semicolon_imported_does_not_count_self() {
        // Sanity check: `import os; pass` still flags `os` as unused —
        // the new range check must not accidentally count the import
        // statement's own `os` token as usage.
        let m = parse("import os; pass\n");
        let names: Vec<&str> = m.unused_imports.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, vec!["os"]);
    }

    #[test]
    fn try_except_handler_type_counts_as_usage() {
        let m = parse(
            "from errors import MyError\ndef f():\n    try:\n        pass\n    except MyError:\n        pass\n",
        );
        assert!(
            m.unused_imports.is_empty(),
            "except handler type must count as usage, got: {:?}",
            m.unused_imports
        );
    }

    #[test]
    fn try_star_except_handler_type_counts_as_usage() {
        let m = parse(
            "from errors import MyError\ndef f():\n    try:\n        pass\n    except* MyError:\n        pass\n",
        );
        assert!(
            m.unused_imports.is_empty(),
            "except* handler type must count as usage, got: {:?}",
            m.unused_imports
        );
    }

    #[test]
    fn type_alias_value_reference_counts_as_usage() {
        let m = parse("from decimal import Decimal\ntype Money = Decimal\n");
        assert!(
            m.unused_imports.is_empty(),
            "type alias rhs must count as usage, got: {:?}",
            m.unused_imports
        );
    }

    #[test]
    fn match_class_pattern_reference_counts_as_usage() {
        let m = parse(
            "from shapes import Point\ndef f(x):\n    match x:\n        case Point():\n            pass\n",
        );
        assert!(
            m.unused_imports.is_empty(),
            "match class pattern usage must count, got: {:?}",
            m.unused_imports
        );
    }

    #[test]
    fn match_value_pattern_dotted_reference_counts_as_usage() {
        // `case Constants.KIND_A:` is a value pattern referencing the import
        // (`case KIND_A:` would be a capture binding, not a value match —
        // PEP 634 requires a dotted attribute or a literal here).
        let m = parse(
            "from m import Constants\ndef f(x):\n    match x:\n        case Constants.KIND_A:\n            pass\n",
        );
        assert!(
            m.unused_imports.is_empty(),
            "match value pattern usage must count, got: {:?}",
            m.unused_imports
        );
    }

    #[test]
    fn class_base_reference_counts_as_usage() {
        let m = parse("from pydantic import BaseModel\nclass User(BaseModel):\n    name: str\n");
        assert!(
            m.unused_imports.is_empty(),
            "import used as a class base must not be flagged unused, got: {:?}",
            m.unused_imports
        );
    }

    #[test]
    fn decorator_reference_counts_as_usage() {
        let m =
            parse("from functools import lru_cache\n@lru_cache\ndef compute(x):\n    return x\n");
        assert!(m.unused_imports.is_empty(), "decorator usage must count");
    }

    #[test]
    fn function_annotation_reference_counts_as_usage() {
        let m =
            parse("from typing import Optional\ndef get(x: Optional[int]) -> None:\n    pass\n");
        assert!(
            m.unused_imports.is_empty(),
            "annotation usage must count, got: {:?}",
            m.unused_imports
        );
    }

    #[test]
    fn default_argument_reference_counts_as_usage() {
        let m = parse("from typing import List\ndef get(items=List):\n    return items\n");
        assert!(m.unused_imports.is_empty(), "default arg usage must count");
    }

    #[test]
    fn dunder_all_list_credits_imports_as_used() {
        // `__all__` is the canonical export-declaration mechanism for
        // non-__init__ public modules. Names listed there must count as
        // usage of the imports they reference.
        let path = Path::new("pkg/api.py");
        let src = "from .impl import Foo, Bar\n__all__ = [\"Foo\", \"Bar\"]\n";
        let m = parse_source(path, src).unwrap();
        let names: Vec<&str> = m.unused_imports.iter().map(|u| u.name.as_str()).collect();
        assert!(
            names.is_empty(),
            "names listed in __all__ must not be flagged unused, got {names:?}"
        );
    }

    #[test]
    fn dunder_all_tuple_also_credits_imports() {
        // PEP 8 doesn't mandate list vs tuple — both shapes show up in
        // real packages.
        let path = Path::new("pkg/api.py");
        let src = "from .impl import Foo\n__all__ = (\"Foo\",)\n";
        let m = parse_source(path, src).unwrap();
        assert!(m.unused_imports.is_empty(), "{:?}", m.unused_imports);
    }

    #[test]
    fn dunder_all_augmented_assign_credits_imports() {
        // `__all__ += [...]` is the conditional-extension pattern, e.g.
        // `if sys.version_info >= (3, 11): __all__ += ["NewThing"]`.
        let path = Path::new("pkg/api.py");
        let src = "from .impl import Foo\n__all__ = []\n__all__ += [\"Foo\"]\n";
        let m = parse_source(path, src).unwrap();
        assert!(m.unused_imports.is_empty(), "{:?}", m.unused_imports);
    }

    #[test]
    fn dunder_all_with_typed_annotation_credits_imports() {
        // PEP 526 annotated `__all__: list[str] = ["Foo"]` is also valid.
        let path = Path::new("pkg/api.py");
        let src = "from .impl import Foo\n__all__: list[str] = [\"Foo\"]\n";
        let m = parse_source(path, src).unwrap();
        assert!(m.unused_imports.is_empty(), "{:?}", m.unused_imports);
    }

    #[test]
    fn lambda_default_argument_reference_counts_as_usage() {
        // `lambda x=Decimal: x` — the default value is a real expression
        // referencing the import. The walker must visit lambda args
        // defaults, not just the body.
        let m = parse("from decimal import Decimal\nmake = lambda x=Decimal: x\n");
        assert!(
            m.unused_imports.is_empty(),
            "lambda default must count as usage, got {:?}",
            m.unused_imports
        );
    }

    #[test]
    fn lambda_annotation_reference_counts_as_usage() {
        // Lambdas can't carry PEP 484 annotations on parameters, but
        // for completeness verify default-walking handles vararg/kwarg
        // defaults (which do exist) — `lambda *, x=Decimal: x`.
        let m = parse("from decimal import Decimal\nmake = lambda *, x=Decimal: x\n");
        assert!(m.unused_imports.is_empty(), "{:?}", m.unused_imports);
    }

    #[test]
    fn class_metaclass_keyword_reference_counts_as_usage() {
        let m = parse("from abc import ABCMeta\nclass A(metaclass=ABCMeta):\n    pass\n");
        assert!(
            m.unused_imports.is_empty(),
            "metaclass keyword usage must count"
        );
    }

    #[test]
    fn class_decorator_reference_counts_as_usage() {
        let m = parse("from dataclasses import dataclass\n@dataclass\nclass A:\n    x: int\n");
        assert!(
            m.unused_imports.is_empty(),
            "class decorator usage must count"
        );
    }

    #[test]
    fn does_not_count_string_or_comment_mentions_as_usage() {
        let m = parse("import os\nprint(\"os value\")  # mention of os in comment\n");
        let names: Vec<&str> = m.unused_imports.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["os"],
            "string/comment mentions must not mask unused-import"
        );
    }

    #[test]
    fn fstring_identifier_reference_still_counts_as_usage() {
        let m = parse("import os\nprint(f\"{os.sep}\")\n");
        assert!(
            m.unused_imports.is_empty(),
            "f-string interpolation references real identifiers and must count as usage"
        );
    }

    #[test]
    fn does_not_flag_used_import() {
        let m = parse("import os\nprint(os.environ)\n");
        assert!(m.unused_imports.is_empty());
    }

    #[test]
    fn detects_used_alias() {
        let m = parse("import numpy as np\nx = np.zeros(3)\n");
        assert!(m.unused_imports.is_empty());
    }

    #[test]
    fn flags_unused_alias() {
        let m = parse("import numpy as np\nprint(\"hi\")\n");
        let names: Vec<&str> = m.unused_imports.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, vec!["np"]);
    }

    #[test]
    fn from_import_unused() {
        let m = parse("from os.path import join\nprint(\"hi\")\n");
        let names: Vec<&str> = m.unused_imports.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, vec!["join"]);
    }

    #[test]
    fn from_import_used_in_attribute() {
        let m = parse("from os import path\nprint(path.sep)\n");
        assert!(m.unused_imports.is_empty());
    }

    #[test]
    fn star_imports_never_flagged() {
        let m = parse("from os import *\nprint(\"hi\")\n");
        assert!(m.unused_imports.is_empty());
    }

    #[test]
    fn noqa_skips_flagging() {
        let m = parse("import sentry_sdk  # noqa: F401\nprint(\"hi\")\n");
        assert!(m.unused_imports.is_empty());
    }

    #[test]
    fn noqa_bare_skips_flagging() {
        let m = parse("import sentry_sdk  # noqa\nprint(\"hi\")\n");
        assert!(m.unused_imports.is_empty());
    }

    #[test]
    fn imports_in_function_bodies_checked_against_whole_file() {
        let m = parse("def lazy():\n    import json\n    return json.dumps({})\n");
        assert!(m.unused_imports.is_empty());
    }

    #[test]
    fn flags_dotted_import_unused() {
        let m = parse("import os.path\nprint(\"hi\")\n");
        let names: Vec<&str> = m.unused_imports.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, vec!["os"]);
    }

    #[test]
    fn future_imports_never_flagged() {
        let m =
            parse("from __future__ import annotations\nfrom __future__ import division\nx = 1\n");
        assert!(m.unused_imports.is_empty());
    }

    #[test]
    fn line_number_reported() {
        let m = parse("\n\nimport os\nprint(\"hi\")\n");
        assert_eq!(m.unused_imports.len(), 1);
        assert_eq!(m.unused_imports[0].line, 3);
    }

    #[test]
    fn detects_if_name_main_guard() {
        let m = parse("def go():\n    pass\n\nif __name__ == \"__main__\":\n    go()\n");
        assert!(m.is_script_entry);
    }

    #[test]
    fn detects_if_main_name_reversed_guard() {
        let m = parse("if \"__main__\" == __name__:\n    pass\n");
        assert!(m.is_script_entry);
    }

    #[test]
    fn ignores_unrelated_if_blocks() {
        let m = parse("import os\nif os.environ.get(\"DEBUG\"):\n    pass\n");
        assert!(!m.is_script_entry);
    }

    #[test]
    fn ignores_name_check_inside_function() {
        let m = parse("def main():\n    if __name__ == \"__main__\":\n        pass\n");
        assert!(!m.is_script_entry);
    }

    // PEP 484 explicit re-export tests
    // ----------------------------------------------------------------
    // `from .x import Y as Y` and `import X as X` (self-rename) are
    // recognized by mypy / pyright / ruff as deliberate re-exports.
    // Pyllow must not flag the bound name as unused.

    #[test]
    fn pep484_from_import_self_rename_is_not_unused() {
        let m = parse("from .app import Flask as Flask\n");
        assert!(
            m.unused_imports.is_empty(),
            "from .x import Y as Y is a PEP 484 re-export — must not be flagged"
        );
    }

    #[test]
    fn pep484_plain_import_self_rename_is_not_unused() {
        let m = parse("import json as json\n");
        assert!(
            m.unused_imports.is_empty(),
            "import X as X is a PEP 484 re-export — must not be flagged"
        );
    }

    #[test]
    fn from_import_with_different_alias_still_flagged_when_unused() {
        // `import X as different_name` is a real rename, not a re-export
        let m = parse("from os.path import join as joined\nprint(\"hi\")\n");
        let names: Vec<&str> = m.unused_imports.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, vec!["joined"]);
    }

    // PEP 562: module-level __getattr__
    // ----------------------------------------------------------------

    #[test]
    fn module_getattr_function_detected() {
        let m = parse("def __getattr__(name):\n    raise AttributeError(name)\n");
        assert!(m.has_module_getattr);
    }

    #[test]
    fn module_getattr_assignment_detected() {
        // pydantic's pattern: `__getattr__ = getattr_migration(__name__)`
        let m = parse(
            "from ._migration import getattr_migration\n__getattr__ = getattr_migration(__name__)\n",
        );
        assert!(m.has_module_getattr);
    }

    #[test]
    fn nested_getattr_inside_class_does_not_trigger() {
        let m = parse("class X:\n    def __getattr__(self, name):\n        return None\n");
        assert!(!m.has_module_getattr);
    }

    #[test]
    fn no_getattr_means_false() {
        let m = parse("def f(): return 1\n");
        assert!(!m.has_module_getattr);
    }
}
