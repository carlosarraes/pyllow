use pyllow_extract::ast::{self, Expr, Stmt};
use pyllow_extract::{base_class_tail_name, callable_tail_name, ParsedModule};
use pyllow_types::{FileId, ImportKind, PluginResult};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

pub const PLUGIN_NAME: &str = "beanie";

/// Beanie ODM base classes whose subclasses are framework-managed.
const MODEL_BASES: &[&str] = &["Document", "View", "UnionDoc", "TimeSeriesConfig"];

/// Validators / lifecycle hooks that mark methods as live.
const HOOK_DECORATORS: &[&str] = &[
    "before_event",
    "after_event",
    "Insert",
    "Replace",
    "Save",
    "ValidateOnSave",
    "Delete",
];

pub fn discover(parsed: &FxHashMap<FileId, ParsedModule>) -> PluginResult {
    let entry_files: FxHashSet<FileId> = parsed
        .par_iter()
        .filter_map(|(id, module)| module_is_beanie_entry(module).then_some(*id))
        .collect();
    PluginResult {
        plugin_name: PLUGIN_NAME.to_string(),
        entry_files,
        ..Default::default()
    }
}

fn module_is_beanie_entry(module: &ParsedModule) -> bool {
    if !imports_beanie(module) {
        return false;
    }
    module.suite.iter().any(stmt_marks_beanie_entry)
}

fn imports_beanie(module: &ParsedModule) -> bool {
    module.imports.iter().any(|i| {
        matches!(i.kind, ImportKind::Absolute)
            && (i.raw == "beanie" || i.raw.starts_with("beanie."))
    })
}

fn stmt_marks_beanie_entry(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::ClassDef(c) => {
            if c.bases.iter().any(is_beanie_base) {
                return true;
            }
            c.body.iter().any(stmt_marks_beanie_entry)
        }
        Stmt::FunctionDef(f) => {
            f.decorator_list.iter().any(is_hook_decorator)
                || f.body.iter().any(stmt_marks_beanie_entry)
        }
        Stmt::AsyncFunctionDef(f) => {
            f.decorator_list.iter().any(is_hook_decorator)
                || f.body.iter().any(stmt_marks_beanie_entry)
        }
        Stmt::If(s) => {
            s.body.iter().any(stmt_marks_beanie_entry)
                || s.orelse.iter().any(stmt_marks_beanie_entry)
        }
        Stmt::Try(s) => {
            s.body.iter().any(stmt_marks_beanie_entry)
                || s.handlers.iter().any(|h| {
                    let ast::ExceptHandler::ExceptHandler(eh) = h;
                    eh.body.iter().any(stmt_marks_beanie_entry)
                })
        }
        Stmt::With(s) => s.body.iter().any(stmt_marks_beanie_entry),
        Stmt::AsyncWith(s) => s.body.iter().any(stmt_marks_beanie_entry),
        _ => false,
    }
}

fn is_beanie_base(expr: &Expr) -> bool {
    base_class_tail_name(expr)
        .map(|n| MODEL_BASES.contains(&n))
        .unwrap_or(false)
}

fn is_hook_decorator(expr: &Expr) -> bool {
    // Beanie's hook syntax: `@before_event(Insert)` (call form, where the
    // callee is the hook decorator and the arg is an event class). Also
    // recognize `@before_event` plain form for forward compat.
    callable_tail_name(expr)
        .map(|n| HOOK_DECORATORS.contains(&n))
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
    fn detects_document_subclass() {
        let m = parse("from beanie import Document\nclass User(Document):\n    name: str\n");
        assert!(module_is_beanie_entry(&m));
    }

    #[test]
    fn detects_view_subclass() {
        let m = parse("from beanie import View\nclass UserView(View):\n    name: str\n");
        assert!(module_is_beanie_entry(&m));
    }

    #[test]
    fn detects_uniondoc_subclass() {
        let m = parse("from beanie import UnionDoc\nclass AllDocs(UnionDoc):\n    pass\n");
        assert!(module_is_beanie_entry(&m));
    }

    #[test]
    fn detects_attribute_base() {
        let m = parse("import beanie\nclass User(beanie.Document):\n    name: str\n");
        assert!(module_is_beanie_entry(&m));
    }

    #[test]
    fn detects_before_event_hook() {
        let m = parse(
            "from beanie import Document, before_event, Insert\nclass User(Document):\n    @before_event(Insert)\n    async def normalize(self): pass\n",
        );
        assert!(module_is_beanie_entry(&m));
    }

    #[test]
    fn ignores_class_named_document_without_beanie_import() {
        let m = parse("class Document:\n    pass\nclass User(Document):\n    name: str\n");
        assert!(!module_is_beanie_entry(&m));
    }

    #[test]
    fn ignores_unrelated_module() {
        let m = parse("import os\ndef helper(): return 1\n");
        assert!(!module_is_beanie_entry(&m));
    }
}
