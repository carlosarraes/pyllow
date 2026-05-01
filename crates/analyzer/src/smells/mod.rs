//! Detect Python anti-patterns commonly produced by AI-generated code.
//!
//! Each rule lives in its own file under `rules/` so individual heuristics
//! can be reviewed, tuned, and tested in isolation. The shared AST traversal
//! helpers live in `walker.rs`.

mod rules;

use pyllow_extract::ParsedModule;
use pyllow_types::{FileId, Issue, SmellRule};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct SmellsOptions {
    pub disabled: FxHashSet<SmellRule>,
    pub todo_density_threshold: u32,
    /// Extra terminal name segments that mark a field as money-shaped, in
    /// addition to [`rules::money_as_float::DEFAULT_MONEY_WORDS`]. Sourced
    /// from `[smells.money_as_float].extra_name_patterns` in `pyllow.toml`.
    pub money_extra_words: Vec<String>,
}

impl Default for SmellsOptions {
    fn default() -> Self {
        Self {
            disabled: FxHashSet::default(),
            todo_density_threshold: 5,
            money_extra_words: Vec::new(),
        }
    }
}

pub fn analyze(parsed: &FxHashMap<FileId, ParsedModule>, opts: &SmellsOptions) -> Vec<Issue> {
    // Pytest entry files get an exemption from `single-method-class`,
    // `truthy-length-check`, `passthrough-function`, and `stray-print` —
    // those are conventional in tests and would generate noise.
    let pytest_entries = pyllow_plugin_pytest::discover(parsed).entry_files;
    parsed
        .iter()
        .par_bridge()
        .flat_map(|(id, m)| analyze_module(m, opts, pytest_entries.contains(id)))
        .collect()
}

fn analyze_module(
    module: &ParsedModule,
    opts: &SmellsOptions,
    is_pytest_entry: bool,
) -> Vec<Issue> {
    let source = module.source.as_str();
    let path = &module.path;
    let suite = &module.suite;
    let mut issues = Vec::new();
    let enabled = |r: SmellRule| !opts.disabled.contains(&r);

    if enabled(SmellRule::MutableDefault) {
        rules::mutable_default::check(suite, source, path, &mut issues);
    }
    if enabled(SmellRule::BroadExcept) {
        rules::broad_except::check(suite, source, path, &mut issues);
    }
    if enabled(SmellRule::SentinelEquality) {
        rules::sentinel_equality::check(suite, source, path, &mut issues);
    }
    if enabled(SmellRule::TruthyLengthCheck) && !is_pytest_entry {
        rules::truthy_length::check(suite, source, path, &mut issues);
    }
    if enabled(SmellRule::UnreachableAfterExit) {
        rules::unreachable::check(suite, source, path, &mut issues);
    }
    if enabled(SmellRule::PassthroughFunction) && !is_pytest_entry {
        rules::passthrough::check(suite, source, path, &mut issues);
    }
    if enabled(SmellRule::StrayPrint) && !module.is_script_entry && !is_pytest_entry {
        rules::stray_print::check(suite, source, path, &mut issues);
    }
    if enabled(SmellRule::SingleMethodClass) && !is_pytest_entry {
        rules::single_method_class::check(suite, source, path, &mut issues);
    }
    if enabled(SmellRule::HighTodoDensity) {
        rules::todo_density::check(source, path, opts.todo_density_threshold, &mut issues);
    }
    if enabled(SmellRule::RaiseFromNone) {
        rules::raise_from_none::check(suite, source, path, &mut issues);
    }
    if enabled(SmellRule::MoneyAsFloat) {
        let words = effective_money_words(&opts.money_extra_words);
        rules::money_as_float::check(suite, source, path, &words, &mut issues);
    }
    issues
}

/// Combines the default money-words set with any extras from config.
/// Returned as `Vec<&str>` so the rule can match against `&[&str]` without
/// per-call allocation of the static defaults.
fn effective_money_words(extras: &[String]) -> Vec<&str> {
    let mut out: Vec<&str> = rules::money_as_float::DEFAULT_MONEY_WORDS.to_vec();
    out.extend(extras.iter().map(|s| s.as_str()));
    out
}

