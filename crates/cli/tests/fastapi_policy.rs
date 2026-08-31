//! Issue #9: the FastAPI plugin's policy exemptions — narrow, explainable,
//! and gone when the plugin is disabled.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;

fn pyllow_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pyllow"))
}

/// An idiomatic FastAPI module: routes, Depends() defaults, nested
/// dependencies, Pydantic parsing, and HTTPException translation.
const APP: &str = r#"from fastapi import APIRouter, Depends, HTTPException
from pydantic import BaseModel

router = APIRouter()


class Item(BaseModel):
    id: int
    name: str


def get_settings():
    return {"db": "sqlite://"}


def get_db(settings=Depends(get_settings)):
    return {"url": settings["db"]}


@router.get("/items/{item_id}", response_model=Item)
async def get_item(item_id: int, db=Depends(get_db)):
    try:
        return Item(id=item_id, name="hammer")
    except KeyError:
        raise HTTPException(status_code=404) from None
"#;

fn project(config: &str) -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/api.py"), APP).unwrap();
    if !config.is_empty() {
        fs::write(dir.path().join("pyllow.toml"), config).unwrap();
    }
    dir
}

fn smells_json(root: &Path) -> (i32, Value, String) {
    let out = Command::new(pyllow_bin())
        .args(["smells", root.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap_or(Value::Null),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn idiomatic_fastapi_module_is_clean_with_an_explainable_exemption() {
    let dir = project("");
    let (code, json, stderr) = smells_json(dir.path());
    assert_eq!(code, 0, "{stderr}\n{json}");
    let exemptions = json["stats"]["exemptions"]
        .as_array()
        .expect("exemptions in stats");
    assert_eq!(exemptions.len(), 1);
    let note = exemptions[0].as_str().unwrap();
    assert!(
        note.contains("HTTPException") && note.contains("api.py"),
        "exemption must say what and where: {note}"
    );
    assert!(stderr.contains("exempt"), "humans see it too: {stderr}");
}

#[test]
fn disabling_the_plugin_restores_the_framework_agnostic_rule() {
    let dir = project("[plugins.fastapi]\nenabled = false\n");
    let (code, json, _) = smells_json(dir.path());
    assert_eq!(code, 1);
    let rules: Vec<&str> = json["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["rule"].as_str().unwrap())
        .collect();
    assert!(rules.contains(&"raise-from-none"), "{rules:?}");
    assert!(
        json["stats"].get("exemptions").is_none(),
        "no exemptions when disabled"
    );
}

// Depends() call defaults are the official idiom; they must never trip
// mutable-default. Regression fence — no special-case code exists for this.
#[test]
fn depends_defaults_are_clean() {
    let dir = project("");
    let (_, json, _) = smells_json(dir.path());
    let rules: Vec<&str> = json["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["rule"].as_str().unwrap())
        .collect();
    assert!(!rules.contains(&"mutable-default"), "{rules:?}");
}
