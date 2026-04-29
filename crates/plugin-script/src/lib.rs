use pyllow_extract::ParsedModule;
use pyllow_types::{FileId, PluginResult};
use rustc_hash::{FxHashMap, FxHashSet};

pub const PLUGIN_NAME: &str = "script";

pub fn discover(parsed: &FxHashMap<FileId, ParsedModule>) -> PluginResult {
    let mut entry_files = FxHashSet::default();
    for (id, module) in parsed {
        if is_script_module(module) {
            entry_files.insert(*id);
        }
    }
    PluginResult {
        plugin_name: PLUGIN_NAME.to_string(),
        entry_files,
        ..Default::default()
    }
}

fn is_script_module(module: &ParsedModule) -> bool {
    if module.is_script_entry {
        return true;
    }
    module
        .path
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s == "__main__.py")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_extract::parse_source;
    use std::path::Path;

    fn module_with_path(path: &str, src: &str) -> ParsedModule {
        parse_source(Path::new(path), src).unwrap()
    }

    #[test]
    fn detects_dunder_main_py_basename() {
        let m = module_with_path("pkg/__main__.py", "from pkg import lib\nlib.run()\n");
        assert!(is_script_module(&m));
    }

    #[test]
    fn detects_if_name_main_guard() {
        let m = module_with_path("script.py", "if __name__ == \"__main__\":\n    pass\n");
        assert!(is_script_module(&m));
    }

    #[test]
    fn ignores_regular_modules() {
        let m = module_with_path("src/lib.py", "def helper():\n    pass\n");
        assert!(!is_script_module(&m));
    }

    #[test]
    fn ignores_main_py_without_guard() {
        let m = module_with_path("src/main.py", "from x import y\ny()\n");
        assert!(!is_script_module(&m));
    }
}
