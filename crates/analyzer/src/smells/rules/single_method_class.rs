use crate::walker::walk_stmts;
use pyllow_extract::ast::Stmt;
use pyllow_extract::line_at_offset;
use pyllow_types::{Issue, SmellRule};
use std::path::Path;

pub(in crate::smells) fn check(stmts: &[Stmt], source: &str, path: &Path, out: &mut Vec<Issue>) {
    let mut visit = |stmt: &Stmt| {
        let Stmt::ClassDef(c) = stmt else { return };
        // Skip classes with non-trivial bases (likely framework subclasses).
        if !c.bases.is_empty() || !c.keywords.is_empty() || !c.decorator_list.is_empty() {
            return;
        }
        let mut method_count = 0u32;
        let mut has_state = false;
        let mut sole_method: Option<String> = None;
        let mut bail = false;
        for inner in &c.body {
            let fname = match inner {
                Stmt::FunctionDef(f) => Some(f.name.as_str()),
                Stmt::AsyncFunctionDef(f) => Some(f.name.as_str()),
                Stmt::Assign(_) | Stmt::AnnAssign(_) => {
                    has_state = true;
                    None
                }
                _ => None,
            };
            let Some(fname) = fname else { continue };
            if matches!(fname, "__init__" | "__new__") {
                has_state = true;
                continue;
            }
            if fname.starts_with("__") && fname.ends_with("__") {
                bail = true;
                break;
            }
            method_count += 1;
            sole_method = Some(fname.to_string());
        }
        if bail || has_state || method_count != 1 {
            return;
        }
        let line = line_at_offset(source, c.range.start().to_usize());
        let method = sole_method.as_deref().unwrap_or("<method>");
        out.push(Issue::Smell {
            path: path.to_path_buf(),
            line,
            rule: SmellRule::SingleMethodClass,
            detail: format!(
                "class `{}` has only `{}` and no state; consider a free function",
                c.name.as_str(),
                method
            ),
        });
    };
    walk_stmts(stmts, &mut visit);
}
