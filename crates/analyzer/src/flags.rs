//! Feature-flag inventory.
//!
//! Detects flag references across four common Python patterns:
//! - `os.environ.get("FEATURE_X")` env-var conventions
//! - Django `settings.FEATURES["x"]`
//! - SDK calls: LaunchDarkly, Statsig, Unleash, GrowthBook
//!
//! Each detected reference becomes an `Issue::FeatureFlag` carrying the flag
//! name and the provider that surfaced it. No deduction or scoring; this is
//! an inventory pass.

use pyllow_extract::ast::{self, Constant, Expr, Stmt};
use pyllow_extract::{line_at_offset, ParsedModule};
use pyllow_types::{FileId, FlagProvider, Issue};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::path::Path;

pub fn analyze(parsed: &FxHashMap<FileId, ParsedModule>) -> Vec<Issue> {
    parsed
        .par_iter()
        .flat_map(|(_, module)| analyze_module(module))
        .collect()
}

fn analyze_module(module: &ParsedModule) -> Vec<Issue> {
    let mut out = Vec::new();
    for stmt in &module.suite {
        scan_stmt(stmt, &module.path, module.source.as_str(), &mut out);
    }
    out
}

fn scan_stmt(stmt: &Stmt, path: &Path, source: &str, out: &mut Vec<Issue>) {
    use Stmt::*;
    match stmt {
        Expr(e) => scan_expr(&e.value, path, source, out),
        Assign(a) => {
            for t in &a.targets {
                scan_expr(t, path, source, out);
            }
            scan_expr(&a.value, path, source, out);
        }
        AnnAssign(a) => {
            scan_expr(&a.target, path, source, out);
            if let Some(v) = &a.value {
                scan_expr(v, path, source, out);
            }
        }
        AugAssign(a) => scan_expr(&a.value, path, source, out),
        Return(r) => {
            if let Some(v) = &r.value {
                scan_expr(v, path, source, out);
            }
        }
        If(s) => {
            scan_expr(&s.test, path, source, out);
            for inner in s.body.iter().chain(s.orelse.iter()) {
                scan_stmt(inner, path, source, out);
            }
        }
        While(s) => {
            scan_expr(&s.test, path, source, out);
            for inner in s.body.iter().chain(s.orelse.iter()) {
                scan_stmt(inner, path, source, out);
            }
        }
        For(s) => {
            scan_expr(&s.iter, path, source, out);
            for inner in s.body.iter().chain(s.orelse.iter()) {
                scan_stmt(inner, path, source, out);
            }
        }
        AsyncFor(s) => {
            scan_expr(&s.iter, path, source, out);
            for inner in s.body.iter().chain(s.orelse.iter()) {
                scan_stmt(inner, path, source, out);
            }
        }
        FunctionDef(f) => {
            for inner in &f.body {
                scan_stmt(inner, path, source, out);
            }
        }
        AsyncFunctionDef(f) => {
            for inner in &f.body {
                scan_stmt(inner, path, source, out);
            }
        }
        ClassDef(c) => {
            for inner in &c.body {
                scan_stmt(inner, path, source, out);
            }
        }
        Try(s) => {
            for inner in &s.body {
                scan_stmt(inner, path, source, out);
            }
            for ast::ExceptHandler::ExceptHandler(h) in &s.handlers {
                for inner in &h.body {
                    scan_stmt(inner, path, source, out);
                }
            }
            for inner in s.orelse.iter().chain(s.finalbody.iter()) {
                scan_stmt(inner, path, source, out);
            }
        }
        With(s) => {
            for inner in &s.body {
                scan_stmt(inner, path, source, out);
            }
        }
        AsyncWith(s) => {
            for inner in &s.body {
                scan_stmt(inner, path, source, out);
            }
        }
        Raise(r) => {
            if let Some(e) = &r.exc {
                scan_expr(e, path, source, out);
            }
        }
        _ => {}
    }
}

