use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DeclaredDep {
    pub name: String,
    pub source: String,
}

/// Single-pass view of `pyproject.toml`: declared deps + entry-point modules.
/// One disk read and one TOML parse per analysis instead of two.
#[derive(Debug, Clone, Default)]
pub struct Pyproject {
    pub path: PathBuf,
    pub deps: Vec<DeclaredDep>,
    pub entries: Vec<PyprojectEntry>,
}

pub fn read_pyproject(project_root: &Path) -> Pyproject {
    let path = project_root.join("pyproject.toml");
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Pyproject { path, ..Default::default() },
    };
    let parsed: PyProject = match toml::from_str(&raw) {
        Ok(p) => p,
        Err(_) => return Pyproject { path, ..Default::default() },
    };

    let mut deps = Vec::new();
    let mut entries = Vec::new();
    if let Some(project) = parsed.project {
        collect_deps(&project, &mut deps);
        collect_entries(project, &mut entries);
    }
    if let Some(tool) = parsed.tool {
        if let Some(poetry) = tool.poetry {
            if let Some(map) = poetry.dependencies {
                for name in map.into_keys() {
                    if name == "python" {
                        continue;
                    }
                    deps.push(DeclaredDep {
                        name,
                        source: "tool.poetry.dependencies".to_string(),
                    });
                }
            }
        }
    }
    Pyproject { path, deps, entries }
}

fn collect_deps(project: &ProjectTable, out: &mut Vec<DeclaredDep>) {
    if let Some(list) = &project.dependencies {
        for spec in list {
            if let Some(name) = parse_dep_name(spec) {
                out.push(DeclaredDep {
                    name,
                    source: "project.dependencies".to_string(),
                });
            }
        }
    }
}

fn collect_entries(project: ProjectTable, out: &mut Vec<PyprojectEntry>) {
    let push = |spec: &str, group: &str, out: &mut Vec<PyprojectEntry>| {
        if let Some((module, _attr)) = spec.split_once(':') {
            let trimmed = module.trim();
            if !trimmed.is_empty() {
                out.push(PyprojectEntry {
                    module: trimmed.to_string(),
                    group: group.to_string(),
                });
            }
        }
    };
    if let Some(scripts) = project.scripts {
        for spec in scripts.values() {
            push(spec, "scripts", out);
        }
    }
    if let Some(scripts) = project.gui_scripts {
        for spec in scripts.values() {
            push(spec, "gui-scripts", out);
        }
    }
    if let Some(groups) = project.entry_points {
        for (group, group_entries) in &groups {
            for spec in group_entries.values() {
                push(spec, group, out);
            }
        }
    }
}

fn parse_dep_name(spec: &str) -> Option<String> {
    let mut name = spec.trim();
    for sep in ['<', '>', '=', '!', '~', ';', '@', '['] {
        if let Some(idx) = name.find(sep) {
            name = &name[..idx];
        }
    }
    let trimmed = name.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Map a distribution name to its likely top-level Python module name(s).
/// Returns multiple candidates because some packages expose more than one
/// top-level module (e.g., `google-cloud-storage` exposes `google.cloud`).
pub fn dist_to_import_names(dist: &str) -> Vec<String> {
    let lower = dist.to_lowercase();
    if let Some(known) = lookup_known(&lower) {
        return known.iter().map(|s| s.to_string()).collect();
    }
    let normalized = lower.replace('-', "_");
    if normalized != lower {
        vec![normalized.clone(), lower]
    } else {
        vec![lower]
    }
}

fn lookup_known(dist_lower: &str) -> Option<&'static [&'static str]> {
    match dist_lower {
        "pyyaml" => Some(&["yaml"]),
        "pillow" => Some(&["PIL"]),
        "beautifulsoup4" => Some(&["bs4"]),
        "pyjwt" => Some(&["jwt"]),
        "python-dateutil" => Some(&["dateutil"]),
        "scikit-learn" => Some(&["sklearn"]),
        "scikit-image" => Some(&["skimage"]),
        "python-dotenv" => Some(&["dotenv"]),
        "python-multipart" => Some(&["multipart"]),
        "opencv-python" | "opencv-python-headless" => Some(&["cv2"]),
        "msgpack-python" => Some(&["msgpack"]),
        "google-cloud-storage" | "google-cloud-pubsub" | "google-cloud-firestore" => {
            Some(&["google"])
        }
        "google-genai" => Some(&["google"]),
        "google-auth" => Some(&["google"]),
        "google-api-python-client" => Some(&["googleapiclient"]),
        "protobuf" => Some(&["google"]),
        "pyserial" => Some(&["serial"]),
        "pillow-simd" => Some(&["PIL"]),
        "memcached-python" => Some(&["memcache"]),
        "python-socketio" => Some(&["socketio"]),
        "python-engineio" => Some(&["engineio"]),
        "ruamel.yaml" => Some(&["ruamel"]),
        _ => None,
    }
}

/// One declared entry point in pyproject.toml, paired with the group label
/// (synthetic `"scripts"` / `"gui-scripts"` for the top-level tables, or the
/// quoted group from `[project.entry-points."<group>"]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyprojectEntry {
    pub module: String,
    pub group: String,
}

