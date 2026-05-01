use pyllow_extract::ast::{self, Expr, Stmt};
use pyllow_extract::{base_class_tail_name, callable_tail_name, ParsedModule};
use pyllow_types::{FileId, ImportKind, PluginResult};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

pub const PLUGIN_NAME: &str = "aiohttp";

const CTOR_NAMES: &[&str] = &["Application", "RouteTableDef"];

const ROUTE_DECORATORS: &[&str] = &[
    "get", "post", "put", "delete", "patch", "head", "options", "view", "route",
];

const VIEW_BASES: &[&str] = &["View"];

pub fn discover(parsed: &FxHashMap<FileId, ParsedModule>) -> PluginResult {
    let entry_files: FxHashSet<FileId> = parsed
        .par_iter()
        .filter_map(|(id, module)| module_is_aiohttp_entry(module).then_some(*id))
        .collect();
    PluginResult {
        plugin_name: PLUGIN_NAME.to_string(),
        entry_files,
        ..Default::default()
    }
}

fn module_is_aiohttp_entry(module: &ParsedModule) -> bool {
    if !imports_aiohttp(module) {
        return false;
    }
    module.suite.iter().any(stmt_marks_aiohttp_entry)
}

fn imports_aiohttp(module: &ParsedModule) -> bool {
    module.imports.iter().any(|i| {
        matches!(i.kind, ImportKind::Absolute)
            && (i.raw == "aiohttp" || i.raw.starts_with("aiohttp."))
    })
}

fn stmt_marks_aiohttp_entry(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::ClassDef(c) => {
            if c.bases.iter().any(is_view_base) {
                return true;
            }
            c.body.iter().any(stmt_marks_aiohttp_entry)
        }
        Stmt::Assign(a) => is_aiohttp_ctor(&a.value),
        Stmt::AnnAssign(a) => a.value.as_deref().map(is_aiohttp_ctor).unwrap_or(false),
        Stmt::Expr(e) => is_aiohttp_ctor(&e.value),
        Stmt::FunctionDef(f) => {
            f.decorator_list.iter().any(is_route_decorator)
                || f.body.iter().any(stmt_marks_aiohttp_entry)
        }
        Stmt::AsyncFunctionDef(f) => {
            f.decorator_list.iter().any(is_route_decorator)
                || f.body.iter().any(stmt_marks_aiohttp_entry)
        }
        Stmt::If(s) => {
            s.body.iter().any(stmt_marks_aiohttp_entry)
                || s.orelse.iter().any(stmt_marks_aiohttp_entry)
        }
        Stmt::Try(s) => {
            s.body.iter().any(stmt_marks_aiohttp_entry)
                || s.handlers.iter().any(|h| {
                    let ast::ExceptHandler::ExceptHandler(eh) = h;
                    eh.body.iter().any(stmt_marks_aiohttp_entry)
                })
        }
        Stmt::With(s) => s.body.iter().any(stmt_marks_aiohttp_entry),
        Stmt::AsyncWith(s) => s.body.iter().any(stmt_marks_aiohttp_entry),
        _ => false,
    }
}

fn is_aiohttp_ctor(expr: &Expr) -> bool {
    if !matches!(expr, Expr::Call(_)) {
        return false;
    }
    callable_tail_name(expr)
        .map(|n| CTOR_NAMES.contains(&n))
        .unwrap_or(false)
}

fn is_route_decorator(expr: &Expr) -> bool {
    callable_tail_name(expr)
        .map(|n| ROUTE_DECORATORS.contains(&n))
        .unwrap_or(false)
}

fn is_view_base(expr: &Expr) -> bool {
    base_class_tail_name(expr)
        .map(|n| VIEW_BASES.contains(&n))
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
    fn detects_application_ctor() {
        let m = module_from(
            "from aiohttp import web\napp = web.Application()\n",
        );
        assert!(module_is_aiohttp_entry(&m));
    }

    #[test]
    fn detects_route_decorator() {
        let m = module_from(
            "from aiohttp import web\nroutes = web.RouteTableDef()\n\
             @routes.get('/')\nasync def hello(request):\n    return web.Response(text='hi')\n",
        );
        assert!(module_is_aiohttp_entry(&m));
    }

    #[test]
    fn detects_view_subclass() {
        let m = module_from(
            "from aiohttp import web\nclass MyView(web.View):\n    async def get(self):\n        return web.Response()\n",
        );
        assert!(module_is_aiohttp_entry(&m));
    }

    #[test]
    fn ignores_module_without_aiohttp_import() {
        let m = module_from("def get(self): pass\n");
        assert!(!module_is_aiohttp_entry(&m));
    }

    #[test]
    fn ignores_aiohttp_import_without_use() {
        let m = module_from("from aiohttp import ClientSession\n");
        assert!(!module_is_aiohttp_entry(&m));
    }
}
