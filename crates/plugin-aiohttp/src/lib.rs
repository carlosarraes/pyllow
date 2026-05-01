use pyllow_extract::ast::{Expr, Stmt};
use pyllow_extract::walker::walk_stmts;
use pyllow_extract::{base_class_tail_in, callable_tail_in, has_top_level_import, ParsedModule};
use pyllow_types::{FileId, PluginResult};
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
    if !has_top_level_import(module, &["aiohttp"]) {
        return false;
    }
    let mut found = false;
    walk_stmts(&module.suite, &mut |stmt: &Stmt| {
        if found {
            return;
        }
        let hit = match stmt {
            Stmt::ClassDef(c) => c.bases.iter().any(|b| base_class_tail_in(b, VIEW_BASES)),
            Stmt::Assign(a) => is_aiohttp_ctor(&a.value),
            Stmt::AnnAssign(a) => a.value.as_deref().map(is_aiohttp_ctor).unwrap_or(false),
            Stmt::Expr(e) => is_aiohttp_ctor(&e.value),
            Stmt::FunctionDef(f) => f
                .decorator_list
                .iter()
                .any(|d| callable_tail_in(d, ROUTE_DECORATORS)),
            Stmt::AsyncFunctionDef(f) => f
                .decorator_list
                .iter()
                .any(|d| callable_tail_in(d, ROUTE_DECORATORS)),
            _ => false,
        };
        if hit {
            found = true;
        }
    });
    found
}

fn is_aiohttp_ctor(expr: &Expr) -> bool {
    matches!(expr, Expr::Call(_)) && callable_tail_in(expr, CTOR_NAMES)
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
        let m = module_from("from aiohttp import web\napp = web.Application()\n");
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
