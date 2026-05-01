use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DeclaredDep {
    pub name: String,
    pub source: String,
    /// Path of the `pyproject.toml` that declared this dep. In a uv /
    /// hatch workspace each member has its own pyproject; the unused-dep
    /// finding has to point at the file where the dep actually lives, not
    /// at the workspace marker that doesn't even mention it.
    pub source_path: PathBuf,
}

/// Single-pass view of `pyproject.toml`: declared deps + entry-point modules.
/// One disk read and one TOML parse per analysis instead of two.
#[derive(Debug, Clone, Default)]
pub struct Pyproject {
    pub path: PathBuf,
    pub deps: Vec<DeclaredDep>,
    pub entries: Vec<PyprojectEntry>,
    /// All `[project] name` values found across the search roots — used
    /// by library-mode entry detection to find each package's public-API
    /// `__init__.py`. A monorepo with multiple sibling library packages
    /// contributes one name per member; storing only the first would
    /// leave the others' public APIs falsely flagged as unused-file.
    pub project_names: Vec<String>,
}

pub fn read_pyproject(project_root: &Path) -> Pyproject {
    let path = project_root.join("pyproject.toml");
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => {
            return Pyproject {
                path,
                ..Default::default()
            }
        }
    };
    let parsed: PyProject = match toml::from_str(&raw) {
        Ok(p) => p,
        Err(_) => {
            return Pyproject {
                path,
                ..Default::default()
            }
        }
    };

    let mut deps = Vec::new();
    let mut entries = Vec::new();
    let mut project_names = Vec::new();
    if let Some(project) = parsed.project {
        if let Some(name) = project.name.clone() {
            project_names.push(name);
        }
        collect_deps(&project, &path, &mut deps);
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
                        source_path: path.clone(),
                    });
                }
            }
        }
    }
    Pyproject {
        path,
        deps,
        entries,
        project_names,
    }
}

/// Read pyproject metadata from each candidate root and merge into one
/// view. Used by the analyzer pipeline because uv/hatch workspaces have a
/// bare top-level pyproject (no `[project]`, no deps) plus per-member
/// pyprojects that carry the real metadata. Reading only the workspace
/// root would silently drop every member's deps, scripts, and
/// `[project] name`.
///
/// Aggregation rules:
/// - `path` is the first existing pyproject (used as the canonical
///   location for diagnostics that don't have a per-dep path).
/// - `deps` are concatenated, each tagged with its own source pyproject.
/// - `entries` are concatenated.
/// - `project_names` are concatenated and deduplicated; each member of a
///   workspace contributes one, and library-mode entry detection seeds
///   one `LibraryPublicApi` entry per name so every member's public API
///   stays reachable.
pub fn read_pyprojects(roots: &[PathBuf]) -> Pyproject {
    let mut all_deps = Vec::new();
    let mut all_entries = Vec::new();
    let mut all_names: Vec<String> = Vec::new();
    let mut primary_path = None;

    for root in roots {
        let py = read_pyproject(root);
        if py.path.is_file() && primary_path.is_none() {
            primary_path = Some(py.path.clone());
        }
        for name in py.project_names {
            if !all_names.contains(&name) {
                all_names.push(name);
            }
        }
        all_deps.extend(py.deps);
        all_entries.extend(py.entries);
    }

    Pyproject {
        path: primary_path.unwrap_or_else(|| {
            roots
                .first()
                .map(|r| r.join("pyproject.toml"))
                .unwrap_or_default()
        }),
        deps: all_deps,
        entries: all_entries,
        project_names: all_names,
    }
}

