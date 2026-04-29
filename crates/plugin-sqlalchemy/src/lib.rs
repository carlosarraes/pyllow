use pyllow_extract::ast::{self, Expr, Stmt};
use pyllow_extract::ParsedModule;
use pyllow_types::{FileId, ImportKind, PluginResult};
use rustc_hash::{FxHashMap, FxHashSet};

pub const PLUGIN_NAME: &str = "sqlalchemy";

/// Class names that signal a declarative ORM model when used as a base.
const MODEL_BASES: &[&str] = &[
    "DeclarativeBase",
    "Base",
    "Model",       // Flask-SQLAlchemy
    "SQLModel",    // sqlmodel
    "MappedAsDataclass",
];

/// Names that indicate a `__tablename__` attribute (mapper signal).
const TABLENAME_ATTR: &str = "__tablename__";

/// Function calls that bind columns / relationships at class scope.
const COLUMN_FACTORIES: &[&str] = &["mapped_column", "Column", "relationship"];

pub fn discover(parsed: &FxHashMap<FileId, ParsedModule>) -> PluginResult {
    let mut entry_files = FxHashSet::default();
    for (id, module) in parsed {
        if module_is_sqlalchemy_entry(module) {
            entry_files.insert(*id);
        }
    }
    PluginResult {
        plugin_name: PLUGIN_NAME.to_string(),
        entry_files,
        ..Default::default()
    }
}

fn module_is_sqlalchemy_entry(module: &ParsedModule) -> bool {
    if !imports_sqlalchemy(module) {
        return false;
    }
    module.suite.iter().any(stmt_marks_orm_model)
}

fn imports_sqlalchemy(module: &ParsedModule) -> bool {
    module.imports.iter().any(|i| {
        if !matches!(i.kind, ImportKind::Absolute) {
            return false;
        }
        i.raw == "sqlalchemy"
            || i.raw.starts_with("sqlalchemy.")
            || i.raw == "sqlmodel"
            || i.raw.starts_with("sqlmodel.")
            || i.raw == "flask_sqlalchemy"
            || i.raw.starts_with("flask_sqlalchemy.")
    })
}

fn stmt_marks_orm_model(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::ClassDef(c) => {
            if c.bases.iter().any(is_orm_base) {
                return true;
            }
            if class_has_tablename_or_columns(&c.body) {
                return true;
            }
            c.body.iter().any(stmt_marks_orm_model)
        }
        Stmt::FunctionDef(f) => f.body.iter().any(stmt_marks_orm_model),
        Stmt::AsyncFunctionDef(f) => f.body.iter().any(stmt_marks_orm_model),
        Stmt::If(s) => {
            s.body.iter().any(stmt_marks_orm_model)
                || s.orelse.iter().any(stmt_marks_orm_model)
        }
        Stmt::Try(s) => {
            s.body.iter().any(stmt_marks_orm_model)
                || s.handlers.iter().any(|h| {
                    let ast::ExceptHandler::ExceptHandler(eh) = h;
                    eh.body.iter().any(stmt_marks_orm_model)
                })
                || s.orelse.iter().any(stmt_marks_orm_model)
                || s.finalbody.iter().any(stmt_marks_orm_model)
        }
        Stmt::With(s) => s.body.iter().any(stmt_marks_orm_model),
        Stmt::AsyncWith(s) => s.body.iter().any(stmt_marks_orm_model),
        _ => false,
    }
}

fn is_orm_base(expr: &Expr) -> bool {
    let name = match expr {
        Expr::Name(n) => n.id.as_str(),
        Expr::Attribute(a) => a.attr.as_str(),
        Expr::Call(c) => match c.func.as_ref() {
            Expr::Name(n) => n.id.as_str(),
            Expr::Attribute(a) => a.attr.as_str(),
            _ => return false,
        },
        Expr::Subscript(s) => match s.value.as_ref() {
            Expr::Name(n) => n.id.as_str(),
            Expr::Attribute(a) => a.attr.as_str(),
            _ => return false,
        },
        _ => return false,
    };
    MODEL_BASES.contains(&name) || name == "declarative_base"
}

