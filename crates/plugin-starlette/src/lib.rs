use pyllow_extract::ast::{Expr, Stmt};
use pyllow_extract::walker::walk_stmts;
use pyllow_extract::{callable_tail_in, has_top_level_import, ParsedModule};
use pyllow_types::{FileId, PluginResult};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

pub const PLUGIN_NAME: &str = "starlette";

const STARLETTE_CALLABLES: &[&str] = &["Starlette", "Route", "WebSocketRoute", "Mount", "Host"];

const APP_DECORATORS: &[&str] = &["exception_handler", "middleware", "on_event"];

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
    if !has_top_level_import(module, &["starlette"]) {
        return false;
    }
    let mut found = false;
    walk_stmts(&module.suite, &mut |stmt: &Stmt| {
        if found {
            return;
        }
        let hit = match stmt {
            Stmt::Assign(a) => expr_contains_starlette_call(&a.value),
            Stmt::AnnAssign(a) => a
                .value
                .as_deref()
                .map(expr_contains_starlette_call)
                .unwrap_or(false),
            Stmt::Expr(e) => expr_contains_starlette_call(&e.value),
            Stmt::FunctionDef(f) => f
                .decorator_list
                .iter()
                .any(|d| callable_tail_in(d, APP_DECORATORS)),
            Stmt::AsyncFunctionDef(f) => f
                .decorator_list
                .iter()
                .any(|d| callable_tail_in(d, APP_DECORATORS)),
            _ => false,
        };
        if hit {
            found = true;
        }
    });
    found
}

/// Look for Starlette-shaped calls inside an expression: bare ctor calls
/// (`Starlette(...)`), or list literals containing `Route`/`Mount` items
/// (the canonical `routes = [Route(...)]` pattern).
fn expr_contains_starlette_call(expr: &Expr) -> bool {
    if callable_tail_in(expr, STARLETTE_CALLABLES) {
        return true;
    }
    if let Expr::List(list) = expr {
        return list
            .elts
            .iter()
            .any(|e| callable_tail_in(e, STARLETTE_CALLABLES));
    }
    false
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
        let m = module_from("from fastapi import FastAPI\napp = FastAPI()\n");
        assert!(!module_is_starlette_entry(&m));
    }

    #[test]
    fn ignores_starlette_import_without_call_or_decorator() {
        let m = module_from("from starlette import status\nx = status.HTTP_200_OK\n");
        assert!(!module_is_starlette_entry(&m));
    }
}
