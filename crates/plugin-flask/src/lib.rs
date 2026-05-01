use pyllow_extract::ast::{self, Expr, Stmt};
use pyllow_extract::{base_class_tail_name, callable_tail_name, ParsedModule};
use pyllow_types::{FileId, ImportKind, PluginResult};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

pub const PLUGIN_NAME: &str = "flask";

const CTOR_NAMES: &[&str] = &["Flask", "Blueprint"];

const ROUTE_DECORATORS: &[&str] = &[
    // HTTP routing
    "route", "get", "post", "put", "delete", "patch",
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
    if !imports_flask(module) {
        return false;
    }
    module.suite.iter().any(stmt_marks_flask_entry)
}

fn imports_flask(module: &ParsedModule) -> bool {
    module.imports.iter().any(|i| {
        matches!(i.kind, ImportKind::Absolute)
            && (i.raw == "flask" || i.raw.starts_with("flask."))
    })
}

fn stmt_marks_flask_entry(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::ClassDef(c) => {
            if c.bases.iter().any(is_view_base) {
                return true;
            }
            c.body.iter().any(stmt_marks_flask_entry)
        }
        Stmt::Assign(a) => is_flask_ctor(&a.value),
        Stmt::AnnAssign(a) => a.value.as_deref().map(is_flask_ctor).unwrap_or(false),
        Stmt::Expr(e) => is_flask_ctor(&e.value),
        Stmt::FunctionDef(f) => {
            f.decorator_list.iter().any(is_flask_decorator)
                || f.body.iter().any(stmt_marks_flask_entry)
        }
        Stmt::AsyncFunctionDef(f) => {
            f.decorator_list.iter().any(is_flask_decorator)
                || f.body.iter().any(stmt_marks_flask_entry)
        }
        Stmt::If(s) => {
            s.body.iter().any(stmt_marks_flask_entry)
                || s.orelse.iter().any(stmt_marks_flask_entry)
        }
        Stmt::Try(s) => {
            s.body.iter().any(stmt_marks_flask_entry)
                || s.handlers.iter().any(|h| {
                    let ast::ExceptHandler::ExceptHandler(eh) = h;
                    eh.body.iter().any(stmt_marks_flask_entry)
                })
        }
        Stmt::With(s) => s.body.iter().any(stmt_marks_flask_entry),
        Stmt::AsyncWith(s) => s.body.iter().any(stmt_marks_flask_entry),
        _ => false,
    }
}

fn is_flask_ctor(expr: &Expr) -> bool {
    if !matches!(expr, Expr::Call(_)) {
        return false;
    }
    callable_tail_name(expr)
        .map(|n| CTOR_NAMES.contains(&n))
        .unwrap_or(false)
}

fn is_flask_decorator(expr: &Expr) -> bool {
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
    fn detects_flask_app_ctor() {
        let m = module_from("from flask import Flask\napp = Flask(__name__)\n");
        assert!(module_is_flask_entry(&m));
    }

    #[test]
    fn detects_blueprint_ctor() {
        let m = module_from(
            "from flask import Blueprint\nbp = Blueprint('users', __name__)\n",
        );
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