pub fn run_with_files(files: &[PathBuf], opts: &SmellsOptions) -> Vec<Issue> {
    let (parsed, mut issues) = crate::parse_files_into_map(files);
    issues.extend(analyze(&parsed, opts));
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_extract::parse_source;
    use std::path::PathBuf;

    fn run(source: &str) -> Vec<Issue> {
        let path = PathBuf::from("/tmp/test.py");
        let module = parse_source(&path, source).expect("parse");
        analyze_module(&module, &SmellsOptions::default(), false)
    }

    fn rules(issues: &[Issue]) -> Vec<SmellRule> {
        issues
            .iter()
            .filter_map(|i| match i {
                Issue::Smell { rule, .. } => Some(*rule),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn detects_mutable_default() {
        let src = "def f(x=[]):\n    return x\n";
        assert!(rules(&run(src)).contains(&SmellRule::MutableDefault));
    }

    #[test]
    fn ignores_immutable_default() {
        let src = "def f(x=()):\n    return x\n";
        assert!(!rules(&run(src)).contains(&SmellRule::MutableDefault));
    }

    #[test]
    fn detects_bare_except() {
        let src = "try:\n    pass\nexcept:\n    pass\n";
        assert!(rules(&run(src)).contains(&SmellRule::BroadExcept));
    }

    #[test]
    fn ignores_specific_except() {
        let src = "try:\n    pass\nexcept ValueError:\n    pass\n";
        assert!(!rules(&run(src)).contains(&SmellRule::BroadExcept));
    }

    #[test]
    fn detects_sentinel_equality() {
        let src = "x = 1\nif x == None: pass\nif x != True: pass\n";
        let r = rules(&run(src));
        assert!(
            r.iter()
                .filter(|x| **x == SmellRule::SentinelEquality)
                .count()
                >= 2
        );
    }

    #[test]
    fn detects_truthy_length() {
        let src = "x = []\nif len(x) > 0: pass\n";
        assert!(rules(&run(src)).contains(&SmellRule::TruthyLengthCheck));
    }

    #[test]
    fn detects_unreachable_after_return() {
        let src = "def f():\n    return 1\n    print(\"dead\")\n";
        assert!(rules(&run(src)).contains(&SmellRule::UnreachableAfterExit));
    }

    #[test]
    fn detects_passthrough() {
        let src = "def wrap(a, b):\n    return inner(a, b)\n";
        assert!(rules(&run(src)).contains(&SmellRule::PassthroughFunction));
    }

    #[test]
    fn skips_method_passthrough() {
        let src = "def wrap(a, b):\n    return self.inner(a, b)\n";
        assert!(!rules(&run(src)).contains(&SmellRule::PassthroughFunction));
    }

    #[test]
    fn detects_stray_print() {
        let src = "def f():\n    print(\"hi\")\n";
        assert!(rules(&run(src)).contains(&SmellRule::StrayPrint));
    }

    #[test]
    fn skips_print_under_main_guard() {
        let src = "if __name__ == \"__main__\":\n    print(\"hi\")\n";
        assert!(!rules(&run(src)).contains(&SmellRule::StrayPrint));
    }

    #[test]
    fn detects_single_method_class() {
        let src = "class Helper:\n    def run(self, x):\n        return x + 1\n";
        assert!(rules(&run(src)).contains(&SmellRule::SingleMethodClass));
    }

    #[test]
    fn skips_class_with_state() {
        let src =
            "class State:\n    counter = 0\n    def run(self):\n        return self.counter\n";
        assert!(!rules(&run(src)).contains(&SmellRule::SingleMethodClass));
    }

    #[test]
    fn detects_high_todo_density() {
        let mut src = String::new();
        for i in 0..6 {
            src.push_str(&format!("# TODO: thing {i}\n"));
        }
        src.push_str("x = 1\n");
        assert!(rules(&run(&src)).contains(&SmellRule::HighTodoDensity));
    }

    #[test]
    fn detects_raise_from_none() {
        let src = "try:\n    pass\nexcept ValueError:\n    raise RuntimeError() from None\n";
        assert!(rules(&run(src)).contains(&SmellRule::RaiseFromNone));
    }

    #[test]
    fn disabled_rule_is_skipped() {
        let src = "def f(x=[]):\n    return x\n";
        let mut disabled = FxHashSet::default();
        disabled.insert(SmellRule::MutableDefault);
        let opts = SmellsOptions {
            disabled,
            ..SmellsOptions::default()
        };
        let path = PathBuf::from("/tmp/test.py");
        let module = parse_source(&path, src).unwrap();
        let issues = analyze_module(&module, &opts, false);
        assert!(!rules(&issues).contains(&SmellRule::MutableDefault));
    }

    // Refinement tests (precision pass)

    #[test]
    fn skips_orm_filter_expressions() {
        let src = "x = Action.deleted_at == None\ny = SimulationResultRecord.excluded == False\n";
        assert!(!rules(&run(src)).contains(&SmellRule::SentinelEquality));
    }

    #[test]
    fn still_flags_local_variable_sentinel_equality() {
        let src = "if x == None: pass\n";
        assert!(rules(&run(src)).contains(&SmellRule::SentinelEquality));
    }

    #[test]
    fn skips_unreachable_in_generator() {
        let src = "async def stream():\n    raise RuntimeError(\"x\")\n    yield\n";
        assert!(!rules(&run(src)).contains(&SmellRule::UnreachableAfterExit));
    }

    #[test]
    fn still_flags_unreachable_in_non_generator() {
        let src = "def f():\n    return 1\n    print(\"dead\")\n";
        assert!(rules(&run(src)).contains(&SmellRule::UnreachableAfterExit));
    }

    #[test]
    fn passthrough_requires_argument_name_match() {
        let src = "def org_id(self):\n    return UUID(\"11111111-1111-1111-1111-111111111111\")\n";
        assert!(!rules(&run(src)).contains(&SmellRule::PassthroughFunction));
    }

    #[test]
    fn passthrough_real_wrapper_still_flagged() {
        let src = "def wrap(a, b):\n    return inner(a, b)\n";
        assert!(rules(&run(src)).contains(&SmellRule::PassthroughFunction));
    }

    #[test]
    fn passthrough_skips_reordered_arguments() {
        let src = "def wrap(a, b):\n    return inner(b, a)\n";
        assert!(!rules(&run(src)).contains(&SmellRule::PassthroughFunction));
    }

    #[test]
    fn pytest_entry_files_skip_single_method_class() {
        let src = "class TestSomething:\n    def test_x(self):\n        assert True\n";
        let path = PathBuf::from("/tmp/test_x.py");
        let module = parse_source(&path, src).unwrap();
        let issues = analyze_module(&module, &SmellsOptions::default(), true);
        assert!(!rules(&issues).contains(&SmellRule::SingleMethodClass));
    }

    #[test]
    fn non_pytest_files_still_flag_single_method_class() {
        let src = "class Helper:\n    def run(self, x):\n        return x + 1\n";
        let path = PathBuf::from("/tmp/helper.py");
        let module = parse_source(&path, src).unwrap();
        let issues = analyze_module(&module, &SmellsOptions::default(), false);
        assert!(rules(&issues).contains(&SmellRule::SingleMethodClass));
    }

    #[test]
    fn pytest_entry_files_skip_truthy_length_check() {
        let src = "def test_x():\n    items = [1, 2]\n    assert len(items) > 0\n";
        let path = PathBuf::from("/tmp/test_x.py");
        let module = parse_source(&path, src).unwrap();
        let issues = analyze_module(&module, &SmellsOptions::default(), true);
        assert!(!rules(&issues).contains(&SmellRule::TruthyLengthCheck));
    }

    #[test]
    fn pytest_entry_files_skip_stray_print() {
        let src = "def test_x():\n    print(\"debug\")\n";
        let path = PathBuf::from("/tmp/test_x.py");
        let module = parse_source(&path, src).unwrap();
        let issues = analyze_module(&module, &SmellsOptions::default(), true);
        assert!(!rules(&issues).contains(&SmellRule::StrayPrint));
    }

    #[test]
    fn pytest_entry_files_skip_passthrough() {
        let src = "def wrap(a, b):\n    return inner(a, b)\n";
        let path = PathBuf::from("/tmp/test_x.py");
        let module = parse_source(&path, src).unwrap();
        let issues = analyze_module(&module, &SmellsOptions::default(), true);
        assert!(!rules(&issues).contains(&SmellRule::PassthroughFunction));
    }
}