fn class_has_tablename_or_columns(body: &[Stmt]) -> bool {
    for stmt in body {
        match stmt {
            Stmt::Assign(a) => {
                for target in &a.targets {
                    if let Expr::Name(n) = target {
                        if n.id.as_str() == TABLENAME_ATTR {
                            return true;
                        }
                    }
                }
                if is_column_factory_call(&a.value) {
                    return true;
                }
            }
            Stmt::AnnAssign(a) => {
                if let Expr::Name(n) = a.target.as_ref() {
                    if n.id.as_str() == TABLENAME_ATTR {
                        return true;
                    }
                }
                if let Some(value) = &a.value {
                    if is_column_factory_call(value) {
                        return true;
                    }
                }
                if is_mapped_annotation(&a.annotation) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn is_column_factory_call(expr: &Expr) -> bool {
    let Expr::Call(call) = expr else { return false };
    let name = match call.func.as_ref() {
        Expr::Name(n) => n.id.as_str(),
        Expr::Attribute(a) => a.attr.as_str(),
        _ => return false,
    };
    COLUMN_FACTORIES.contains(&name)
}

fn is_mapped_annotation(expr: &Expr) -> bool {
    // `name: Mapped[int]` or `name: sa.orm.Mapped[int]`
    let target = match expr {
        Expr::Subscript(s) => s.value.as_ref(),
        other => other,
    };
    let name = match target {
        Expr::Name(n) => n.id.as_str(),
        Expr::Attribute(a) => a.attr.as_str(),
        _ => return false,
    };
    name == "Mapped"
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
    fn detects_declarative_base_subclass() {
        let m = parse(
            "from sqlalchemy.orm import DeclarativeBase\nclass Base(DeclarativeBase):\n    pass\nclass User(Base):\n    __tablename__ = \"users\"\n",
        );
        assert!(module_is_sqlalchemy_entry(&m));
    }

    #[test]
    fn detects_mapped_column_class() {
        let m = parse(
            "from sqlalchemy.orm import Mapped, mapped_column\nclass User:\n    id: Mapped[int] = mapped_column(primary_key=True)\n    name: Mapped[str]\n",
        );
        assert!(module_is_sqlalchemy_entry(&m));
    }

    #[test]
    fn detects_legacy_column_class() {
        let m = parse(
            "from sqlalchemy import Column, Integer\nfrom sqlalchemy.ext.declarative import declarative_base\nBase = declarative_base()\nclass User(Base):\n    id = Column(Integer, primary_key=True)\n",
        );
        assert!(module_is_sqlalchemy_entry(&m));
    }

    #[test]
    fn detects_sqlmodel() {
        let m = parse(
            "from sqlmodel import SQLModel, Field\nclass User(SQLModel, table=True):\n    id: int = Field(primary_key=True)\n",
        );
        assert!(module_is_sqlalchemy_entry(&m));
    }

    #[test]
    fn detects_flask_sqlalchemy_model() {
        let m = parse(
            "from flask_sqlalchemy import SQLAlchemy\ndb = SQLAlchemy()\nclass User(db.Model):\n    __tablename__ = \"users\"\n",
        );
        assert!(module_is_sqlalchemy_entry(&m));
    }

    #[test]
    fn ignores_class_named_base_without_import() {
        let m = parse("class Base:\n    pass\nclass Inner(Base):\n    __tablename__ = \"x\"\n");
        assert!(!module_is_sqlalchemy_entry(&m));
    }

    #[test]
    fn ignores_unrelated_module() {
        let m = parse("import os\ndef helper():\n    return 1\n");
        assert!(!module_is_sqlalchemy_entry(&m));
    }
}
