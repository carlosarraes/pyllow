use pyllow_types::{ImportKind, ImportSpecifier};
use rustpython_ast::{Stmt, Suite};
use rustpython_parser::Parse;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

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

    Ok(ParsedModule {
        path: path.to_path_buf(),
        imports: visitor.imports,
        exports: visitor.exports,
        suite,
    })
}

pub use rustpython_ast as ast;

#[derive(Default)]
struct Visitor {
    imports: Vec<ImportSpecifier>,
    exports: Vec<String>,
}

impl Visitor {
    fn walk_top(&mut self, body: &[Stmt]) {
        for stmt in body {
            self.walk_stmt(stmt, /*conditional=*/ false, /*top_level=*/ true);
        }
    }

    fn walk_stmt(&mut self, stmt: &Stmt, conditional: bool, top_level: bool) {
        match stmt {
            Stmt::Import(s) => {
                for alias in &s.names {
                    self.imports.push(ImportSpecifier {
                        raw: alias.name.as_str().to_string(),
                        kind: ImportKind::Absolute,
                        is_conditional: conditional,
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
                    });
                }
            }
            Stmt::If(s) => {
                let cond_branch = conditional || is_type_checking_test(&s.test);
                for inner in &s.body {
                    self.walk_stmt(inner, cond_branch, top_level);
                }
                for inner in &s.orelse {
                    self.walk_stmt(inner, conditional, top_level);
                }
            }
            Stmt::Try(s) => {
                let handles_import_error = s.handlers.iter().any(handler_matches_import_error);
                let cond_body = conditional || handles_import_error;
                for inner in &s.body {
                    self.walk_stmt(inner, cond_body, top_level);
                }
                for handler in &s.handlers {
                    let rustpython_ast::ExceptHandler::ExceptHandler(h) = handler;
                    for inner in &h.body {
                        self.walk_stmt(inner, true, top_level);
                    }
                }
                for inner in &s.orelse {
                    self.walk_stmt(inner, conditional, top_level);
                }
                for inner in &s.finalbody {
                    self.walk_stmt(inner, conditional, top_level);
                }
            }
            Stmt::FunctionDef(f) => {
                if top_level {
                    self.exports.push(f.name.as_str().to_string());
                }
                for inner in &f.body {
                    self.walk_stmt(inner, conditional, false);
                }
            }
            Stmt::AsyncFunctionDef(f) => {
                if top_level {
                    self.exports.push(f.name.as_str().to_string());
                }
                for inner in &f.body {
                    self.walk_stmt(inner, conditional, false);
                }
            }
            Stmt::ClassDef(c) => {
                if top_level {
                    self.exports.push(c.name.as_str().to_string());
                }
                for inner in &c.body {
                    self.walk_stmt(inner, conditional, false);
                }
            }
            Stmt::With(s) => {
                for inner in &s.body {
                    self.walk_stmt(inner, conditional, top_level);
                }
            }
            Stmt::AsyncWith(s) => {
                for inner in &s.body {
                    self.walk_stmt(inner, conditional, top_level);
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
        let typing = m.imports.iter().find(|i| i.raw == "typing").unwrap();
        assert!(!typing.is_conditional);
    }

    #[test]
    fn marks_try_import_error_conditional() {
        let m = parse(
            "try:\n    import fast_thing\nexcept ImportError:\n    import slow_thing\n",
        );
        let fast = m.imports.iter().find(|i| i.raw == "fast_thing").unwrap();
        let slow = m.imports.iter().find(|i| i.raw == "slow_thing").unwrap();
        assert!(fast.is_conditional);
        assert!(slow.is_conditional);
    }

    #[test]
    fn module_not_found_error_also_treated_conditional() {
        let m = parse(
            "try:\n    import opt\nexcept ModuleNotFoundError:\n    pass\n",
        );
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
}
