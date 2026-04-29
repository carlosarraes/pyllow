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

fn file_is_pytest_entry(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    if name == "conftest.py" {
        return true;
    }
    if let Some(stem) = name.strip_suffix(".py") {
        let test_prefix = stem.strip_prefix("test_").is_some_and(|rest| !rest.is_empty());
        let test_suffix = stem.strip_suffix("_test").is_some_and(|rest| !rest.is_empty());
        return test_prefix || test_suffix;
    }
    false
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
}
