use crate::smells::imports::ImportBindings;
use pyllow_extract::ast::{self, ExceptHandler, Expr, Stmt};
use pyllow_extract::line_at_offset;
use pyllow_extract::walker::walk_stmts;
use pyllow_types::{Issue, SmellRule};
use std::path::Path;

/// Qualified names whose `raise X(...) from None` is FastAPI's documented
/// exception-translation idiom (FastAPI re-exports Starlette's class).
const HTTP_EXCEPTION_PATHS: &[&str] = &[
    "fastapi.HTTPException",
    "fastapi.exceptions.HTTPException",
    "starlette.exceptions.HTTPException",
];

pub(in crate::smells) fn check(
    stmts: &[Stmt],
    source: &str,
    path: &Path,
    exempt_http_translation: bool,
    out: &mut Vec<Issue>,
    exemptions: &mut Vec<String>,
) {
    let bindings = if exempt_http_translation {
        Some(ImportBindings::collect(stmts, |_, _| {}))
    } else {
        None
    };
    let mut visit = |stmt: &Stmt| {
        let Stmt::Try(t) = stmt else { return };
        for handler in &t.handlers {
            let ExceptHandler::ExceptHandler(h) = handler;
            for inner in &h.body {
                let Stmt::Raise(r) = inner else { continue };
                let Some(cause) = &r.cause else { continue };
                let Expr::Constant(c) = cause.as_ref() else {
                    continue;
                };
                if !matches!(c.value, ast::Constant::None) {
                    continue;
                }
                let line = line_at_offset(source, r.range.start().to_usize());
                if let Some(bindings) = &bindings {
                    if raised_import_origin(r.exc.as_deref(), bindings)
                        .is_some_and(|origin| HTTP_EXCEPTION_PATHS.contains(&origin.as_str()))
                    {
                        // Narrow, explainable exemption: only an
                        // import-resolved HTTPException, only inside an
                        // except handler, and it leaves a trace.
                        exemptions.push(format!(
                            "fastapi: raise-from-none exempted at {}:{line} (HTTPException translation idiom)",
                            path.display()
                        ));
                        continue;
                    }
                }
                out.push(Issue::Smell {
                    path: path.to_path_buf(),
                    line,
                    rule: SmellRule::RaiseFromNone,
                    detail: "`raise ... from None` discards the original exception cause"
                        .to_string(),
                });
            }
        }
    };
    walk_stmts(stmts, &mut visit);
}

/// Qualified origin of the raised exception's callee (`raise X(...)` or
/// `raise X`), resolved through the module's imports.
fn raised_import_origin(exc: Option<&Expr>, bindings: &ImportBindings) -> Option<String> {
    let target = match exc? {
        Expr::Call(call) => call.func.as_ref(),
        other => other,
    };
    bindings.resolve(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_extract::parse_source;
    use std::path::PathBuf;

    fn run(src: &str, exempt: bool) -> (Vec<u32>, Vec<String>) {
        let path = PathBuf::from("/tmp/api.py");
        let module = parse_source(&path, src).unwrap();
        let mut out = Vec::new();
        let mut exemptions = Vec::new();
        check(&module.suite, src, &path, exempt, &mut out, &mut exemptions);
        let lines = out
            .iter()
            .filter_map(|i| match i {
                Issue::Smell { line, .. } => Some(*line),
                _ => None,
            })
            .collect();
        (lines, exemptions)
    }

    const TRANSLATE: &str = "from fastapi import HTTPException\n\ndef get(uid):\n    try:\n        return db[uid]\n    except KeyError:\n        raise HTTPException(status_code=404) from None\n";

    // #9: `raise HTTPException(...) from None` is FastAPI's documented
    // exception-translation idiom — hiding the KeyError from the client is
    // the point.
    #[test]
    fn http_exception_translation_is_exempt_when_policy_is_on() {
        let (lines, exemptions) = run(TRANSLATE, true);
        assert!(lines.is_empty(), "translation idiom must not be flagged");
        assert_eq!(exemptions.len(), 1);
        assert!(
            exemptions[0].contains("HTTPException") && exemptions[0].contains("7"),
            "exemption must be explainable (what + where): {}",
            exemptions[0]
        );
    }

    #[test]
    fn disabling_the_policy_restores_framework_agnostic_behavior() {
        let (lines, exemptions) = run(TRANSLATE, false);
        assert_eq!(lines, vec![7]);
        assert!(exemptions.is_empty());
    }

    #[test]
    fn aliased_and_qualified_forms_are_exempt() {
        let aliased = "from fastapi import HTTPException as HE\n\ndef f():\n    try:\n        pass\n    except KeyError:\n        raise HE(404) from None\n";
        let qualified = "import fastapi\n\ndef f():\n    try:\n        pass\n    except KeyError:\n        raise fastapi.HTTPException(404) from None\n";
        let starlette = "from starlette.exceptions import HTTPException\n\ndef f():\n    try:\n        pass\n    except KeyError:\n        raise HTTPException(404) from None\n";
        for src in [aliased, qualified, starlette] {
            let (lines, exemptions) = run(src, true);
            assert!(lines.is_empty(), "{src}");
            assert_eq!(exemptions.len(), 1, "{src}");
        }
    }

    // The exemption is narrow: everything that is not an import-resolved
    // HTTPException keeps the framework-agnostic behavior.
    #[test]
    fn unrelated_raise_from_none_is_still_flagged_in_the_same_file() {
        let src = "from fastapi import HTTPException\n\ndef f():\n    try:\n        pass\n    except KeyError:\n        raise ValueError(\"x\") from None\n";
        let (lines, exemptions) = run(src, true);
        assert_eq!(lines, vec![7]);
        assert!(exemptions.is_empty());
    }

    #[test]
    fn local_class_named_http_exception_is_not_exempt() {
        let src = "class HTTPException(Exception): ...\n\ndef f():\n    try:\n        pass\n    except KeyError:\n        raise HTTPException() from None\n";
        let (lines, _) = run(src, true);
        assert_eq!(lines, vec![7], "a local class is not the framework idiom");
    }
}
