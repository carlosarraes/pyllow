use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileId(pub u32);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleNode {
    pub id: FileId,
    pub path: PathBuf,
    pub kind: ModuleKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModuleKind {
    Module,
    PackageInit,
    NamespacePackage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportSpecifier {
    pub raw: String,
    pub kind: ImportKind,
    pub is_conditional: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportKind {
    Absolute,
    Relative { level: u32 },
    DynamicLiteral,
    DynamicOpaque,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: FileId,
    pub to: FileId,
    pub specifier: ImportSpecifier,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryPoint {
    pub file: FileId,
    pub source: EntryPointSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EntryPointSource {
    Config,
    Plugin(String),
    ScriptEntryPoint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Issue {
    UnusedFile { path: PathBuf },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginResult {
    pub plugin_name: String,
    pub entry_files: FxHashSet<FileId>,
    pub entry_patterns: Vec<String>,
    pub used_symbols: Vec<UsedSymbol>,
    pub implicit_dependencies: Vec<String>,
    pub path_aliases: FxHashMap<String, PathBuf>,
    pub excluded_files: FxHashSet<FileId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsedSymbol {
    pub file: FileId,
    pub symbol: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnalysisResults {
    pub issues: Vec<Issue>,
    pub stats: AnalysisStats,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnalysisStats {
    pub files_scanned: usize,
    pub entry_points: usize,
    pub plugins_run: Vec<String>,
    pub elapsed_ms: u64,
}
