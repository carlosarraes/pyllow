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

use pyllow_extract::ast::{Constant, Expr, Ranged};
use pyllow_extract::walker::walk_stmts_for_exprs;
use pyllow_extract::{line_at_offset, ParsedModule};
use pyllow_types::{FileId, FlagProvider, Issue};
use rayon::prelude::*;
use rustc_hash::FxHashMap;

/// Top-level imports that signal a flag-related code path. Modules without
/// any of these are skipped entirely — the most common case by far.
const FLAG_IMPORT_PREFIXES: &[&str] = &[
    "os",
    "django",
    "django.conf",
    "ldclient",
    "launchdarkly",
    "statsig",
    "UnleashClient",
    "unleash",
    "growthbook",
];

pub fn analyze(parsed: &FxHashMap<FileId, ParsedModule>) -> Vec<Issue> {
    parsed
        .par_iter()
        .flat_map(|(_, module)| analyze_module(module))
        .collect()
}

fn analyze_module(module: &ParsedModule) -> Vec<Issue> {
    if !has_flag_relevant_import(module) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let path = &module.path;
    let source = module.source.as_str();
    let mut visit = |expr: &Expr| {
        let detected = detect_flag_call(expr).or_else(|| detect_django_settings(expr));
        if let Some((flag, provider)) = detected {
            let line = line_at_offset(source, expr.range().start().to_usize());
            out.push(Issue::FeatureFlag {
                path: path.to_path_buf(),
                line,
                flag,
                provider,
            });
        }
    };
    walk_stmts_for_exprs(&module.suite, &mut visit);
    out
}

fn has_flag_relevant_import(module: &ParsedModule) -> bool {
    module.imports.iter().any(|i| {
        let raw = i.raw.as_str();
        FLAG_IMPORT_PREFIXES
            .iter()
            .any(|p| raw == *p || raw.starts_with(&format!("{p}.")))
    })
}

/// Detect SDK-style flag calls that take the flag name as the first string arg.
fn detect_flag_call(expr: &Expr) -> Option<(String, FlagProvider)> {
    let Expr::Call(call) = expr else { return None };
    let Expr::Attribute(attr) = call.func.as_ref() else {
        return None;
    };
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
    let Expr::Subscript(s) = expr else {
        return None;
    };
    let Expr::Attribute(attr) = s.value.as_ref() else {
        return None;
    };
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
    let Constant::Str(s) = &c.value else {
        return None;
    };
    Some(s.clone())
}

fn looks_like_feature_env_var(name: &str) -> bool {
    name.starts_with("FEATURE_")
        || name.starts_with("FF_")
        || name.starts_with("ENABLE_")
        || name.starts_with("DISABLE_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_extract::parse_source;
    use std::path::{Path, PathBuf};

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
        let issues = analyze(&parsed("import os\nx = os.environ.get(\"DATABASE_URL\")\n"));
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
            "import ldclient\nresult = client.variation(\"new-onboarding\", user, False)\n",
        ));
        assert_eq!(
            flags(&issues),
            vec![("new-onboarding", FlagProvider::LaunchDarkly)]
        );
    }

    #[test]
    fn detects_statsig_check_gate() {
        let issues = analyze(&parsed(
            "import statsig\nif Statsig.check_gate(\"experimental_ui\", user):\n    pass\n",
        ));
        assert_eq!(
            flags(&issues),
            vec![("experimental_ui", FlagProvider::Statsig)]
        );
    }

    #[test]
    fn detects_unleash_is_enabled() {
        let issues = analyze(&parsed(
            "from UnleashClient import UnleashClient\nif unleash.is_enabled(\"checkout-v2\"):\n    pass\n",
        ));
        assert_eq!(flags(&issues), vec![("checkout-v2", FlagProvider::Unleash)]);
    }

    #[test]
    fn detects_growthbook_is_on() {
        let issues = analyze(&parsed(
            "import growthbook\nif gb.is_on(\"dark-mode\"):\n    pass\n",
        ));
        assert_eq!(
            flags(&issues),
            vec![("dark-mode", FlagProvider::GrowthBook)]
        );
    }

    #[test]
    fn detects_flag_inside_function_body() {
        let issues = analyze(&parsed(
            "import statsig\ndef handler():\n    return Statsig.check_gate(\"nested_flag\")\n",
        ));
        assert_eq!(flags(&issues), vec![("nested_flag", FlagProvider::Statsig)]);
    }

    #[test]
    fn ignores_call_without_string_arg() {
        let issues = analyze(&parsed(
            "import ldclient\nk = config_key\nif client.variation(k, user):\n    pass\n",
        ));
        assert!(flags(&issues).is_empty());
    }

    #[test]
    fn skips_modules_without_flag_relevant_imports() {
        // No flag SDK imported — even an SDK-shaped call is ignored, since
        // the heuristic skips the entire module.
        let issues = analyze(&parsed(
            "x = client.variation(\"new-onboarding\", user, False)\n",
        ));
        assert!(flags(&issues).is_empty());
    }

    #[test]
    fn detects_flag_inside_list_comprehension() {
        // The shared walker covers comprehensions, which the previous
        // hand-rolled scanner missed.
        let issues = analyze(&parsed(
            "import statsig\nactive = [g for g in gates if Statsig.check_gate(g, user)]\n",
        ));
        // The first arg here is a name (not a string) so this should be ignored
        // — but the walker MUST visit the call, otherwise we'd miss flag-shaped
        // calls deeper. Use a literal-string variant to confirm the walker fires:
        let issues2 = analyze(&parsed(
            "import statsig\nresult = [Statsig.check_gate(\"in-comp\") for _ in range(3)]\n",
        ));
        assert!(flags(&issues).is_empty());
        assert_eq!(flags(&issues2), vec![("in-comp", FlagProvider::Statsig)]);
    }
}
