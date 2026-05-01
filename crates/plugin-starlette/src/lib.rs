use pyllow_extract::ast::{self, Expr, Stmt};
use pyllow_extract::{callable_tail_name, ParsedModule};
use pyllow_types::{FileId, ImportKind, PluginResult};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

pub const PLUGIN_NAME: &str = "starlette";

const STARLETTE_CALLABLES: &[&str] = &[
    "Starlette",
    "Route",
    "WebSocketRoute",
    "Mount",
    "Host",
];

const APP_DECORATORS: &[&str] =
    &["exception_handler", "middleware", "on_event"];

pub fn discover(parsed: &FxHashMap<FileId, ParsedModule>) -> PluginResult {
    let entry_files: FxHashSet<FileId> = parsed
        .par_iter()
        .filter_map(|(id, module)| module_is_starlette_entry(module).then_some(*id))
        .collect();
    PluginResult {
        plugin_name: PLUGIN_NAME.to_string(),
        entry_files,
        ..Default::default()
    }
}

fn module_is_starlette_entry(module: &ParsedModule) -> bool {
    if !imports_starlette(module) {
        return false;
    }
    module.suite.iter().any(stmt_marks_starlette_entry)
}

fn imports_starlette(module: &ParsedModule) -> bool {
    module.imports.iter().any(|i| {
        matches!(i.kind, ImportKind::Absolute)
            && (i.raw == "starlette" || i.raw.starts_with("starlette."))
    })
}

fn stmt_marks_starlette_entry(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Assign(a) => expr_contains_starlette_call(&a.value),
        Stmt::AnnAssign(a) => a
            .value
            .as_deref()
            .map(expr_contains_starlette_call)
            .unwrap_or(false),
        Stmt::Expr(e) => expr_contains_starlette_call(&e.value),
        Stmt::FunctionDef(f) => {
            f.decorator_list.iter().any(is_app_decorator)
                || f.body.iter().any(stmt_marks_starlette_entry)
        }
        Stmt::AsyncFunctionDef(f) => {
            f.decorator_list.iter().any(is_app_decorator)
                || f.body.iter().any(stmt_marks_starlette_entry)
        }
        Stmt::If(s) => {
            s.body.iter().any(stmt_marks_starlette_entry)
                || s.orelse.iter().any(stmt_marks_starlette_entry)
        }
        Stmt::Try(s) => {
            s.body.iter().any(stmt_marks_starlette_entry)
                || s.handlers.iter().any(|h| {
                    let ast::ExceptHandler::ExceptHandler(eh) = h;
                    eh.body.iter().any(stmt_marks_starlette_entry)
                })
        }
        Stmt::With(s) => s.body.iter().any(stmt_marks_starlette_entry),
        Stmt::AsyncWith(s) => s.body.iter().any(stmt_marks_starlette_entry),
        _ => false,
    }
}

/// Look for Starlette-shaped calls inside an expression: bare ctor calls
/// (`Starlette(...)`), or list literals containing `Route`/`Mount` items
/// (the canonical `routes = [Route(...)]` pattern).
fn expr_contains_starlette_call(expr: &Expr) -> bool {
    if is_starlette_call(expr) {
        return true;
    }
    if let Expr::List(list) = expr {
        return list.elts.iter().any(is_starlette_call);
    }
    false
}

fn is_starlette_call(expr: &Expr) -> bool {
    if !matches!(expr, Expr::Call(_)) {
        return false;
    }
    callable_tail_name(expr)
        .map(|n| STARLETTE_CALLABLES.contains(&n))
        .unwrap_or(false)
}

fn is_app_decorator(expr: &Expr) -> bool {
    callable_tail_name(expr)
        .map(|n| APP_DECORATORS.contains(&n))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_extract::parse_source;
    use std::path::Path;

    fn module_from(src: &str) -> ParsedModule {
        parse_source(Path::new("test.py"), src).unwrap()
    }

    #[test]
    fn detects_starlette_ctor() {
        let m = module_from(
            "from starlette.applications import Starlette\n\
             app = Starlette(debug=True)\n",
        );
        assert!(module_is_starlette_entry(&m));
    }

    #[test]
    fn detects_routes_list_pattern() {
        let m = module_from(
            "from starlette.routing import Route, Mount\n\
             from starlette.responses import PlainTextResponse\n\
             def homepage(request):\n    return PlainTextResponse('hi')\n\
             routes = [Route('/', homepage), Mount('/static', app=None)]\n",
        );
        assert!(module_is_starlette_entry(&m));
    }

    #[test]
    fn detects_app_decorator() {
        let m = module_from(
            "from starlette.applications import Starlette\n\
             from starlette.exceptions import HTTPException\n\
             app = ...\n\
             @app.exception_handler(HTTPException)\n\
             async def handler(request, exc):\n    return ...\n",
        );
        assert!(module_is_starlette_entry(&m));
    }

    #[test]
    fn ignores_module_without_starlette_import() {
        let m = module_from(
            "from fastapi import FastAPI\napp = FastAPI()\n",
        );
        assert!(!module_is_starlette_entry(&m));
    }

    #[test]
    fn ignores_starlette_import_without_call_or_decorator() {
        let m = module_from("from starlette import status\nx = status.HTTP_200_OK\n");
        assert!(!module_is_starlette_entry(&m));
    }
}
