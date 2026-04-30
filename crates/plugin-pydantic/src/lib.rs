use pyllow_extract::ast::{self, Expr, Stmt};
use pyllow_extract::{base_class_tail_name, callable_tail_name, ParsedModule};
use pyllow_types::{FileId, ImportKind, PluginResult};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

pub const PLUGIN_NAME: &str = "pydantic";

const MODEL_BASES: &[&str] = &[
    "BaseModel",
    "BaseSettings",
    "GenericModel",
    "RootModel",
];

const VALIDATOR_DECORATORS: &[&str] = &[
    "field_validator",
    "model_validator",
    "validator",
    "root_validator",
    "computed_field",
];

pub fn discover(parsed: &FxHashMap<FileId, ParsedModule>) -> PluginResult {
    let entry_files: FxHashSet<FileId> = parsed
        .par_iter()
        .filter_map(|(id, module)| module_is_pydantic_entry(module).then_some(*id))
        .collect();
    PluginResult {
        plugin_name: PLUGIN_NAME.to_string(),
        entry_files,
        ..Default::default()
    }
}

fn module_is_pydantic_entry(module: &ParsedModule) -> bool {
    if !imports_pydantic(module) {
        return false;
    }
    module.suite.iter().any(stmt_marks_pydantic_entry)
}

fn imports_pydantic(module: &ParsedModule) -> bool {
    module.imports.iter().any(|i| {
        matches!(i.kind, ImportKind::Absolute)
            && (i.raw == "pydantic"
                || i.raw.starts_with("pydantic.")
                || i.raw == "pydantic_settings"
                || i.raw.starts_with("pydantic_settings."))
    })
}

fn stmt_marks_pydantic_entry(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::ClassDef(c) => {
            if c.bases.iter().any(is_pydantic_base) {
                return true;
            }
            if c.body.iter().any(stmt_marks_pydantic_entry) {
                return true;
            }
            false
        }
        Stmt::FunctionDef(f) => {
            f.decorator_list.iter().any(is_validator_decorator)
                || f.body.iter().any(stmt_marks_pydantic_entry)
        }
        Stmt::AsyncFunctionDef(f) => {
            f.decorator_list.iter().any(is_validator_decorator)
                || f.body.iter().any(stmt_marks_pydantic_entry)
        }
        Stmt::If(s) => {
            s.body.iter().any(stmt_marks_pydantic_entry)
                || s.orelse.iter().any(stmt_marks_pydantic_entry)
        }
        Stmt::Try(s) => {
            s.body.iter().any(stmt_marks_pydantic_entry)
                || s.handlers.iter().any(|h| {
                    let ast::ExceptHandler::ExceptHandler(eh) = h;
                    eh.body.iter().any(stmt_marks_pydantic_entry)
                })
                || s.orelse.iter().any(stmt_marks_pydantic_entry)
                || s.finalbody.iter().any(stmt_marks_pydantic_entry)
        }
        Stmt::With(s) => s.body.iter().any(stmt_marks_pydantic_entry),
        Stmt::AsyncWith(s) => s.body.iter().any(stmt_marks_pydantic_entry),
        _ => false,
    }
}

fn is_pydantic_base(expr: &Expr) -> bool {
    base_class_tail_name(expr)
        .map(|n| MODEL_BASES.contains(&n))
        .unwrap_or(false)
}

fn is_validator_decorator(expr: &Expr) -> bool {
    callable_tail_name(expr)
        .map(|n| VALIDATOR_DECORATORS.contains(&n))
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
    fn detects_basemodel_subclass() {
        let m = parse(
            "from pydantic import BaseModel\nclass User(BaseModel):\n    name: str\n    age: int = 0\n",
        );
        assert!(module_is_pydantic_entry(&m));
    }

    #[test]
    fn detects_basesettings_subclass() {
        let m = parse(
            "from pydantic_settings import BaseSettings\nclass Config(BaseSettings):\n    api_key: str\n",
        );
        assert!(module_is_pydantic_entry(&m));
    }

    #[test]
    fn detects_attribute_base() {
        let m = parse(
            "import pydantic\nclass User(pydantic.BaseModel):\n    name: str\n",
        );
        assert!(module_is_pydantic_entry(&m));
    }

    #[test]
    fn detects_generic_base() {
        let m = parse(
            "from pydantic import BaseModel\nfrom typing import TypeVar, Generic\nT = TypeVar(\"T\")\nclass Wrapper(BaseModel, Generic[T]):\n    value: T\n",
        );
        assert!(module_is_pydantic_entry(&m));
    }

    #[test]
    fn detects_field_validator() {
        let m = parse(
            "from pydantic import BaseModel, field_validator\n\nclass X(BaseModel):\n    name: str\n    @field_validator(\"name\")\n    @classmethod\n    def upper(cls, v):\n        return v.upper()\n",
        );
        assert!(module_is_pydantic_entry(&m));
    }

    #[test]
    fn ignores_module_without_pydantic_import() {
        let m = parse(
            "class BaseModel:\n    pass\nclass Custom(BaseModel):\n    name: str\n",
        );
        assert!(!module_is_pydantic_entry(&m));
    }

    #[test]
    fn ignores_unrelated_module() {
        let m = parse("import os\ndef helper():\n    return os.environ.get(\"X\")\n");
        assert!(!module_is_pydantic_entry(&m));
    }
}
