use pyllow_extract::ast::{self, Expr, Stmt};
use pyllow_extract::{callable_tail_name, ParsedModule};
use pyllow_types::{FileId, ImportKind, PluginResult};
use rustc_hash::{FxHashMap, FxHashSet};

pub const PLUGIN_NAME: &str = "celery";

const TASK_DECORATORS: &[&str] = &["task", "shared_task", "periodic_task"];
const CTOR_NAMES: &[&str] = &["Celery"];

pub fn discover(parsed: &FxHashMap<FileId, ParsedModule>) -> PluginResult {
    let mut entry_files = FxHashSet::default();
    for (id, module) in parsed {
        if module_is_celery_entry(module) {
            entry_files.insert(*id);
        }
    }
    PluginResult {
        plugin_name: PLUGIN_NAME.to_string(),
        entry_files,
        ..Default::default()
    }
}

fn module_is_celery_entry(module: &ParsedModule) -> bool {
    if !imports_celery(module) {
        return false;
    }
    if module.suite.iter().any(stmt_has_celery_ctor) {
        return true;
    }
    module.suite.iter().any(stmt_has_task_decorator)
}

fn imports_celery(module: &ParsedModule) -> bool {
    module.imports.iter().any(|i| {
        matches!(i.kind, ImportKind::Absolute)
            && (i.raw == "celery" || i.raw.starts_with("celery."))
    })
}

fn stmt_has_celery_ctor(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Assign(a) => is_celery_call(&a.value),
        Stmt::AnnAssign(a) => a.value.as_deref().map(is_celery_call).unwrap_or(false),
        Stmt::Expr(e) => is_celery_call(&e.value),
        Stmt::FunctionDef(f) => f.body.iter().any(stmt_has_celery_ctor),
        Stmt::AsyncFunctionDef(f) => f.body.iter().any(stmt_has_celery_ctor),
        Stmt::If(s) => {
            s.body.iter().any(stmt_has_celery_ctor)
                || s.orelse.iter().any(stmt_has_celery_ctor)
        }
        Stmt::Try(s) => {
            s.body.iter().any(stmt_has_celery_ctor)
                || s.handlers.iter().any(|h| {
                    let ast::ExceptHandler::ExceptHandler(eh) = h;
                    eh.body.iter().any(stmt_has_celery_ctor)
                })
        }
        Stmt::With(s) => s.body.iter().any(stmt_has_celery_ctor),
        Stmt::AsyncWith(s) => s.body.iter().any(stmt_has_celery_ctor),
        _ => false,
    }
}

fn is_celery_call(expr: &Expr) -> bool {
    if !matches!(expr, Expr::Call(_)) {
        return false;
    }
    callable_tail_name(expr)
        .map(|n| CTOR_NAMES.contains(&n))
        .unwrap_or(false)
}

fn stmt_has_task_decorator(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::FunctionDef(f) => {
            f.decorator_list.iter().any(is_task_decorator)
                || f.body.iter().any(stmt_has_task_decorator)
        }
        Stmt::AsyncFunctionDef(f) => {
            f.decorator_list.iter().any(is_task_decorator)
                || f.body.iter().any(stmt_has_task_decorator)
        }
        Stmt::ClassDef(c) => c.body.iter().any(stmt_has_task_decorator),
        Stmt::If(s) => {
            s.body.iter().any(stmt_has_task_decorator)
                || s.orelse.iter().any(stmt_has_task_decorator)
        }
        Stmt::Try(s) => {
            s.body.iter().any(stmt_has_task_decorator)
                || s.handlers.iter().any(|h| {
                    let ast::ExceptHandler::ExceptHandler(eh) = h;
                    eh.body.iter().any(stmt_has_task_decorator)
                })
        }
        Stmt::With(s) => s.body.iter().any(stmt_has_task_decorator),
        Stmt::AsyncWith(s) => s.body.iter().any(stmt_has_task_decorator),
        _ => false,
    }
}

fn is_task_decorator(expr: &Expr) -> bool {
    callable_tail_name(expr)
        .map(|n| TASK_DECORATORS.contains(&n))
        .unwrap_or(false)
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
    fn detects_celery_constructor() {
        let m = parse(
            "from celery import Celery\napp = Celery(\"tasks\", broker=\"redis://\")\n",
        );
        assert!(module_is_celery_entry(&m));
    }

    #[test]
    fn detects_app_task_decorator() {
        let m = parse(
            "from celery import Celery\nfrom .app import app\n@app.task\ndef send_email(to):\n    pass\n",
        );
        assert!(module_is_celery_entry(&m));
    }

    #[test]
    fn detects_shared_task_decorator() {
        let m = parse(
            "from celery import shared_task\n@shared_task\ndef compute():\n    pass\n",
        );
        assert!(module_is_celery_entry(&m));
    }

    #[test]
    fn detects_decorator_with_call_form() {
        let m = parse(
            "from celery import shared_task\n@shared_task(bind=True)\ndef retryable(self):\n    pass\n",
        );
        assert!(module_is_celery_entry(&m));
    }

    #[test]
    fn ignores_decorator_without_celery_import() {
        let m = parse(
            "from rq import Queue\n@Queue.task\ndef something():\n    pass\n",
        );
        assert!(!module_is_celery_entry(&m));
    }

    #[test]
    fn ignores_unrelated_module() {
        let m = parse("import os\ndef helper(): return 1\n");
        assert!(!module_is_celery_entry(&m));
    }
}