/// Names we never flag as unused: type-stub packages, build-system tools,
/// and packages whose presence has runtime effects unrelated to imports.
pub fn is_implicit_runtime(dist: &str) -> bool {
    let lower = dist.to_lowercase();
    if lower.starts_with("types-") || lower.ends_with("-stubs") {
        return true;
    }
    matches!(
        lower.as_str(),
        // build / packaging
        "setuptools"
            | "wheel"
            | "pip"
            | "build"
            | "hatchling"
            | "poetry"
            | "uv"
            | "pdm"
            | "flit"
            | "maturin"
            | "tox"
            // ASGI / WSGI servers (entry points, not imports)
            | "uvicorn"
            | "gunicorn"
            | "hypercorn"
            | "daphne"
            | "granian"
            // FastAPI / Pydantic transitive runtime deps
            | "python-multipart"
            | "email-validator"
            | "ujson"
            | "orjson"
            // uvicorn standard extras
            | "httptools"
            | "uvloop"
            | "watchfiles"
            | "websockets"
            | "httpx"
    )
}

#[derive(Default, Deserialize)]
struct PyProject {
    project: Option<ProjectTable>,
    tool: Option<ToolTable>,
}

#[derive(Default, Deserialize)]
struct ProjectTable {
    dependencies: Option<Vec<String>>,
    /// `[project.scripts]` — `name = "module.path:attr"` console_scripts.
    scripts: Option<std::collections::BTreeMap<String, String>>,
    /// `[project.gui-scripts]` — same shape as scripts.
    #[serde(rename = "gui-scripts")]
    gui_scripts: Option<std::collections::BTreeMap<String, String>>,
    /// `[project.entry-points."group.name"]` — plugin entry points
    /// (mypy plugins, hypothesis plugins, etc.). The outer map keys group
    /// names, the inner maps `entry_name = "module.path:attr"`.
    #[serde(rename = "entry-points")]
    entry_points: Option<
        std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    >,
}

#[derive(Default, Deserialize)]
struct ToolTable {
    poetry: Option<PoetryTable>,
}

#[derive(Default, Deserialize)]
struct PoetryTable {
    dependencies: Option<std::collections::BTreeMap<String, toml::Value>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_pep_621() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname=\"x\"\ndependencies = [\"fastapi>=0.115\", \"PyYAML\", \"requests\"]",
        )
        .unwrap();
        let names: Vec<String> = read_pyproject(dir.path())
            .deps
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert!(names.iter().any(|n| n == "fastapi"));
        assert!(names.iter().any(|n| n == "PyYAML"));
        assert!(names.iter().any(|n| n == "requests"));
    }

    #[test]
    fn parses_poetry_table() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.poetry]\nname=\"x\"\nversion=\"0.1\"\n[tool.poetry.dependencies]\npython=\"^3.11\"\nfastapi=\"^0.115\"\n",
        )
        .unwrap();
        let names: Vec<String> = read_pyproject(dir.path())
            .deps
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert!(names.iter().any(|n| n == "fastapi"));
        assert!(!names.iter().any(|n| n == "python"));
    }

    #[test]
    fn dist_name_strips_version() {
        assert_eq!(parse_dep_name("fastapi>=0.115,<1").as_deref(), Some("fastapi"));
        assert_eq!(parse_dep_name("requests==2.31.0").as_deref(), Some("requests"));
        assert_eq!(parse_dep_name("uvicorn[standard]>=0.46").as_deref(), Some("uvicorn"));
        assert_eq!(
            parse_dep_name("foo @ git+https://github.com/x/foo.git").as_deref(),
            Some("foo")
        );
    }

    #[test]
    fn dist_to_import_known() {
        assert_eq!(dist_to_import_names("PyYAML"), vec!["yaml"]);
        assert_eq!(dist_to_import_names("Pillow"), vec!["PIL"]);
        assert_eq!(dist_to_import_names("scikit-learn"), vec!["sklearn"]);
    }

    #[test]
    fn dist_to_import_default_normalizes_hyphens() {
        let v = dist_to_import_names("python-jose");
        assert!(v.contains(&"python_jose".to_string()));
        assert!(v.contains(&"python-jose".to_string()));
    }

    #[test]
    fn implicit_runtime_skips_stubs_and_tools() {
        assert!(is_implicit_runtime("types-PyYAML"));
        assert!(is_implicit_runtime("django-stubs"));
        assert!(is_implicit_runtime("uvicorn"));
        assert!(is_implicit_runtime("setuptools"));
        assert!(!is_implicit_runtime("fastapi"));
    }

    #[test]
    fn entries_from_scripts_carry_group_label() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname=\"x\"\n[project.scripts]\npyllow = \"pyllow.cli:main\"\n",
        )
        .unwrap();
        assert_eq!(
            read_pyproject(dir.path()).entries,
            vec![PyprojectEntry {
                module: "pyllow.cli".into(),
                group: "scripts".into(),
            }]
        );
    }

    #[test]
    fn entries_from_entry_points_groups_preserve_group_name() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname=\"pydantic\"\n\n[project.entry-points.\"mypy.plugins\"]\npydantic_mypy = \"pydantic.mypy:plugin\"\n\n[project.entry-points.\"hypothesis\"]\n_register = \"pydantic.v1._hypothesis_plugin:_register\"\n",
        )
        .unwrap();
        let mut entries = read_pyproject(dir.path()).entries;
        entries.sort_by(|a, b| a.module.cmp(&b.module));
        assert_eq!(
            entries,
            vec![
                PyprojectEntry {
                    module: "pydantic.mypy".into(),
                    group: "mypy.plugins".into(),
                },
                PyprojectEntry {
                    module: "pydantic.v1._hypothesis_plugin".into(),
                    group: "hypothesis".into(),
                },
            ]
        );
    }

    #[test]
    fn entries_handle_missing_pyproject() {
        let dir = tempfile::tempdir().unwrap();
        let result = read_pyproject(dir.path());
        assert!(result.entries.is_empty());
        assert!(result.deps.is_empty());
    }
}
