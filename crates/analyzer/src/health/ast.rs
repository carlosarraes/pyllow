//! Per-function complexity collection from the Python AST.

use pyllow_extract::ast::{self, Stmt};
use pyllow_extract::{line_at_offset, ParsedModule};

#[derive(Debug, Clone)]
pub(super) struct FunctionHealth {
    pub name: String,
    pub line: u32,
    pub cyclomatic: u32,
    pub cognitive: u32,
}

#[derive(Debug, Clone)]
pub(super) struct FileHealth {
    pub path: std::path::PathBuf,
    pub functions: Vec<FunctionHealth>,
    pub total_cyclomatic: u32,
    pub loc: u32,
    pub maintainability: Option<u32>,
}

impl FileHealth {
    pub(super) fn avg_cyclomatic(&self) -> f32 {
        if self.functions.is_empty() {
            1.0
        } else {
            self.total_cyclomatic as f32 / self.functions.len() as f32
        }
    }
}

pub(super) fn compute_file_health(module: &ParsedModule) -> FileHealth {
    let source = module.source.as_str();
    let loc = super::metrics::count_loc(source);

    let mut functions = Vec::new();
    for stmt in &module.suite {
        collect_functions(stmt, source, &mut functions);
    }

    let total_cyclomatic: u32 = functions.iter().map(|f| f.cyclomatic).sum();
    let avg_cyclomatic = if functions.is_empty() {
        1.0
    } else {
        total_cyclomatic as f32 / functions.len() as f32
    };

    let maintainability = if loc == 0 {
        None
    } else {
        Some(super::metrics::maintainability_index(
            source,
            avg_cyclomatic,
            loc,
        ))
    };

    FileHealth {
        path: module.path.clone(),
        functions,
        total_cyclomatic,
        loc,
        maintainability,
    }
}

pub(super) fn collect_functions(stmt: &Stmt, source: &str, out: &mut Vec<FunctionHealth>) {
    match stmt {
        Stmt::FunctionDef(f) => {
            let line = line_at_offset(source, f.range.start().to_usize());
            let mut cc = 1u32;
            let mut cog = 0u32;
            for inner in &f.body {
                accumulate_complexity(inner, 0, &mut cc, &mut cog);
            }
            out.push(FunctionHealth {
                name: f.name.as_str().to_string(),
                line,
                cyclomatic: cc,
                cognitive: cog,
            });
            for inner in &f.body {
                collect_functions(inner, source, out);
            }
        }
        Stmt::AsyncFunctionDef(f) => {
            let line = line_at_offset(source, f.range.start().to_usize());
            let mut cc = 1u32;
            let mut cog = 0u32;
            for inner in &f.body {
                accumulate_complexity(inner, 0, &mut cc, &mut cog);
            }
            out.push(FunctionHealth {
                name: f.name.as_str().to_string(),
                line,
                cyclomatic: cc,
                cognitive: cog,
            });
            for inner in &f.body {
                collect_functions(inner, source, out);
            }
        }
        Stmt::ClassDef(c) => {
            for inner in &c.body {
                collect_functions(inner, source, out);
            }
        }
        Stmt::If(s) => {
            for inner in &s.body {
                collect_functions(inner, source, out);
            }
            for inner in &s.orelse {
                collect_functions(inner, source, out);
            }
        }
        Stmt::While(s) => {
            for inner in &s.body {
                collect_functions(inner, source, out);
            }
        }
        Stmt::For(s) => {
            for inner in &s.body {
                collect_functions(inner, source, out);
            }
        }
        Stmt::AsyncFor(s) => {
            for inner in &s.body {
                collect_functions(inner, source, out);
            }
        }
        Stmt::Try(s) => {
            for inner in &s.body {
                collect_functions(inner, source, out);
            }
            for h in &s.handlers {
                let ast::ExceptHandler::ExceptHandler(eh) = h;
                for inner in &eh.body {
                    collect_functions(inner, source, out);
                }
            }
            for inner in &s.finalbody {
                collect_functions(inner, source, out);
            }
        }
        Stmt::With(s) => {
            for inner in &s.body {
                collect_functions(inner, source, out);
            }
        }
        Stmt::AsyncWith(s) => {
            for inner in &s.body {
                collect_functions(inner, source, out);
            }
        }
        _ => {}
    }
}

fn accumulate_complexity(stmt: &Stmt, depth: u32, cc: &mut u32, cog: &mut u32) {
    match stmt {
        Stmt::If(s) => {
            *cc += 1;
            *cog += 1 + depth;
            *cc += count_bool_ops(s.test.as_ref());
            for inner in &s.body {
                accumulate_complexity(inner, depth + 1, cc, cog);
            }
            for inner in &s.orelse {
                accumulate_complexity(inner, depth, cc, cog);
            }
        }
        Stmt::While(s) => {
            *cc += 1;
            *cog += 1 + depth;
            *cc += count_bool_ops(s.test.as_ref());
            for inner in &s.body {
                accumulate_complexity(inner, depth + 1, cc, cog);
            }
            for inner in &s.orelse {
                accumulate_complexity(inner, depth, cc, cog);
            }
        }
        Stmt::For(s) => {
            *cc += 1;
            *cog += 1 + depth;
            for inner in &s.body {
                accumulate_complexity(inner, depth + 1, cc, cog);
            }
        }
        Stmt::AsyncFor(s) => {
            *cc += 1;
            *cog += 1 + depth;
            for inner in &s.body {
                accumulate_complexity(inner, depth + 1, cc, cog);
            }
        }
        Stmt::Try(s) => {
            for inner in &s.body {
                accumulate_complexity(inner, depth + 1, cc, cog);
            }
            for h in &s.handlers {
                let ast::ExceptHandler::ExceptHandler(eh) = h;
                *cc += 1;
                *cog += 1 + depth;
                for inner in &eh.body {
                    accumulate_complexity(inner, depth + 1, cc, cog);
                }
            }
            for inner in &s.orelse {
                accumulate_complexity(inner, depth, cc, cog);
            }
            for inner in &s.finalbody {
                accumulate_complexity(inner, depth, cc, cog);
            }
        }
        Stmt::With(s) => {
            for inner in &s.body {
                accumulate_complexity(inner, depth + 1, cc, cog);
            }
        }
        Stmt::AsyncWith(s) => {
            for inner in &s.body {
                accumulate_complexity(inner, depth + 1, cc, cog);
            }
        }
        Stmt::Match(s) => {
            for case in &s.cases {
                if !is_wildcard_pattern(&case.pattern) {
                    *cc += 1;
                    *cog += 1 + depth;
                }
                for inner in &case.body {
                    accumulate_complexity(inner, depth + 1, cc, cog);
                }
            }
        }
        _ => {}
    }
}

fn count_bool_ops(expr: &ast::Expr) -> u32 {
    match expr {
        ast::Expr::BoolOp(b) => {
            let mut count = if b.values.len() > 1 {
                (b.values.len() - 1) as u32
            } else {
                0
            };
            for v in &b.values {
                count += count_bool_ops(v);
            }
            count
        }
        _ => 0,
    }
}

fn is_wildcard_pattern(p: &ast::Pattern) -> bool {
    matches!(p, ast::Pattern::MatchAs(a) if a.name.is_none() && a.pattern.is_none())
}