fn scan_expr(expr: &Expr, path: &Path, source: &str, out: &mut Vec<Issue>) {
    let detected = detect_flag_call(expr).or_else(|| detect_django_settings(expr));
    if let Some((flag, provider)) = detected {
        let line = line_at_offset(source, expr_range_start(expr));
        out.push(Issue::FeatureFlag {
            path: path.to_path_buf(),
            line,
            flag,
            provider,
        });
    }
    // Recurse into compound expressions so flags nested in conditions or
    // arguments are still surfaced.
    use Expr::*;
    match expr {
        BoolOp(b) => {
            for v in &b.values {
                scan_expr(v, path, source, out);
            }
        }
        BinOp(b) => {
            scan_expr(&b.left, path, source, out);
            scan_expr(&b.right, path, source, out);
        }
        UnaryOp(u) => scan_expr(&u.operand, path, source, out),
        IfExp(i) => {
            scan_expr(&i.test, path, source, out);
            scan_expr(&i.body, path, source, out);
            scan_expr(&i.orelse, path, source, out);
        }
        Compare(c) => {
            scan_expr(&c.left, path, source, out);
            for r in &c.comparators {
                scan_expr(r, path, source, out);
            }
        }
        Call(c) => {
            for a in &c.args {
                scan_expr(a, path, source, out);
            }
            for kw in &c.keywords {
                scan_expr(&kw.value, path, source, out);
            }
        }
        Subscript(s) => {
            scan_expr(&s.value, path, source, out);
            scan_expr(&s.slice, path, source, out);
        }
        _ => {}
    }
}

/// Detect SDK-style flag calls that take the flag name as the first string arg.
fn detect_flag_call(expr: &Expr) -> Option<(String, FlagProvider)> {
    let Expr::Call(call) = expr else { return None };
    let Expr::Attribute(attr) = call.func.as_ref() else { return None };
    let method = attr.attr.as_str();
    let provider = match method {
        "get" => match attr.value.as_ref() {
            // `os.environ.get(...)` — only flag-shaped names ("FEATURE_*", "FF_*", "ENABLE_*").
            Expr::Attribute(parent) if parent.attr.as_str() == "environ" => {
                let key = first_string_arg(&call.args)?;
                if !looks_like_feature_env_var(&key) {
                    return None;
                }
                return Some((key, FlagProvider::EnvVar));
            }
            _ => return None,
        },
        // LaunchDarkly: client.variation("flag-key", user, default)
        "variation" | "variation_detail" | "bool_variation" => FlagProvider::LaunchDarkly,
        // Statsig: Statsig.check_gate("gate") / get_config("config")
        "check_gate" | "get_config" | "get_experiment" => FlagProvider::Statsig,
        // Unleash: client.is_enabled("flag")
        "is_enabled" => FlagProvider::Unleash,
        // GrowthBook: gb.is_on("flag") / gb.feature_value("flag", default)
        "is_on" | "feature_value" => FlagProvider::GrowthBook,
        _ => return None,
    };
    let key = first_string_arg(&call.args)?;
    Some((key, provider))
}

/// Detect Django `settings.FEATURES["X"]` subscript reads.
fn detect_django_settings(expr: &Expr) -> Option<(String, FlagProvider)> {
    let Expr::Subscript(s) = expr else { return None };
    let Expr::Attribute(attr) = s.value.as_ref() else { return None };
    if attr.attr.as_str() != "FEATURES" {
        return None;
    }
    // The base may be either `settings.FEATURES[...]` (Name) or
    // `django.conf.settings.FEATURES[...]` (Attribute chain).
    let base_name = match attr.value.as_ref() {
        Expr::Name(n) => n.id.as_str(),
        Expr::Attribute(parent) => parent.attr.as_str(),
        _ => return None,
    };
    if base_name != "settings" {
        return None;
    }
    let key = string_constant(&s.slice)?;
    Some((key, FlagProvider::DjangoSettings))
}

fn first_string_arg(args: &[Expr]) -> Option<String> {
    args.first().and_then(string_constant)
}

fn string_constant(expr: &Expr) -> Option<String> {
    let Expr::Constant(c) = expr else { return None };
    let Constant::Str(s) = &c.value else { return None };
    Some(s.clone())
}

