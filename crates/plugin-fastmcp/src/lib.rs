use pyllow_extract::ast::{self, Expr, Stmt};
use pyllow_extract::{callable_tail_name, ParsedModule};
use pyllow_types::{FileId, ImportKind, PluginResult};
use rustc_hash::{FxHashMap, FxHashSet};

pub const PLUGIN_NAME: &str = "fastmcp";

const CTOR_NAME: &str = "FastMCP";

const REGISTRATION_DECORATORS: &[&str] = &["tool", "resource", "prompt"];

pub fn discover(parsed: &FxHashMap<FileId, ParsedModule>) -> PluginResult {
    let mut entry_files = FxHashSet::default();
    for (id, module) in parsed {
        if module_is_fastmcp_entry(module) {
            entry_files.insert(*id);
        }
    }
    PluginResult {
        plugin_name: PLUGIN_NAME.to_string(),
        entry_files,
        ..Default::default()
    }
}

fn module_is_fastmcp_entry(module: &ParsedModule) -> bool {
    if module.suite.iter().any(stmt_has_fastmcp_ctor) {
        return true;
    }
    if imports_fastmcp(module)
        && module.suite.iter().any(stmt_has_registration_decorator)
    {
        return true;
    }
    false
}

fn imports_fastmcp(module: &ParsedModule) -> bool {
    module.imports.iter().any(|i| {
        matches!(i.kind, ImportKind::Absolute)
            && (i.raw == "fastmcp" || i.raw.starts_with("fastmcp."))
    })
}

fn stmt_has_fastmcp_ctor(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Assign(a) => is_fastmcp_call(&a.value),
        Stmt::AnnAssign(a) => a.value.as_deref().map(is_fastmcp_call).unwrap_or(false),
        Stmt::Expr(e) => is_fastmcp_call(&e.value),
        Stmt::FunctionDef(f) => f.body.iter().any(stmt_has_fastmcp_ctor),
        Stmt::AsyncFunctionDef(f) => f.body.iter().any(stmt_has_fastmcp_ctor),
        Stmt::ClassDef(c) => c.body.iter().any(stmt_has_fastmcp_ctor),
        Stmt::If(s) => {
            s.body.iter().any(stmt_has_fastmcp_ctor)
                || s.orelse.iter().any(stmt_has_fastmcp_ctor)
        }
        Stmt::Try(s) => {
            s.body.iter().any(stmt_has_fastmcp_ctor)
                || s.handlers.iter().any(|h| {
                    let ast::ExceptHandler::ExceptHandler(eh) = h;
                    eh.body.iter().any(stmt_has_fastmcp_ctor)
                })
                || s.orelse.iter().any(stmt_has_fastmcp_ctor)
                || s.finalbody.iter().any(stmt_has_fastmcp_ctor)
        }
        Stmt::With(s) => s.body.iter().any(stmt_has_fastmcp_ctor),
        Stmt::AsyncWith(s) => s.body.iter().any(stmt_has_fastmcp_ctor),
        _ => false,
    }
}

fn is_fastmcp_call(expr: &Expr) -> bool {
    if !matches!(expr, Expr::Call(_)) {
        return false;
    }
    callable_tail_name(expr) == Some(CTOR_NAME)
}

fn stmt_has_registration_decorator(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::FunctionDef(f) => {
            f.decorator_list.iter().any(is_registration_decorator)
                || f.body.iter().any(stmt_has_registration_decorator)
        }
        Stmt::AsyncFunctionDef(f) => {
            f.decorator_list.iter().any(is_registration_decorator)
                || f.body.iter().any(stmt_has_registration_decorator)
        }
        Stmt::ClassDef(c) => c.body.iter().any(stmt_has_registration_decorator),
        Stmt::If(s) => {
            s.body.iter().any(stmt_has_registration_decorator)
                || s.orelse.iter().any(stmt_has_registration_decorator)
        }
        Stmt::Try(s) => {
            s.body.iter().any(stmt_has_registration_decorator)
                || s.handlers.iter().any(|h| {
                    let ast::ExceptHandler::ExceptHandler(eh) = h;
                    eh.body.iter().any(stmt_has_registration_decorator)
                })
                || s.orelse.iter().any(stmt_has_registration_decorator)
                || s.finalbody.iter().any(stmt_has_registration_decorator)
        }
        Stmt::With(s) => s.body.iter().any(stmt_has_registration_decorator),
        Stmt::AsyncWith(s) => s.body.iter().any(stmt_has_registration_decorator),
        _ => false,
    }
}

fn is_registration_decorator(expr: &Expr) -> bool {
    // FastMCP decorators are always namespaced (`@mcp.tool`), so we only
    // accept the attribute form — never a bare `tool` name.
    let target = match expr {
        Expr::Call(c) => c.func.as_ref(),
        other => other,
    };
    let Expr::Attribute(attr) = target else {
        return false;
    };
    REGISTRATION_DECORATORS.contains(&attr.attr.as_str())
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
    fn detects_fastmcp_constructor() {
        let m = parse(
            "from fastmcp import FastMCP\nmcp = FastMCP(\"server\", instructions=\"x\")\n",
        );
        assert!(module_is_fastmcp_entry(&m));
    }

    #[test]
    fn detects_constructor_inside_factory() {
        let m = parse(
            "from fastmcp import FastMCP\n\ndef build_mcp() -> FastMCP:\n    mcp = FastMCP(\"x\")\n    return mcp\n",
        );
        assert!(module_is_fastmcp_entry(&m));
    }

    #[test]
    fn detects_tool_decorator_in_separate_file() {
        let m = parse(
            "from fastmcp import Context\nfrom src.mcp_server.server import mcp\n\n@mcp.tool()\nasync def list_things(ctx: Context):\n    return []\n",
        );
        assert!(module_is_fastmcp_entry(&m));
    }

    #[test]
    fn detects_resource_and_prompt_decorators() {
        let m = parse(
            "from fastmcp.server import dependencies\nfrom src.x import mcp\n\n@mcp.resource(\"file://x\")\ndef read_file():\n    pass\n\n@mcp.prompt(\"summary\")\ndef summary_prompt():\n    pass\n",
        );
        assert!(module_is_fastmcp_entry(&m));
    }

    #[test]
    fn ignores_decorators_without_fastmcp_import() {
        let m = parse(
            "class Builder:\n    def tool(self):\n        pass\n\nbuilder = Builder()\n\n@builder.tool()\ndef something():\n    pass\n",
        );
        assert!(!module_is_fastmcp_entry(&m));
    }

    #[test]
    fn ignores_unrelated_modules() {
        let m = parse(
            "import os\n\ndef helper():\n    return os.environ.get(\"X\")\n",
        );
        assert!(!module_is_fastmcp_entry(&m));
    }

    #[test]
    fn detects_constructor_even_without_fastmcp_import_string_match() {
        let m = parse(
            "import fastmcp\nmcp = fastmcp.FastMCP(\"x\")\n",
        );
        assert!(module_is_fastmcp_entry(&m));
    }
}
