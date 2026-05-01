use pyllow_extract::ast::Stmt;
use pyllow_extract::walker::walk_stmts;
use pyllow_extract::{base_class_tail_in, callable_tail_in, has_top_level_import, ParsedModule};
use pyllow_types::{FileId, PluginResult};
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
    if !has_top_level_import(module, &["marshmallow"]) {
        return false;
    }
    let mut found = false;
    walk_stmts(&module.suite, &mut |stmt: &Stmt| {
        if found {
            return;
        }
        match stmt {
            Stmt::ClassDef(c) => {
                if c.bases.iter().any(|b| base_class_tail_in(b, SCHEMA_BASES)) {
                    found = true;
                }
            }
            Stmt::FunctionDef(f) => {
                if f.decorator_list
                    .iter()
                    .any(|d| callable_tail_in(d, HOOK_DECORATORS))
                {
                    found = true;
                }
            }
            Stmt::AsyncFunctionDef(f) => {
                if f.decorator_list
                    .iter()
                    .any(|d| callable_tail_in(d, HOOK_DECORATORS))
                {
                    found = true;
                }
            }
            _ => {}
        }
    });
    found
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