fn looks_like_feature_env_var(name: &str) -> bool {
    name.starts_with("FEATURE_")
        || name.starts_with("FF_")
        || name.starts_with("ENABLE_")
        || name.starts_with("DISABLE_")
}

fn expr_range_start(expr: &Expr) -> usize {
    use Expr::*;
    match expr {
        Call(e) => e.range.start().to_usize(),
        Attribute(e) => e.range.start().to_usize(),
        Subscript(e) => e.range.start().to_usize(),
        Name(e) => e.range.start().to_usize(),
        Constant(e) => e.range.start().to_usize(),
        BoolOp(e) => e.range.start().to_usize(),
        BinOp(e) => e.range.start().to_usize(),
        UnaryOp(e) => e.range.start().to_usize(),
        IfExp(e) => e.range.start().to_usize(),
        Compare(e) => e.range.start().to_usize(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_extract::parse_source;
    use std::path::PathBuf;

    fn parsed(src: &str) -> FxHashMap<FileId, ParsedModule> {
        let mut m = parse_source(Path::new("test.py"), src).unwrap();
        m.path = PathBuf::from("test.py");
        let mut map = FxHashMap::default();
        map.insert(FileId(0), m);
        map
    }

    fn flags(issues: &[Issue]) -> Vec<(&str, FlagProvider)> {
        issues
            .iter()
            .filter_map(|i| match i {
                Issue::FeatureFlag { flag, provider, .. } => Some((flag.as_str(), *provider)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn detects_os_environ_get_feature_prefix() {
        let issues = analyze(&parsed(
            "import os\nif os.environ.get(\"FEATURE_NEW_BILLING\"):\n    pass\n",
        ));
        assert_eq!(
            flags(&issues),
            vec![("FEATURE_NEW_BILLING", FlagProvider::EnvVar)]
        );
    }

    #[test]
    fn ignores_unrelated_environ_get() {
        let issues = analyze(&parsed(
            "import os\nx = os.environ.get(\"DATABASE_URL\")\n",
        ));
        assert!(flags(&issues).is_empty());
    }

    #[test]
    fn detects_django_settings_features_subscript() {
        let issues = analyze(&parsed(
            "from django.conf import settings\nif settings.FEATURES[\"new_checkout\"]:\n    pass\n",
        ));
        assert_eq!(
            flags(&issues),
            vec![("new_checkout", FlagProvider::DjangoSettings)]
        );
    }

    #[test]
    fn detects_launchdarkly_variation() {
        let issues = analyze(&parsed(
            "result = client.variation(\"new-onboarding\", user, False)\n",
        ));
        assert_eq!(
            flags(&issues),
            vec![("new-onboarding", FlagProvider::LaunchDarkly)]
        );
    }

    #[test]
    fn detects_statsig_check_gate() {
        let issues = analyze(&parsed(
            "if Statsig.check_gate(\"experimental_ui\", user):\n    pass\n",
        ));
        assert_eq!(
            flags(&issues),
            vec![("experimental_ui", FlagProvider::Statsig)]
        );
    }

    #[test]
    fn detects_unleash_is_enabled() {
        let issues = analyze(&parsed(
            "if unleash.is_enabled(\"checkout-v2\"):\n    pass\n",
        ));
        assert_eq!(
            flags(&issues),
            vec![("checkout-v2", FlagProvider::Unleash)]
        );
    }

    #[test]
    fn detects_growthbook_is_on() {
        let issues = analyze(&parsed("if gb.is_on(\"dark-mode\"):\n    pass\n"));
        assert_eq!(flags(&issues), vec![("dark-mode", FlagProvider::GrowthBook)]);
    }

    #[test]
    fn detects_flag_inside_function_body() {
        let issues = analyze(&parsed(
            "def handler():\n    return Statsig.check_gate(\"nested_flag\")\n",
        ));
        assert_eq!(
            flags(&issues),
            vec![("nested_flag", FlagProvider::Statsig)]
        );
    }

    #[test]
    fn ignores_call_without_string_arg() {
        let issues = analyze(&parsed(
            "k = config_key\nif client.variation(k, user):\n    pass\n",
        ));
        assert!(flags(&issues).is_empty());
    }
}
