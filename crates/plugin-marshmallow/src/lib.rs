use pyllow_extract::ast::{Expr, Stmt};
use pyllow_extract::{base_class_tail_name, callable_tail_name, ParsedModule};
use pyllow_types::{FileId, ImportKind, PluginResult};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

pub const PLUGIN_NAME: &str = "marshmallow";

const SCHEMA_BASES: &[&str] = &["Schema", "SchemaOpts"];

const HOOK_DECORATORS: &[&str] = &[
    "validates",
    "validates_schema",
    "pre_load",
    "post_load",
    "pre_dump",
    "post_dump",
];

pub fn discover(parsed: &FxHashMap<FileId, ParsedModule>) -> PluginResult {
    let entry_files: FxHashSet<FileId> = parsed
        .par_iter()
        .filter_map(|(id, module)| module_is_marshmallow_entry(module).then_some(*id))
        .collect();
    PluginResult {
        plugin_name: PLUGIN_NAME.to_string(),
        entry_files,
        ..Default::default()
    }
}

fn module_is_marshmallow_entry(module: &ParsedModule) -> bool {
    if !imports_marshmallow(module) {
        return false;
    }
    module.suite.iter().any(stmt_marks_marshmallow_entry)
}

fn imports_marshmallow(module: &ParsedModule) -> bool {
    module.imports.iter().any(|i| {
        matches!(i.kind, ImportKind::Absolute)
            && (i.raw == "marshmallow" || i.raw.starts_with("marshmallow."))
    })
}

fn stmt_marks_marshmallow_entry(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::ClassDef(c) => {
            if c.bases.iter().any(is_schema_base) {
                return true;
            }
            c.body.iter().any(stmt_marks_marshmallow_entry)
        }
        Stmt::FunctionDef(f) => {
            f.decorator_list.iter().any(is_marshmallow_hook_decorator)
                || f.body.iter().any(stmt_marks_marshmallow_entry)
        }
        Stmt::AsyncFunctionDef(f) => {
            f.decorator_list.iter().any(is_marshmallow_hook_decorator)
                || f.body.iter().any(stmt_marks_marshmallow_entry)
        }
        Stmt::If(s) => {
            s.body.iter().any(stmt_marks_marshmallow_entry)
                || s.orelse.iter().any(stmt_marks_marshmallow_entry)
        }
        _ => false,
    }
}

fn is_schema_base(expr: &Expr) -> bool {
    base_class_tail_name(expr)
        .map(|n| SCHEMA_BASES.contains(&n))
        .unwrap_or(false)
}

fn is_marshmallow_hook_decorator(expr: &Expr) -> bool {
    callable_tail_name(expr)
        .map(|n| HOOK_DECORATORS.contains(&n))
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
    fn detects_schema_subclass() {
        let m = module_from(
            "from marshmallow import Schema, fields\n\
             class UserSchema(Schema):\n    name = fields.Str()\n",
        );
        assert!(module_is_marshmallow_entry(&m));
    }

    #[test]
    fn detects_validates_decorator() {
        let m = module_from(
            "from marshmallow import validates, ValidationError\n\
             @validates('name')\ndef _check(self, value):\n    pass\n",
        );
        assert!(module_is_marshmallow_entry(&m));
    }

    #[test]
    fn detects_pre_load_decorator() {
        let m = module_from(
            "from marshmallow import pre_load\n\
             @pre_load\ndef strip(self, data, **kw):\n    return data\n",
        );
        assert!(module_is_marshmallow_entry(&m));
    }

    #[test]
    fn ignores_module_without_marshmallow_import() {
        let m = module_from(
            "from pydantic import BaseModel\nclass UserSchema(BaseModel):\n    name: str\n",
        );
        assert!(!module_is_marshmallow_entry(&m));
    }

    #[test]
    fn ignores_marshmallow_import_without_schema_or_hook() {
        let m = module_from("import marshmallow\nx = 1\n");
        assert!(!module_is_marshmallow_entry(&m));
    }
}