fn collect_deps(project: &ProjectTable, source_path: &Path, out: &mut Vec<DeclaredDep>) {
    if let Some(list) = &project.dependencies {
        for spec in list {
            if let Some(name) = parse_dep_name(spec) {
                out.push(DeclaredDep {
                    name,
                    source: "project.dependencies".to_string(),
                    source_path: source_path.to_path_buf(),
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

/// Candidate import-name modules a `[project] name` could resolve to.
/// PEP 503 normalization (lowercase + underscores) is the most common
/// dist→module mapping, but the literal name and lowercased name also show
/// up in practice. Order matters: more-canonical guesses first so
/// `resolver.resolve_dotted` short-circuits on the right answer.
pub fn library_init_candidates(project_name: &str) -> Vec<String> {
    let trimmed = project_name.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let lower = trimmed.to_lowercase();
    let pep503 = lower.replace(['-', '.'], "_");
    let mut out = Vec::new();
    if !pep503.is_empty() {
        out.push(pep503.clone());
    }
    if lower != pep503 && !lower.is_empty() {
        out.push(lower);
    }
    if trimmed != out[0] {
        out.push(trimmed.to_string());
    }
    out.dedup();
    out
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
            // Database drivers — loaded by string from a SQLAlchemy /
            // Django / asyncpg URL, never imported directly. Ubiquitous
            // false positives in any backend project. Conservative list:
            // packages here MUST be ones users almost never `import` by
            // name (they sit behind a connection-string adapter). Things
            // like `redis` or `duckdb` are deliberately excluded — those
            // ARE commonly imported directly.
            | "psycopg"
            | "psycopg2"
            | "psycopg2-binary"
            | "psycopg-binary"
            | "asyncpg"
            | "aiopg"
            | "mysqlclient"
            | "pymysql"
            | "aiomysql"
            | "asyncmy"
            | "cx-oracle"
            | "cx_oracle"
            | "oracledb"
            | "pyodbc"
            | "pymssql"
            | "aioodbc"
    )
}

#[derive(Default, Deserialize)]
struct PyProject {
    project: Option<ProjectTable>,
    tool: Option<ToolTable>,
}

#[derive(Default, Deserialize)]
struct ProjectTable {
    name: Option<String>,
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
    entry_points:
        Option<std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>>,
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
        assert_eq!(
            parse_dep_name("fastapi>=0.115,<1").as_deref(),
            Some("fastapi")
        );
        assert_eq!(
            parse_dep_name("requests==2.31.0").as_deref(),
            Some("requests")
        );
        assert_eq!(
            parse_dep_name("uvicorn[standard]>=0.46").as_deref(),
            Some("uvicorn")
        );
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
    fn implicit_runtime_covers_url_loaded_db_drivers() {
        // SQLAlchemy / Django ORM use connection strings; the underlying
        // driver dist is declared in pyproject but never imported. Without
        // these, `psycopg2-binary` shows up as unused-dep on every backend.
        assert!(is_implicit_runtime("psycopg2-binary"));
        assert!(is_implicit_runtime("psycopg"));
        assert!(is_implicit_runtime("asyncpg"));
        assert!(is_implicit_runtime("mysqlclient"));
        assert!(is_implicit_runtime("oracledb"));
        // But things users do `import` directly stay flaggable.
        assert!(!is_implicit_runtime("redis"));
        assert!(!is_implicit_runtime("duckdb"));
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
    fn library_init_candidates_normalizes_dist_to_module() {
        // PEP 503: dashes/dots in dist names map to underscores in modules.
        assert_eq!(library_init_candidates("Flask"), vec!["flask", "Flask"]);
        assert_eq!(
            library_init_candidates("scikit-learn"),
            vec!["scikit_learn", "scikit-learn"]
        );
        assert_eq!(
            library_init_candidates("ruamel.yaml"),
            vec!["ruamel_yaml", "ruamel.yaml"]
        );
    }

    #[test]
    fn library_init_candidates_returns_empty_for_blank_name() {
        assert!(library_init_candidates("").is_empty());
        assert!(library_init_candidates("   ").is_empty());
    }

    #[test]
    fn pyproject_exposes_project_name() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"my-lib\"\n",
        )
        .unwrap();
        assert_eq!(
            read_pyproject(dir.path()).project_names,
            vec!["my-lib".to_string()]
        );
    }

    #[test]
    fn read_pyprojects_collects_all_member_names() {
        // Two sibling library packages each with `[project] name`. Both
        // names must reach `project_names` so each library's
        // `__init__.py` can be seeded as a public-API entry.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::write(
            dir.join("pyproject.toml"),
            "[tool.uv.workspace]\nmembers = [\"libA\", \"libB\"]\n",
        )
        .unwrap();
        for member in ["libA", "libB"] {
            fs::create_dir_all(dir.join(member)).unwrap();
            fs::write(
                dir.join(member).join("pyproject.toml"),
                format!("[project]\nname=\"{member}\"\n"),
            )
            .unwrap();
        }
        let merged = read_pyprojects(&[dir.clone(), dir.join("libA"), dir.join("libB")]);
        assert_eq!(
            merged.project_names,
            vec!["libA".to_string(), "libB".to_string()]
        );
    }

    #[test]
    fn read_pyprojects_aggregates_workspace_member_metadata() {
        // Workspace root carries a marker only; the member pyproject has
        // the real `[project]` block, deps, and entries.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        fs::write(
            dir.join("pyproject.toml"),
            "[tool.uv.workspace]\nmembers = [\"backend\"]\n",
        )
        .unwrap();
        fs::create_dir_all(dir.join("backend")).unwrap();
        fs::write(
            dir.join("backend/pyproject.toml"),
            "[project]\nname=\"app\"\ndependencies = [\"requests\"]\n[project.scripts]\napp = \"app.cli:main\"\n",
        )
        .unwrap();
        let merged = read_pyprojects(&[dir.clone(), dir.join("backend")]);
        assert_eq!(merged.project_names, vec!["app".to_string()]);
        assert_eq!(merged.deps.len(), 1);
        assert_eq!(merged.deps[0].name, "requests");
        // The dep's source_path must point at the file it was declared
        // in, not the workspace root marker.
        assert!(
            merged.deps[0]
                .source_path
                .ends_with("backend/pyproject.toml"),
            "got {:?}",
            merged.deps[0].source_path
        );
        assert_eq!(merged.entries.len(), 1);
        assert_eq!(merged.entries[0].module, "app.cli");
    }

    #[test]
    fn entries_handle_missing_pyproject() {
        let dir = tempfile::tempdir().unwrap();
        let result = read_pyproject(dir.path());
        assert!(result.entries.is_empty());
        assert!(result.deps.is_empty());
    }
}
