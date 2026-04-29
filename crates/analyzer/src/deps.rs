use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct DeclaredDep {
    pub name: String,
    pub source: String,
}

pub fn read_dependencies(project_root: &Path) -> (PathBuf, Vec<DeclaredDep>) {
    let pyproject = project_root.join("pyproject.toml");
    let raw = match fs::read_to_string(&pyproject) {
        Ok(s) => s,
        Err(_) => return (pyproject, Vec::new()),
    };

    let parsed: PyProject = match toml::from_str(&raw) {
        Ok(p) => p,
        Err(_) => return (pyproject, Vec::new()),
    };

    let mut deps = Vec::new();

    if let Some(project) = &parsed.project {
        if let Some(list) = &project.dependencies {
            for spec in list {
                if let Some(name) = parse_dep_name(spec) {
                    deps.push(DeclaredDep {
                        name,
                        source: "project.dependencies".to_string(),
                    });
                }
            }
        }
    }

    if let Some(tool) = &parsed.tool {
        if let Some(poetry) = &tool.poetry {
            if let Some(map) = &poetry.dependencies {
                for name in map.keys() {
                    if name == "python" {
                        continue;
                    }
                    deps.push(DeclaredDep {
                        name: name.clone(),
                        source: "tool.poetry.dependencies".to_string(),
                    });
                }
            }
        }
    }

    (pyproject, deps)
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
        let (_, deps) = read_dependencies(dir.path());
        let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"fastapi"));
        assert!(names.contains(&"PyYAML"));
        assert!(names.contains(&"requests"));
    }

    #[test]
    fn parses_poetry_table() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.poetry]\nname=\"x\"\nversion=\"0.1\"\n[tool.poetry.dependencies]\npython=\"^3.11\"\nfastapi=\"^0.115\"\n",
        )
        .unwrap();
        let (_, deps) = read_dependencies(dir.path());
        let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"fastapi"));
        assert!(!names.contains(&"python"));
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
}
