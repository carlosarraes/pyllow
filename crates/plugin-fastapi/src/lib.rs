use pyllow_extract::ast::{self, Expr, Stmt};
use pyllow_extract::{callable_tail_name, ParsedModule};
use pyllow_types::{FileId, PluginResult};
use rustc_hash::{FxHashMap, FxHashSet};

pub const PLUGIN_NAME: &str = "fastapi";

const HTTP_VERB_METHODS: &[&str] = &[
    "get",
    "post",
    "put",
    "patch",
    "delete",
    "head",
    "options",
    "websocket",
];

const APP_CTORS: &[&str] = &["FastAPI", "APIRouter"];

pub fn discover(parsed: &FxHashMap<FileId, ParsedModule>) -> PluginResult {
    let mut entry_files = FxHashSet::default();
    for (id, module) in parsed {
        if module_is_fastapi_entry(&module.suite) {
            entry_files.insert(*id);
        }
    }
    PluginResult {
        plugin_name: PLUGIN_NAME.to_string(),
        entry_files,
        ..Default::default()
    }
}

fn module_is_fastapi_entry(body: &[Stmt]) -> bool {
    body.iter().any(stmt_marks_fastapi_entry)
}

fn stmt_marks_fastapi_entry(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::FunctionDef(f) => {
            f.decorator_list.iter().any(is_route_decorator)
                || f.body.iter().any(stmt_marks_fastapi_entry)
        }
        Stmt::AsyncFunctionDef(f) => {
            f.decorator_list.iter().any(is_route_decorator)
                || f.body.iter().any(stmt_marks_fastapi_entry)
        }
        Stmt::ClassDef(c) => c.body.iter().any(stmt_marks_fastapi_entry),
        Stmt::Assign(a) => is_app_ctor_call(&a.value),
        Stmt::AnnAssign(a) => a.value.as_deref().map(is_app_ctor_call).unwrap_or(false),
        Stmt::Expr(e) => is_include_router_call(&e.value),
        Stmt::If(s) => {
            s.body.iter().any(stmt_marks_fastapi_entry)
                || s.orelse.iter().any(stmt_marks_fastapi_entry)
        }
        Stmt::Try(s) => {
            s.body.iter().any(stmt_marks_fastapi_entry)
                || s.handlers.iter().any(|h| {
                    let ast::ExceptHandler::ExceptHandler(eh) = h;
                    eh.body.iter().any(stmt_marks_fastapi_entry)
                })
                || s.orelse.iter().any(stmt_marks_fastapi_entry)
                || s.finalbody.iter().any(stmt_marks_fastapi_entry)
        }
        Stmt::With(s) => s.body.iter().any(stmt_marks_fastapi_entry),
        Stmt::AsyncWith(s) => s.body.iter().any(stmt_marks_fastapi_entry),
        _ => false,
    }
}

fn is_route_decorator(expr: &Expr) -> bool {
    let attr_target = match expr {
        Expr::Call(c) => c.func.as_ref(),
        other => other,
    };
    let Expr::Attribute(attr) = attr_target else {
        return false;
    };
    HTTP_VERB_METHODS.contains(&attr.attr.as_str())
}

fn is_app_ctor_call(expr: &Expr) -> bool {
    if !matches!(expr, Expr::Call(_)) {
        return false;
    }
    callable_tail_name(expr)
        .map(|n| APP_CTORS.contains(&n))
        .unwrap_or(false)
}

fn is_include_router_call(expr: &Expr) -> bool {
    let Expr::Call(call) = expr else {
        return false;
    };
    let Expr::Attribute(attr) = call.func.as_ref() else {
        return false;
    };
    attr.attr.as_str() == "include_router"
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_extract::parse_source;
    use std::path::Path;

    fn parse(src: &str) -> ParsedModule {
        parse_source(Path::new("test.py"), src).unwrap()
    }

    #[test]
    fn detects_route_decorator_with_call() {
        let m = parse(
            "from fastapi import FastAPI\napp = FastAPI()\n@app.get(\"/items\")\ndef list_items():\n    pass\n",
        );
        assert!(module_is_fastapi_entry(&m.suite));
    }

    #[test]
    fn detects_websocket_decorator() {
        let m = parse(
            "from fastapi import APIRouter\nrouter = APIRouter()\n@router.websocket(\"/ws\")\nasync def ws_handler():\n    pass\n",
        );
        assert!(module_is_fastapi_entry(&m.suite));
    }

    #[test]
    fn detects_app_ctor() {
        let m = parse("from fastapi import FastAPI\napp = FastAPI(title=\"x\")\n");
        assert!(module_is_fastapi_entry(&m.suite));
    }

    #[test]
    fn detects_router_ctor() {
        let m = parse("from fastapi import APIRouter\nrouter = APIRouter(prefix=\"/v1\")\n");
        assert!(module_is_fastapi_entry(&m.suite));
    }

    #[test]
    fn detects_include_router_call() {
        let m = parse(
            "from .routers import users\napp.include_router(users.router)\n",
        );
        assert!(module_is_fastapi_entry(&m.suite));
    }

    #[test]
    fn ignores_unrelated_modules() {
        let m = parse("def helper():\n    return 42\n\nclass Thing:\n    pass\n");
        assert!(!module_is_fastapi_entry(&m.suite));
    }

    #[test]
    fn ignores_unrelated_decorators() {
        let m = parse(
            "import functools\n@functools.lru_cache\ndef cached():\n    return 1\n",
        );
        assert!(!module_is_fastapi_entry(&m.suite));
    }

    #[test]
    fn detects_factory_pattern() {
        let m = parse(
            "from fastapi import FastAPI\n\ndef create_app() -> FastAPI:\n    app = FastAPI()\n    app.include_router(routes)\n    return app\n",
        );
        assert!(module_is_fastapi_entry(&m.suite));
    }

    #[test]
    fn detects_route_inside_method() {
        let m = parse(
            "class Builder:\n    def configure(self, app):\n        @app.get(\"/x\")\n        def handler():\n            pass\n",
        );
        assert!(module_is_fastapi_entry(&m.suite));
    }
}
