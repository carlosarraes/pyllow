use pyllow_extract::ast::{self, Expr, Stmt};
use pyllow_extract::ParsedModule;
use pyllow_types::{FileId, ImportKind, PluginResult};
use rustc_hash::{FxHashMap, FxHashSet};

pub const PLUGIN_NAME: &str = "prefect";

const FLOW_DECORATORS: &[&str] = &["flow", "task", "materialize"];

pub fn discover(parsed: &FxHashMap<FileId, ParsedModule>) -> PluginResult {
    let mut entry_files = FxHashSet::default();
    for (id, module) in parsed {
        if module_is_prefect_entry(module) {
            entry_files.insert(*id);
        }
    }
    PluginResult {
        plugin_name: PLUGIN_NAME.to_string(),
        entry_files,
        ..Default::default()
    }
}

fn module_is_prefect_entry(module: &ParsedModule) -> bool {
    if !imports_prefect(module) {
        return false;
    }
    module.suite.iter().any(stmt_has_prefect_decorator)
}

fn imports_prefect(module: &ParsedModule) -> bool {
    module.imports.iter().any(|i| {
        matches!(i.kind, ImportKind::Absolute)
            && (i.raw == "prefect" || i.raw.starts_with("prefect."))
    })
}

fn stmt_has_prefect_decorator(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::FunctionDef(f) => {
            f.decorator_list.iter().any(is_prefect_decorator)
                || f.body.iter().any(stmt_has_prefect_decorator)
        }
        Stmt::AsyncFunctionDef(f) => {
            f.decorator_list.iter().any(is_prefect_decorator)
                || f.body.iter().any(stmt_has_prefect_decorator)
        }
        Stmt::ClassDef(c) => c.body.iter().any(stmt_has_prefect_decorator),
        Stmt::If(s) => {
            s.body.iter().any(stmt_has_prefect_decorator)
                || s.orelse.iter().any(stmt_has_prefect_decorator)
        }
        Stmt::Try(s) => {
            s.body.iter().any(stmt_has_prefect_decorator)
                || s.handlers.iter().any(|h| {
                    let ast::ExceptHandler::ExceptHandler(eh) = h;
                    eh.body.iter().any(stmt_has_prefect_decorator)
                })
                || s.orelse.iter().any(stmt_has_prefect_decorator)
                || s.finalbody.iter().any(stmt_has_prefect_decorator)
        }
        Stmt::With(s) => s.body.iter().any(stmt_has_prefect_decorator),
        Stmt::AsyncWith(s) => s.body.iter().any(stmt_has_prefect_decorator),
        _ => false,
    }
}

fn is_prefect_decorator(expr: &Expr) -> bool {
    let target = match expr {
        Expr::Call(c) => c.func.as_ref(),
        other => other,
    };
    match target {
        Expr::Name(n) => FLOW_DECORATORS.contains(&n.id.as_str()),
        Expr::Attribute(a) => FLOW_DECORATORS.contains(&a.attr.as_str()),
        _ => false,
    }
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
    fn detects_flow_decorator_bare() {
        let m = parse(
            "from prefect import flow\n\n@flow\ndef ingest():\n    pass\n",
        );
        assert!(module_is_prefect_entry(&m));
    }

    #[test]
    fn detects_flow_decorator_called() {
        let m = parse(
            "from prefect import flow\n\n@flow(name=\"ingest\")\ndef run():\n    pass\n",
        );
        assert!(module_is_prefect_entry(&m));
    }

    #[test]
    fn detects_qualified_decorator() {
        let m = parse(
            "import prefect\n\n@prefect.task\ndef step():\n    pass\n",
        );
        assert!(module_is_prefect_entry(&m));
    }

    #[test]
    fn detects_task_inside_flow() {
        let m = parse(
            "from prefect import flow, task\n\n@flow\ndef pipeline():\n    @task\n    def inner():\n        pass\n    inner()\n",
        );
        assert!(module_is_prefect_entry(&m));
    }

    #[test]
    fn ignores_decorators_without_prefect_import() {
        let m = parse(
            "def flow():\n    pass\n\n@flow\ndef wrong():\n    pass\n",
        );
        assert!(!module_is_prefect_entry(&m));
    }

    #[test]
    fn ignores_unrelated_modules() {
        let m = parse("import os\n\ndef helper():\n    pass\n");
        assert!(!module_is_prefect_entry(&m));
    }
}
