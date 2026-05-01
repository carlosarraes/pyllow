use pyllow_extract::ast::{self, Expr, Stmt};
use pyllow_extract::ParsedModule;
use pyllow_types::{FileId, ImportKind, PluginResult};
use rustc_hash::{FxHashMap, FxHashSet};

pub const PLUGIN_NAME: &str = "click";

const COMMAND_DECORATORS: &[&str] = &["command", "group", "callback"];

const APP_CTORS: &[&str] = &["Typer", "Group"];

pub fn discover(parsed: &FxHashMap<FileId, ParsedModule>) -> PluginResult {
    let mut entry_files = FxHashSet::default();
    for (id, module) in parsed {
        if module_is_cli_entry(module) {
            entry_files.insert(*id);
        }
    }
    PluginResult {
        plugin_name: PLUGIN_NAME.to_string(),
        entry_files,
        ..Default::default()
    }
}

fn module_is_cli_entry(module: &ParsedModule) -> bool {
    if module.suite.iter().any(stmt_has_app_ctor) {
        return true;
    }
    if imports_click_or_typer(module) && module.suite.iter().any(stmt_has_command_decorator) {
        return true;
    }
    false
}

fn imports_click_or_typer(module: &ParsedModule) -> bool {
    module.imports.iter().any(|i| {
        if !matches!(i.kind, ImportKind::Absolute) {
            return false;
        }
        i.raw == "click"
            || i.raw.starts_with("click.")
            || i.raw == "typer"
            || i.raw.starts_with("typer.")
    })
}

fn stmt_has_app_ctor(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Assign(a) => is_app_ctor_call(&a.value),
        Stmt::AnnAssign(a) => a.value.as_deref().map(is_app_ctor_call).unwrap_or(false),
        Stmt::FunctionDef(f) => f.body.iter().any(stmt_has_app_ctor),
        Stmt::AsyncFunctionDef(f) => f.body.iter().any(stmt_has_app_ctor),
        Stmt::ClassDef(c) => c.body.iter().any(stmt_has_app_ctor),
        Stmt::If(s) => {
            s.body.iter().any(stmt_has_app_ctor) || s.orelse.iter().any(stmt_has_app_ctor)
        }
        Stmt::Try(s) => {
            s.body.iter().any(stmt_has_app_ctor)
                || s.handlers.iter().any(|h| {
                    let ast::ExceptHandler::ExceptHandler(eh) = h;
                    eh.body.iter().any(stmt_has_app_ctor)
                })
                || s.orelse.iter().any(stmt_has_app_ctor)
                || s.finalbody.iter().any(stmt_has_app_ctor)
        }
        Stmt::With(s) => s.body.iter().any(stmt_has_app_ctor),
        Stmt::AsyncWith(s) => s.body.iter().any(stmt_has_app_ctor),
        _ => false,
    }
}

fn is_app_ctor_call(expr: &Expr) -> bool {
    let Expr::Call(call) = expr else {
        return false;
    };
    let name = match call.func.as_ref() {
        Expr::Name(n) => n.id.as_str(),
        Expr::Attribute(a) => a.attr.as_str(),
        _ => return false,
    };
    APP_CTORS.contains(&name)
}

fn stmt_has_command_decorator(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::FunctionDef(f) => {
            f.decorator_list.iter().any(is_command_decorator)
                || f.body.iter().any(stmt_has_command_decorator)
        }
        Stmt::AsyncFunctionDef(f) => {
            f.decorator_list.iter().any(is_command_decorator)
                || f.body.iter().any(stmt_has_command_decorator)
        }
        Stmt::ClassDef(c) => c.body.iter().any(stmt_has_command_decorator),
        Stmt::If(s) => {
            s.body.iter().any(stmt_has_command_decorator)
                || s.orelse.iter().any(stmt_has_command_decorator)
        }
        Stmt::Try(s) => {
            s.body.iter().any(stmt_has_command_decorator)
                || s.handlers.iter().any(|h| {
                    let ast::ExceptHandler::ExceptHandler(eh) = h;
                    eh.body.iter().any(stmt_has_command_decorator)
                })
                || s.orelse.iter().any(stmt_has_command_decorator)
                || s.finalbody.iter().any(stmt_has_command_decorator)
        }
        Stmt::With(s) => s.body.iter().any(stmt_has_command_decorator),
        Stmt::AsyncWith(s) => s.body.iter().any(stmt_has_command_decorator),
        _ => false,
    }
}

fn is_command_decorator(expr: &Expr) -> bool {
    let target = match expr {
        Expr::Call(c) => c.func.as_ref(),
        other => other,
    };
    match target {
        Expr::Name(n) => COMMAND_DECORATORS.contains(&n.id.as_str()),
        Expr::Attribute(a) => COMMAND_DECORATORS.contains(&a.attr.as_str()),
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
    fn detects_click_command_decorator() {
        let m = parse("import click\n\n@click.command()\ndef cli():\n    pass\n");
        assert!(module_is_cli_entry(&m));
    }

    #[test]
    fn detects_click_group_decorator() {
        let m = parse("from click import group\n\n@group()\ndef cli():\n    pass\n");
        assert!(module_is_cli_entry(&m));
    }

    #[test]
    fn detects_typer_app_ctor() {
        let m = parse(
            "import typer\napp = typer.Typer()\n\n@app.command()\ndef hello(name: str):\n    pass\n",
        );
        assert!(module_is_cli_entry(&m));
    }

    #[test]
    fn detects_typer_app_command_decorator() {
        let m = parse(
            "from typer import Typer\napp = Typer()\n\n@app.command()\ndef run():\n    pass\n",
        );
        assert!(module_is_cli_entry(&m));
    }

    #[test]
    fn ignores_decorators_without_click_import() {
        let m = parse("def command(fn):\n    return fn\n\n@command\ndef nope():\n    pass\n");
        assert!(!module_is_cli_entry(&m));
    }

    #[test]
    fn ignores_unrelated_modules() {
        let m = parse("import os\n\ndef helper():\n    pass\n");
        assert!(!module_is_cli_entry(&m));
    }
}
