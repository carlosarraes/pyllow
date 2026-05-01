use pyllow_extract::ast::{Expr, Stmt};
use pyllow_extract::walker::walk_stmts;
use pyllow_extract::{base_class_tail_in, callable_tail_in, has_top_level_import, ParsedModule};
use pyllow_types::{FileId, PluginResult};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

pub const PLUGIN_NAME: &str = "flask";

const CTOR_NAMES: &[&str] = &["Flask", "Blueprint"];

const ROUTE_DECORATORS: &[&str] = &[
    // HTTP routing
    "route",
    "get",
    "post",
    "put",
    "delete",
    "patch",
    // request/response lifecycle
    "before_request",
    "before_first_request",
    "after_request",
    "teardown_request",
    "teardown_appcontext",
    "errorhandler",
    "context_processor",
    "url_value_preprocessor",
    "url_defaults",
    // CLI integration (`@app.cli.command`, `@bp.cli.command`)
    "command",
];

const VIEW_BASES: &[&str] = &["View", "MethodView"];

pub fn discover(parsed: &FxHashMap<FileId, ParsedModule>) -> PluginResult {
    let entry_files: FxHashSet<FileId> = parsed
        .par_iter()
        .filter_map(|(id, module)| module_is_flask_entry(module).then_some(*id))
        .collect();
    PluginResult {
        plugin_name: PLUGIN_NAME.to_string(),
        entry_files,
        ..Default::default()
    }
}

fn module_is_flask_entry(module: &ParsedModule) -> bool {
    if !has_top_level_import(module, &["flask"]) {
        return false;
    }
    let mut found = false;
    walk_stmts(&module.suite, &mut |stmt: &Stmt| {
        if found {
            return;
        }
        let hit = match stmt {
            Stmt::ClassDef(c) => c.bases.iter().any(|b| base_class_tail_in(b, VIEW_BASES)),
            Stmt::Assign(a) => is_flask_ctor(&a.value),
            Stmt::AnnAssign(a) => a.value.as_deref().map(is_flask_ctor).unwrap_or(false),
            Stmt::Expr(e) => is_flask_ctor(&e.value),
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

fn is_flask_ctor(expr: &Expr) -> bool {
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
    fn detects_flask_app_ctor() {
        let m = module_from("from flask import Flask\napp = Flask(__name__)\n");
        assert!(module_is_flask_entry(&m));
    }

    #[test]
    fn detects_blueprint_ctor() {
        let m = module_from("from flask import Blueprint\nbp = Blueprint('users', __name__)\n");
        assert!(module_is_flask_entry(&m));
    }

    #[test]
    fn detects_route_decorator() {
        let m = module_from(
            "from flask import Flask\napp = ...\n\
             @app.route('/')\ndef index():\n    return 'hi'\n",
        );
        assert!(module_is_flask_entry(&m));
    }

    #[test]
    fn detects_method_view_subclass() {
        let m = module_from(
            "from flask.views import MethodView\n\
             class UsersAPI(MethodView):\n    def get(self):\n        return []\n",
        );
        assert!(module_is_flask_entry(&m));
    }

    #[test]
    fn detects_errorhandler_decorator() {
        let m = module_from(
            "from flask import Flask\napp = ...\n\
             @app.errorhandler(404)\ndef nf(e):\n    return 'nf', 404\n",
        );
        assert!(module_is_flask_entry(&m));
    }

    #[test]
    fn detects_cli_command_decorator() {
        let m = module_from(
            "from flask import Flask\napp = ...\n\
             @app.cli.command('seed')\ndef seed():\n    pass\n",
        );
        assert!(module_is_flask_entry(&m));
    }

    #[test]
    fn ignores_module_without_flask_import() {
        let m = module_from(
            "def route(p):\n    def inner(f): return f\n    return inner\n\
             @route('/')\ndef hi(): pass\n",
        );
        assert!(!module_is_flask_entry(&m));
    }

    #[test]
    fn ignores_flask_import_without_use() {
        let m = module_from("from flask import current_app\nx = 1\n");
        assert!(!module_is_flask_entry(&m));
    }
}
