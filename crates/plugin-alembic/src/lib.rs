use pyllow_extract::ast::Stmt;
use pyllow_extract::ParsedModule;
use pyllow_types::{FileId, ImportKind, PluginResult};
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::Path;

pub const PLUGIN_NAME: &str = "alembic";

pub fn discover(parsed: &FxHashMap<FileId, ParsedModule>) -> PluginResult {
    let mut entry_files = FxHashSet::default();
    for (id, module) in parsed {
        if module_is_alembic_entry(module) {
            entry_files.insert(*id);
        }
    }
    PluginResult {
        plugin_name: PLUGIN_NAME.to_string(),
        entry_files,
        ..Default::default()
    }
}

fn module_is_alembic_entry(module: &ParsedModule) -> bool {
    // Path-based: `alembic/versions/*.py` and `env.py` in an alembic dir
    // are auto-discovered by Alembic regardless of imports.
    if path_is_alembic_versioned(&module.path) || path_is_alembic_env(&module.path) {
        return true;
    }
    if !imports_alembic(module) {
        return false;
    }
    // Otherwise: must have at least one upgrade()/downgrade() pair.
    has_upgrade_and_downgrade(&module.suite)
}

fn imports_alembic(module: &ParsedModule) -> bool {
    module.imports.iter().any(|i| {
        matches!(i.kind, ImportKind::Absolute)
            && (i.raw == "alembic" || i.raw.starts_with("alembic."))
    })
}

fn path_is_alembic_versioned(path: &Path) -> bool {
    let mut components: Vec<_> = path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    let leaf = components.pop().unwrap_or_default();
    if !leaf.ends_with(".py") {
        return false;
    }
    // Look for `<dir>/versions/<file>.py` where <dir> is anything alembic-like.
    components
        .windows(2)
        .any(|w| w[1] == "versions" && (w[0] == "alembic" || w[0].ends_with("migrations")))
}

fn path_is_alembic_env(path: &Path) -> bool {
    let leaf = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    if leaf != "env.py" {
        return false;
    }
    // env.py is meaningful as alembic config only when it lives next to a
    // `versions/` sibling or under an `alembic/` parent.
    path.parent()
        .map(|p| {
            p.file_name().and_then(|s| s.to_str()) == Some("alembic")
                || p.join("versions").is_dir()
        })
        .unwrap_or(false)
}

fn has_upgrade_and_downgrade(body: &[Stmt]) -> bool {
    let mut up = false;
    let mut down = false;
    for s in body {
        let name = match s {
            Stmt::FunctionDef(f) => f.name.as_str(),
            Stmt::AsyncFunctionDef(f) => f.name.as_str(),
            _ => continue,
        };
        if name == "upgrade" {
            up = true;
        } else if name == "downgrade" {
            down = true;
        }
    }
    up && down
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_extract::parse_source;
    use std::path::PathBuf;

    fn parse_at(p: &str, src: &str) -> ParsedModule {
        let mut m = parse_source(Path::new("placeholder.py"), src).unwrap();
        m.path = PathBuf::from(p);
        m
    }

    fn parse(src: &str) -> ParsedModule {
        parse_source(Path::new("placeholder.py"), src).unwrap()
    }

    #[test]
    fn detects_versioned_migration_by_path() {
        let m = parse_at("alembic/versions/0042_init.py", "x = 1\n");
        assert!(module_is_alembic_entry(&m));
    }

    #[test]
    fn detects_versioned_migration_under_custom_migrations_dir() {
        let m = parse_at("db_migrations/versions/0001_init.py", "x = 1\n");
        assert!(module_is_alembic_entry(&m));
    }

    #[test]
    fn detects_upgrade_downgrade_pair_with_alembic_import() {
        let m = parse(
            "from alembic import op\ndef upgrade():\n    pass\ndef downgrade():\n    pass\n",
        );
        assert!(module_is_alembic_entry(&m));
    }

    #[test]
    fn ignores_upgrade_only_function() {
        let m = parse(
            "from alembic import op\ndef upgrade():\n    pass\n",
        );
        assert!(!module_is_alembic_entry(&m));
    }

    #[test]
    fn ignores_module_without_alembic_import_or_path() {
        let m = parse(
            "def upgrade():\n    pass\ndef downgrade():\n    pass\n",
        );
        assert!(!module_is_alembic_entry(&m));
    }

    #[test]
    fn ignores_unrelated_versions_directory() {
        let m = parse_at("src/versions/api.py", "x = 1\n");
        assert!(!module_is_alembic_entry(&m));
    }
}
