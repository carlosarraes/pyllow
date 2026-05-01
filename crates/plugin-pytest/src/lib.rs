use pyllow_extract::ParsedModule;
use pyllow_types::{FileId, PluginResult};
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::Path;

pub const PLUGIN_NAME: &str = "pytest";

pub fn discover(parsed: &FxHashMap<FileId, ParsedModule>) -> PluginResult {
    let mut entry_files = FxHashSet::default();
    for (id, module) in parsed {
        if file_is_pytest_entry(&module.path) {
            entry_files.insert(*id);
        }
    }
    PluginResult {
        plugin_name: PLUGIN_NAME.to_string(),
        entry_files,
        ..Default::default()
    }
}

/// Path-only check used by analyzers (dupes, health) that want to exempt
/// test files from noise-prone analysis without parsing them first.
pub fn is_pytest_entry_path(path: &Path) -> bool {
    file_is_pytest_entry(path)
}

/// Looser variant of [`is_pytest_entry_path`] — also matches any `.py` file
/// living under a `tests/` (or `test/` / `testing/`) directory. Use for
/// "skip noise on test-adjacent files" filters (dupes, low-MI). Don't use
/// for entry-point discovery — that should stay strict.
pub fn is_test_adjacent_path(path: &Path) -> bool {
    if is_pytest_entry_path(path) {
        return true;
    }
    let extension_is_py = path.extension().and_then(|s| s.to_str()) == Some("py");
    if !extension_is_py {
        return false;
    }
    path.components()
        .any(|c| matches!(c.as_os_str().to_str(), Some("tests" | "test" | "testing")))
}

fn file_is_pytest_entry(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    if name == "conftest.py" {
        return true;
    }
    if let Some(stem) = name.strip_suffix(".py") {
        let test_prefix = stem
            .strip_prefix("test_")
            .is_some_and(|rest| !rest.is_empty());
        let test_suffix = stem
            .strip_suffix("_test")
            .is_some_and(|rest| !rest.is_empty());
        if test_prefix || test_suffix {
            return true;
        }
    }
    is_typecheck_fixture_path(path)
}

/// Files consumed by external type-checkers (pyright, mypy) rather than
/// imported as Python — used as fixtures by pydantic, attrs,
/// dataclasses-json, and others. Treated as live entry points so they
/// don't trip unused-file, and exempted from smells.
fn is_typecheck_fixture_path(path: &Path) -> bool {
    let segments: Vec<&str> = path
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    for window in segments.windows(2) {
        if window == ["mypy", "modules"] || window == ["mypy", "outputs"] {
            return true;
        }
    }
    segments.contains(&"typechecking")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn detects_test_prefix_files() {
        assert!(file_is_pytest_entry(&PathBuf::from("tests/test_module.py")));
        assert!(file_is_pytest_entry(&PathBuf::from(
            "tests/unit/test_widget.py"
        )));
    }

    #[test]
    fn detects_test_suffix_files() {
        assert!(file_is_pytest_entry(&PathBuf::from("tests/widget_test.py")));
        assert!(file_is_pytest_entry(&PathBuf::from("module_test.py")));
    }

    #[test]
    fn detects_conftest() {
        assert!(file_is_pytest_entry(&PathBuf::from("tests/conftest.py")));
        assert!(file_is_pytest_entry(&PathBuf::from(
            "tests/unit/auth/conftest.py"
        )));
    }

    #[test]
    fn ignores_unrelated_files() {
        assert!(!file_is_pytest_entry(&PathBuf::from("src/main.py")));
        assert!(!file_is_pytest_entry(&PathBuf::from("src/testing.py")));
        assert!(!file_is_pytest_entry(&PathBuf::from("src/test.py")));
        assert!(!file_is_pytest_entry(&PathBuf::from("src/_test.py")));
    }

    #[test]
    fn ignores_non_python_files() {
        assert!(!file_is_pytest_entry(&PathBuf::from("test_data.json")));
        assert!(!file_is_pytest_entry(&PathBuf::from("conftest.txt")));
    }

    #[test]
    fn detects_typecheck_fixture_paths() {
        assert!(file_is_pytest_entry(&PathBuf::from(
            "tests/typechecking/secret.py"
        )));
        assert!(file_is_pytest_entry(&PathBuf::from(
            "tests/mypy/modules/dataclass_no_any.py"
        )));
        assert!(file_is_pytest_entry(&PathBuf::from(
            "tests/mypy/outputs/mypy-plugin-strict_ini/plugin_success.py"
        )));
    }

    #[test]
    fn ignores_unrelated_mypy_paths() {
        // Repos that ship a mypy plugin (e.g. pydantic.mypy) should not
        // have their plugin source flagged as a typecheck fixture.
        assert!(!file_is_pytest_entry(&PathBuf::from("pydantic/mypy.py")));
        assert!(!file_is_pytest_entry(&PathBuf::from("src/mypy.py")));
    }

    #[test]
    fn test_adjacent_includes_files_in_tests_directory() {
        // Helpers, factories, fixtures-not-named-conftest all live under
        // tests/ but don't match the strict pytest naming heuristic. The
        // adjacent helper catches them so dupes and MI skip them too.
        assert!(is_test_adjacent_path(&PathBuf::from(
            "tests/integration/simulations/factories.py"
        )));
        assert!(is_test_adjacent_path(&PathBuf::from("tests/helpers.py")));
        assert!(is_test_adjacent_path(&PathBuf::from("test/fixtures.py")));
    }

    #[test]
    fn test_adjacent_keeps_strict_entries_too() {
        // Existing strict matches still count as test-adjacent.
        assert!(is_test_adjacent_path(&PathBuf::from(
            "tests/unit/test_widget.py"
        )));
        assert!(is_test_adjacent_path(&PathBuf::from("conftest.py")));
    }

    #[test]
    fn test_adjacent_excludes_src_files() {
        assert!(!is_test_adjacent_path(&PathBuf::from("src/main.py")));
        assert!(!is_test_adjacent_path(&PathBuf::from("src/testing.py")));
    }
}
