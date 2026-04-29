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
    UnusedFile {
        path: PathBuf,
    },
    UnusedImport {
        path: PathBuf,
        line: u32,
        name: String,
        module: String,
    },
    UnusedDep {
        path: PathBuf,
        name: String,
        source: String,
    },
    Duplicate {
        token_count: u32,
        occurrences: Vec<DuplicateOccurrence>,
    },
    Complexity {
        path: PathBuf,
        line: u32,
        function: String,
        cyclomatic: u32,
        cognitive: u32,
    },
    LowMaintainability {
        path: PathBuf,
        score: u32,
        avg_cyclomatic: f32,
        loc: u32,
    },
    Hotspot {
        path: PathBuf,
        cyclomatic: u32,
        churn: u32,
        score: f32,
    },
    Smell {
        path: PathBuf,
        line: u32,
        rule: SmellRule,
        detail: String,
    },
}

/// Stable identifiers for smell rules. Used for config (`[smells].disabled`),
/// baselines, and JSON output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SmellRule {
    MutableDefault,
    BroadExcept,
    SentinelEquality,
    TruthyLengthCheck,
    UnreachableAfterExit,
    PassthroughFunction,
    StrayPrint,
    SingleMethodClass,
    HighTodoDensity,
    RaiseFromNone,
}

impl SmellRule {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MutableDefault => "mutable-default",
            Self::BroadExcept => "broad-except",
            Self::SentinelEquality => "sentinel-equality",
            Self::TruthyLengthCheck => "truthy-length-check",
            Self::UnreachableAfterExit => "unreachable-after-exit",
            Self::PassthroughFunction => "passthrough-function",
            Self::StrayPrint => "stray-print",
            Self::SingleMethodClass => "single-method-class",
            Self::HighTodoDensity => "high-todo-density",
            Self::RaiseFromNone => "raise-from-none",
        }
    }

    pub fn all() -> &'static [SmellRule] {
        &[
            Self::MutableDefault,
            Self::BroadExcept,
            Self::SentinelEquality,
            Self::TruthyLengthCheck,
            Self::UnreachableAfterExit,
            Self::PassthroughFunction,
            Self::StrayPrint,
            Self::SingleMethodClass,
            Self::HighTodoDensity,
            Self::RaiseFromNone,
        ]
    }
}

impl std::str::FromStr for SmellRule {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        for r in Self::all() {
            if r.as_str() == s {
                return Ok(*r);
            }
        }
        Err(format!("unknown smell rule: {s}"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateOccurrence {
    pub path: PathBuf,
    pub start_line: u32,
    pub end_line: u32,
}

impl Issue {
    pub fn path(&self) -> &std::path::Path {
        match self {
            Issue::UnusedFile { path } => path,
            Issue::UnusedImport { path, .. } => path,
            Issue::UnusedDep { path, .. } => path,
            Issue::Duplicate { occurrences, .. } => {
                occurrences
                    .first()
                    .map(|o| o.path.as_path())
                    .unwrap_or_else(|| std::path::Path::new(""))
            }
            Issue::Complexity { path, .. } => path,
            Issue::LowMaintainability { path, .. } => path,
            Issue::Hotspot { path, .. } => path,
            Issue::Smell { path, .. } => path,
        }
    }

    pub fn line(&self) -> Option<u32> {
        match self {
            Issue::UnusedFile { .. }
            | Issue::UnusedDep { .. }
            | Issue::LowMaintainability { .. }
            | Issue::Hotspot { .. } => None,
            Issue::UnusedImport { line, .. } => Some(*line),
            Issue::Duplicate { occurrences, .. } => occurrences.first().map(|o| o.start_line),
            Issue::Complexity { line, .. } => Some(*line),
            Issue::Smell { line, .. } => Some(*line),
        }
    }

    /// Stable kebab-case rule identifier used by suppressions, baselines, and JSON output.
    pub fn rule_key(&self) -> &'static str {
        match self {
            Issue::UnusedFile { .. } => "unused-file",
            Issue::UnusedImport { .. } => "unused-import",
            Issue::UnusedDep { .. } => "unused-dep",
            Issue::Duplicate { .. } => "duplicate",
            Issue::Complexity { .. } => "complexity",
            Issue::LowMaintainability { .. } => "low-maintainability",
            Issue::Hotspot { .. } => "hotspot",
            Issue::Smell { rule, .. } => rule.as_str(),
        }
    }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryEntryPoint {
    pub path: PathBuf,
    pub dotted_module: String,
    pub source: EntryPointSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryFile {
    pub path: PathBuf,
    pub dotted_module: String,
    pub kind: ModuleKind,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Inventory {
    pub entry_points: Vec<InventoryEntryPoint>,
    pub files: Vec<InventoryFile>,
    pub plugins_run: Vec<String>,
}
