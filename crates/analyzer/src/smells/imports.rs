//! Import-binding resolver shared by policy rules (`banned-api`,
//! `no-explicit-any`).
//!
//! Maps every local name bound by `import` / `from … import` back to its
//! qualified origin, then flattens `Name` / `Attribute` chains through that
//! map. Purely syntactic: names that are not import-bound resolve to `None`,
//! and relative imports are left unresolved rather than guessed.

use pyllow_extract::ast::{Expr, Stmt};
use pyllow_extract::walker::walk_stmts;
use rustc_hash::FxHashMap;

#[derive(Debug, Default)]
pub(super) struct ImportBindings {
    map: FxHashMap<String, String>,
}

impl ImportBindings {
    /// Collect bindings from every import in the module, invoking
    /// `on_import(qualified_name, statement)` for each imported name so
    /// callers can inspect the import statements themselves.
    pub(super) fn collect(stmts: &[Stmt], mut on_import: impl FnMut(&str, &Stmt)) -> Self {
        let mut map: FxHashMap<String, String> = FxHashMap::default();
        let mut on_stmt = |stmt: &Stmt| match stmt {
            Stmt::Import(s) => {
                for alias in &s.names {
                    let full = alias.name.as_str();
                    match &alias.asname {
                        Some(local) => {
                            map.insert(local.as_str().to_string(), full.to_string());
                        }
                        None => {
                            // `import a.b.c` binds only `a`.
                            let root = full.split('.').next().unwrap_or(full);
                            map.insert(root.to_string(), root.to_string());
                        }
                    }
                    on_import(full, stmt);
                }
            }
            Stmt::ImportFrom(s) => {
                if s.level.map(|l| l.to_u32()).unwrap_or(0) > 0 {
                    return;
                }
                let Some(module) = s.module.as_ref().map(|m| m.as_str()) else {
                    return;
                };
                for alias in &s.names {
                    let name = alias.name.as_str();
                    if name == "*" {
                        continue;
                    }
                    let full = format!("{module}.{name}");
                    let local = alias.asname.as_ref().map_or(name, |a| a.as_str());
                    map.insert(local.to_string(), full.clone());
                    on_import(&full, stmt);
                }
            }
            _ => {}
        };
        walk_stmts(stmts, &mut on_stmt);
        Self { map }
    }

    /// Flatten `a.b.c` to a dotted string with the root resolved through the
    /// bindings. `None` when the root is not import-bound.
    pub(super) fn resolve(&self, expr: &Expr) -> Option<String> {
        let mut segments: Vec<&str> = Vec::new();
        let mut cur = expr;
        loop {
            match cur {
                Expr::Attribute(a) => {
                    segments.push(a.attr.as_str());
                    cur = a.value.as_ref();
                }
                Expr::Name(n) => {
                    segments.push(self.map.get(n.id.as_str())?);
                    break;
                }
                _ => return None,
            }
        }
        segments.reverse();
        Some(segments.join("."))
    }
}
