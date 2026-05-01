use pyllow_extract::ast::Stmt;
use pyllow_extract::{base_class_tail_name, ParsedModule};
use pyllow_types::{FileId, ImportKind, PluginResult};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

pub const PLUGIN_NAME: &str = "sqlmodel";

const MODEL_BASES: &[&str] = &["SQLModel"];

pub fn discover(parsed: &FxHashMap<FileId, ParsedModule>) -> PluginResult {
    let entry_files: FxHashSet<FileId> = parsed
        .par_iter()
        .filter_map(|(id, module)| module_is_sqlmodel_entry(module).then_some(*id))
        .collect();
    PluginResult {
        plugin_name: PLUGIN_NAME.to_string(),
        entry_files,
        ..Default::default()
    }
}

fn module_is_sqlmodel_entry(module: &ParsedModule) -> bool {
    if !imports_sqlmodel(module) {
        return false;
    }
    module.suite.iter().any(stmt_marks_sqlmodel_entry)
}

fn imports_sqlmodel(module: &ParsedModule) -> bool {
    module.imports.iter().any(|i| {
        matches!(i.kind, ImportKind::Absolute)
            && (i.raw == "sqlmodel" || i.raw.starts_with("sqlmodel."))
    })
}

fn stmt_marks_sqlmodel_entry(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::ClassDef(c) => {
            if c.bases.iter().any(is_sqlmodel_base) {
                return true;
            }
            c.body.iter().any(stmt_marks_sqlmodel_entry)
        }
        Stmt::FunctionDef(f) => f.body.iter().any(stmt_marks_sqlmodel_entry),
        Stmt::AsyncFunctionDef(f) => f.body.iter().any(stmt_marks_sqlmodel_entry),
        Stmt::If(s) => {
            s.body.iter().any(stmt_marks_sqlmodel_entry)
                || s.orelse.iter().any(stmt_marks_sqlmodel_entry)
        }
        _ => false,
    }
}

fn is_sqlmodel_base(expr: &pyllow_extract::ast::Expr) -> bool {
    base_class_tail_name(expr)
        .map(|n| MODEL_BASES.contains(&n))
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
    fn detects_sqlmodel_subclass() {
        let m = module_from(
            "from sqlmodel import SQLModel, Field\n\
             class Hero(SQLModel, table=True):\n    name: str\n",
        );
        assert!(module_is_sqlmodel_entry(&m));
    }

    #[test]
    fn detects_non_table_sqlmodel_subclass() {
        let m = module_from(
            "from sqlmodel import SQLModel\n\
             class HeroBase(SQLModel):\n    name: str\n",
        );
        assert!(module_is_sqlmodel_entry(&m));
    }

    #[test]
    fn ignores_module_without_sqlmodel_import() {
        let m = module_from(
            "from pydantic import BaseModel\n\
             class Hero(BaseModel):\n    name: str\n",
        );
        assert!(!module_is_sqlmodel_entry(&m));
    }

    #[test]
    fn ignores_sqlmodel_import_without_subclass() {
        let m = module_from("from sqlmodel import Session\n\nx = 1\n");
        assert!(!module_is_sqlmodel_entry(&m));
    }
}
