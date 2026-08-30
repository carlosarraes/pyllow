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
    /// In a branch that may not run at runtime (TYPE_CHECKING block,
    /// try/except-ImportError arm, or any `except` handler body).
    pub is_conditional: bool,
    /// Strictly never executes at runtime — only `if TYPE_CHECKING:`
    /// imports. Used by graph reachability so type-only imports don't
    /// keep dead modules alive (try-fallback imports do, so they're
    /// `is_conditional` but not `is_type_only`).
    #[serde(default)]
    pub is_type_only: bool,
    /// 1-indexed line number of the import statement in the source file.
    /// `0` means "unknown" — used by callers that construct an
    /// `ImportSpecifier` without source context (e.g., synthetic test fixtures).
    #[serde(default)]
    pub line: u32,
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
    /// PEP 562: module defines `__getattr__` at top level — deliberate
    /// dynamic-attribute surface used by external importers (e.g.,
    /// `getattr_migration` shims for backward compatibility).
    ModuleGetattr,
    /// Declared in `pyproject.toml` as a console script
    /// (`[project.scripts]`), GUI script (`[project.gui-scripts]`), or
    /// plugin entry point (`[project.entry-points."<group>"]`). The string
    /// carries the group label so `pyllow list entry-points` can attribute
    /// the entry to its source table (e.g., `mypy.plugins`, `scripts`).
    PyprojectEntryPoint(String),
    /// Top-level `__init__.py` of a library matching `[project] name`.
    /// Without this, library public APIs look unreachable to pyllow.
    LibraryPublicApi,
}

/// One `[[smells.banned_api]]` entry: a fully qualified Python API a project
/// prohibits. `id` becomes the finding's rule key, so it must not collide with
/// a built-in rule and is validated at config load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BannedApi {
    pub id: String,
    /// Dotted qualified name, e.g. `typing.cast` or `unittest.mock.patch`.
    pub path: String,
    pub message: String,
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
        /// 1-indexed last line of the analyzed function body, inclusive. The
        /// metric is computed over the whole body, so diff scoping must match
        /// any line in `line..=end_line`, not just the `def` line. `0` means
        /// the range was unavailable (legacy serialized form).
        #[serde(default)]
        end_line: u32,
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
    CircularDependency {
        /// Files that form the cycle, sorted for stable output.
        /// First element is also reused as the issue's primary `path()`.
        cycle: Vec<PathBuf>,
    },
    RefactorTarget {
        path: PathBuf,
        line: u32,
        /// 1-indexed last line of the analyzed function body, inclusive.
        /// See [`Issue::Complexity::end_line`]. `0` means unavailable.
        #[serde(default)]
        end_line: u32,
        function: String,
        cyclomatic: u32,
        cognitive: u32,
        effort: Effort,
    },
    FeatureFlag {
        path: PathBuf,
        line: u32,
        flag: String,
        provider: FlagProvider,
    },
    /// A file the project tried to analyze but couldn't parse — bad syntax,
    /// unsupported Python construct, IO error. Surfaced as a first-class
    /// issue (rather than a stderr warning) so CI fails instead of silently
    /// excluding the file from every other check.
    ParseError {
        path: PathBuf,
        message: String,
    },
    /// Cross-zone import violation. The file in `from_zone` imports something
    /// from a file in `to_zone` that the configured `[[boundaries.rules]]`
    /// don't allow. `from_line` is the 1-indexed line of the offending
    /// import statement in `from_path`; `0` means line info wasn't available
    /// (synthesized issue / legacy serialized form).
    BoundaryViolation {
        from_path: PathBuf,
        from_line: u32,
        from_zone: String,
        to_path: PathBuf,
        to_zone: String,
    },
    /// Use of an API prohibited by `[[smells.banned_api]]`. `id` is the
    /// configured rule ID and doubles as this finding's rule key; `api` is the
    /// resolved qualified name that matched. Range covers the offending
    /// expression, one-based inclusive.
    BannedApi {
        path: PathBuf,
        line: u32,
        end_line: u32,
        id: String,
        api: String,
        message: String,
    },
}

/// Source of a feature-flag reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FlagProvider {
    /// `os.environ.get("FEATURE_*")`
    EnvVar,
    /// Django `settings.FEATURES["name"]`
    DjangoSettings,
    /// `client.variation("flag-key", ...)`
    LaunchDarkly,
    /// `Statsig.check_gate("gate-name", ...)`
    Statsig,
    /// `unleash.is_enabled("flag-name", ...)`
    Unleash,
    /// `growthbook.is_on("flag-name")` / `growthbook.feature_value(...)`
    GrowthBook,
}

impl FlagProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::EnvVar => "env-var",
            Self::DjangoSettings => "django-settings",
            Self::LaunchDarkly => "launchdarkly",
            Self::Statsig => "statsig",
            Self::Unleash => "unleash",
            Self::GrowthBook => "growthbook",
        }
    }
}

/// Estimated refactoring effort for a code-quality target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
}

impl Effort {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

impl std::str::FromStr for Effort {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "low" => Ok(Self::Low),
            "medium" | "med" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            _ => Err(format!("unknown effort: {s} (expected low|medium|high)")),
        }
    }
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
    MoneyAsFloat,
    /// Family gate for `[[smells.banned_api]]`. Ships disabled; individual
    /// findings are keyed by their configured ID, not by this name.
    BannedApi,
    /// Explicit `typing.Any` in an annotation. Ships disabled.
    NoExplicitAny,
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
            Self::MoneyAsFloat => "money-as-float",
            Self::BannedApi => "banned-api",
            Self::NoExplicitAny => "no-explicit-any",
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
            Self::MoneyAsFloat,
            Self::BannedApi,
            Self::NoExplicitAny,
        ]
    }
}

impl SmellRule {
    /// Whether this rule runs without being named in `[smells].enabled`.
    ///
    /// Every rule shipped so far is default-on. Strict/opinionated rules added
    /// later (issues #2 and #3) return `false` here so existing projects gain
    /// no new findings simply by upgrading.
    pub fn default_enabled(&self) -> bool {
        match self {
            Self::MutableDefault
            | Self::BroadExcept
            | Self::SentinelEquality
            | Self::TruthyLengthCheck
            | Self::UnreachableAfterExit
            | Self::PassthroughFunction
            | Self::StrayPrint
            | Self::SingleMethodClass
            | Self::HighTodoDensity
            | Self::RaiseFromNone
            | Self::MoneyAsFloat => true,
            Self::BannedApi | Self::NoExplicitAny => false,
        }
    }
}

/// Resolve which smell rules run, from a candidate set of `(rule, default_on)`
/// pairs plus the project's explicit opt-ins and opt-outs.
///
/// Composition order is: start from the default-on rules, add everything in
/// `enabled`, then remove everything in `disabled`. **`disabled` wins over
/// `enabled`** — being able to turn a rule off unconditionally is the property
/// that matters when a shared config enables something a project cannot adopt.
pub fn resolve_smell_rules(
    candidates: impl IntoIterator<Item = (SmellRule, bool)>,
    enabled: &[SmellRule],
    disabled: &[SmellRule],
) -> rustc_hash::FxHashSet<SmellRule> {
    let mut active: rustc_hash::FxHashSet<SmellRule> = candidates
        .into_iter()
        .filter(|(_, default_on)| *default_on)
        .map(|(rule, _)| rule)
        .collect();
    active.extend(enabled.iter().copied());
    for rule in disabled {
        active.remove(rule);
    }
    active
}

/// Convenience wrapper over [`resolve_smell_rules`] using every known rule and
/// its shipped default.
pub fn active_smell_rules(
    enabled: &[SmellRule],
    disabled: &[SmellRule],
) -> rustc_hash::FxHashSet<SmellRule> {
    resolve_smell_rules(
        SmellRule::all().iter().map(|r| (*r, r.default_enabled())),
        enabled,
        disabled,
    )
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
            Issue::Duplicate { occurrences, .. } => occurrences
                .first()
                .map(|o| o.path.as_path())
                .unwrap_or_else(|| std::path::Path::new("")),
            Issue::Complexity { path, .. } => path,
            Issue::LowMaintainability { path, .. } => path,
            Issue::Hotspot { path, .. } => path,
            Issue::Smell { path, .. } => path,
            Issue::CircularDependency { cycle } => cycle
                .first()
                .map(|p| p.as_path())
                .unwrap_or_else(|| std::path::Path::new("")),
            Issue::RefactorTarget { path, .. } => path,
            Issue::FeatureFlag { path, .. } => path,
            Issue::ParseError { path, .. } => path,
            Issue::BoundaryViolation { from_path, .. } => from_path,
            Issue::BannedApi { path, .. } => path,
        }
    }

    pub fn line(&self) -> Option<u32> {
        match self {
            Issue::UnusedFile { .. }
            | Issue::UnusedDep { .. }
            | Issue::LowMaintainability { .. }
            | Issue::Hotspot { .. }
            | Issue::CircularDependency { .. }
            | Issue::ParseError { .. } => None,
            Issue::UnusedImport { line, .. } => Some(*line),
            Issue::Duplicate { occurrences, .. } => occurrences.first().map(|o| o.start_line),
            Issue::Complexity { line, .. } => Some(*line),
            Issue::Smell { line, .. } => Some(*line),
            Issue::RefactorTarget { line, .. } => Some(*line),
            Issue::FeatureFlag { line, .. } => Some(*line),
            // A `from_line` of 0 means line info was not available; treat as
            // "no line" so audit's line-scope fallback uses file matching.
            Issue::BoundaryViolation { from_line, .. } => {
                if *from_line == 0 {
                    None
                } else {
                    Some(*from_line)
                }
            }
            Issue::BannedApi { line, .. } => Some(*line),
        }
    }

    /// Inclusive 1-indexed `(start_line, end_line)` span this issue occupies.
    /// `None` for file-level issues that own no specific range.
    pub fn range(&self) -> Option<(u32, u32)> {
        match self {
            // File-level: no range to own. Diff scoping falls back to
            // whole-file matching for these, which is correct rather than
            // over-broad — the finding really is about the file.
            Issue::UnusedFile { .. }
            | Issue::UnusedDep { .. }
            | Issue::LowMaintainability { .. }
            | Issue::Hotspot { .. }
            | Issue::CircularDependency { .. }
            | Issue::ParseError { .. } => None,

            // Function-scoped: the metric is computed over the whole body, so
            // the range must cover it. A `0` end line (legacy serialized form)
            // degrades to the declaration line rather than widening.
            Issue::Complexity { line, end_line, .. }
            | Issue::RefactorTarget { line, end_line, .. }
            | Issue::BannedApi { line, end_line, .. } => {
                Some((*line, (*end_line).max(*line)))
            }

            Issue::Duplicate { occurrences, .. } => {
                occurrences.first().map(|o| (o.start_line, o.end_line))
            }

            Issue::UnusedImport { line, .. }
            | Issue::Smell { line, .. }
            | Issue::FeatureFlag { line, .. } => Some((*line, *line)),

            Issue::BoundaryViolation { from_line, .. } => {
                if *from_line == 0 {
                    None
                } else {
                    Some((*from_line, *from_line))
                }
            }
        }
    }

    /// Mutable access to every path this issue references, so callers can
    /// rewrite them wholesale (e.g. absolute → repository-relative for machine
    /// output). Exhaustive by construction: a new variant that carries a path
    /// will not compile until it is listed here.
    pub fn paths_mut(&mut self) -> Vec<&mut PathBuf> {
        match self {
            Issue::UnusedFile { path }
            | Issue::UnusedImport { path, .. }
            | Issue::UnusedDep { path, .. }
            | Issue::Complexity { path, .. }
            | Issue::LowMaintainability { path, .. }
            | Issue::Hotspot { path, .. }
            | Issue::Smell { path, .. }
            | Issue::RefactorTarget { path, .. }
            | Issue::FeatureFlag { path, .. }
            | Issue::ParseError { path, .. } => vec![path],
            Issue::Duplicate { occurrences, .. } => {
                occurrences.iter_mut().map(|o| &mut o.path).collect()
            }
            Issue::CircularDependency { cycle } => cycle.iter_mut().collect(),
            Issue::BoundaryViolation {
                from_path, to_path, ..
            } => vec![from_path, to_path],
            Issue::BannedApi { path, .. } => vec![path],
        }
    }

    /// Human-readable description of this specific finding, naming the symbol,
    /// function, or file involved. Distinct from [`Issue::rule_short_description`],
    /// which describes the rule in general.
    pub fn message(&self) -> String {
        match self {
            Issue::UnusedFile { .. } => "File is not reachable from any entry point".to_string(),
            Issue::UnusedImport { name, module, .. } => {
                format!("Imported name `{name}` from `{module}` is never used")
            }
            Issue::UnusedDep { name, source, .. } => {
                format!("Dependency `{name}` declared in {source} is never imported")
            }
            Issue::Duplicate {
                token_count,
                occurrences,
            } => format!(
                "Duplicated block of {token_count} tokens repeated {} times",
                occurrences.len()
            ),
            Issue::Complexity {
                function,
                cyclomatic,
                cognitive,
                ..
            } => format!(
                "Function `{function}` is complex (cyclomatic={cyclomatic}, cognitive={cognitive})"
            ),
            Issue::LowMaintainability { score, loc, .. } => {
                format!("Maintainability index {score} over {loc} lines")
            }
            Issue::Hotspot {
                cyclomatic, churn, ..
            } => format!("Hotspot: complexity {cyclomatic} combined with {churn} recent changes"),
            Issue::Smell { rule, detail, .. } => {
                if detail.is_empty() {
                    rule.as_str().to_string()
                } else {
                    format!("{}: {detail}", rule.as_str())
                }
            }
            Issue::CircularDependency { cycle } => {
                format!("Import cycle across {} files", cycle.len())
            }
            Issue::RefactorTarget {
                function, effort, ..
            } => format!(
                "Function `{function}` is a refactor target ({} effort)",
                effort.as_str()
            ),
            Issue::FeatureFlag { flag, provider, .. } => {
                format!("Feature flag `{flag}` ({}) referenced", provider.as_str())
            }
            Issue::ParseError { message, .. } => format!("Failed to parse file: {message}"),
            Issue::BoundaryViolation {
                from_zone, to_zone, ..
            } => format!("Zone `{from_zone}` may not import from zone `{to_zone}`"),
            Issue::BannedApi { api, message, .. } => format!("Use of banned API `{api}`: {message}"),
        }
    }

    /// Stable kebab-case rule identifier used by suppressions, baselines, and JSON output.
    pub fn rule_key(&self) -> std::borrow::Cow<'static, str> {
        use std::borrow::Cow;
        Cow::Borrowed(match self {
            Issue::UnusedFile { .. } => "unused-file",
            Issue::UnusedImport { .. } => "unused-import",
            Issue::UnusedDep { .. } => "unused-dep",
            Issue::Duplicate { .. } => "duplicate",
            Issue::Complexity { .. } => "complexity",
            Issue::LowMaintainability { .. } => "low-maintainability",
            Issue::Hotspot { .. } => "hotspot",
            Issue::Smell { rule, .. } => rule.as_str(),
            Issue::CircularDependency { .. } => "circular-dependency",
            Issue::RefactorTarget { .. } => "refactor-target",
            Issue::FeatureFlag { .. } => "feature-flag",
            Issue::ParseError { .. } => "parse-error",
            Issue::BoundaryViolation { .. } => "boundary-violation",
            Issue::BannedApi { id, .. } => return Cow::Owned(id.clone()),
        })
    }

    /// Short, single-line description used by SARIF rule metadata. Compiler
    /// enforces exhaustiveness so new variants can't silently fall through.
    pub fn rule_short_description(&self) -> &'static str {
        match self {
            Issue::UnusedFile { .. } => "File is not reachable from any entry point",
            Issue::UnusedImport { .. } => "Imported name is never used in the module",
            Issue::UnusedDep { .. } => "Dependency is declared but never imported",
            Issue::Duplicate { .. } => "Repeated code block detected across the codebase",
            Issue::Complexity { .. } => {
                "Function exceeds cyclomatic or cognitive complexity threshold"
            }
            Issue::LowMaintainability { .. } => "File maintainability index falls below threshold",
            Issue::Hotspot { .. } => "File has high complexity × git churn (refactor risk)",
            Issue::CircularDependency { .. } => "Module import graph contains a cycle",
            Issue::Smell { rule, .. } => smell_short_description(*rule),
            Issue::BannedApi { .. } => "Use of an API prohibited by project policy",
            Issue::ParseError { .. } => "File could not be parsed (excluded from analysis)",
            Issue::RefactorTarget { .. } => "Refactoring candidate ranked by complexity and effort",
            Issue::FeatureFlag { .. } => "Feature flag reference (env var, settings, or SDK call)",
            Issue::BoundaryViolation { .. } => {
                "Cross-zone import violates a [[boundaries.rules]] entry"
            }
        }
    }

    /// SARIF severity level: error / warning / note.
    pub fn sarif_level(&self) -> &'static str {
        match self {
            Issue::CircularDependency { .. }
            | Issue::UnusedFile { .. }
            | Issue::LowMaintainability { .. }
            | Issue::ParseError { .. } => "error",
            Issue::UnusedImport { .. }
            | Issue::UnusedDep { .. }
            | Issue::Duplicate { .. }
            | Issue::Complexity { .. }
            | Issue::Hotspot { .. }
            | Issue::BoundaryViolation { .. } => "warning",
            Issue::RefactorTarget { .. } | Issue::FeatureFlag { .. } => "note",
            Issue::Smell { rule, .. } => smell_sarif_level(*rule),
            Issue::BannedApi { .. } => "error",
        }
    }
}

fn smell_short_description(rule: SmellRule) -> &'static str {
    use SmellRule::*;
    match rule {
        MutableDefault => "Function argument has a mutable default value",
        BroadExcept => "except: or except Exception: catches too broadly",
        SentinelEquality => "Compare against True/False/None using `is` not `==`",
        TruthyLengthCheck => "Use truthy/falsy check instead of len(x) == 0 / > 0",
        UnreachableAfterExit => "Statement after return/raise/break/continue is unreachable",
        PassthroughFunction => "Wrapper function only forwards arguments",
        StrayPrint => "print() in non-CLI module — use logging",
        SingleMethodClass => "Class has one method and no state — could be a function",
        HighTodoDensity => "File contains many TODO/FIXME markers",
        RaiseFromNone => "raise ... from None discards the original exception",
        MoneyAsFloat => "Float type used for monetary value (use Decimal)",
        BannedApi => "Use of an API prohibited by project policy",
        NoExplicitAny => "Explicit Any annotation discards type evidence",
    }
}

fn smell_sarif_level(rule: SmellRule) -> &'static str {
    use SmellRule::*;
    match rule {
        MutableDefault | RaiseFromNone | MoneyAsFloat | BannedApi | NoExplicitAny => "error",
        BroadExcept | UnreachableAfterExit => "warning",
        _ => "note",
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
    /// Stable rule keys that actually ran in this analysis, whether or not
    /// they produced findings. Consumers need this to tell "the rule ran and
    /// found nothing" from "the rule never ran" — the two look identical in
    /// the issue list. Defaulted so older serialized snapshots still load.
    #[serde(default)]
    pub executed_rules: Vec<String>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn complexity(line: u32, end_line: u32) -> Issue {
        Issue::Complexity {
            path: PathBuf::from("a.py"),
            line,
            end_line,
            function: "handler".into(),
            cyclomatic: 12,
            cognitive: 8,
        }
    }

    // A diagnostic message must identify *what* was found, not just restate
    // the rule — "unused import" is useless in a file with twelve imports.
    // A boundary violation names two files; a relativizing pass that only
    // rewrote `from_path` would leave an absolute path in machine output.
    // #1 "enabled and disabled rules compose predictably". Uses explicit
    // (rule, default_on) pairs so the default-off path is covered before any
    // shipped rule is default-off.
    #[test]
    fn default_off_rule_stays_off_until_explicitly_enabled() {
        let candidates = [(SmellRule::StrayPrint, false)];
        let active = resolve_smell_rules(candidates, &[], &[]);
        assert!(active.is_empty(), "a default-off rule must not run by default");

        let active = resolve_smell_rules(candidates, &[SmellRule::StrayPrint], &[]);
        assert!(active.contains(&SmellRule::StrayPrint), "opt-in must enable it");
    }

    #[test]
    fn explicit_disable_beats_explicit_enable() {
        let candidates = [(SmellRule::StrayPrint, false)];
        let active = resolve_smell_rules(
            candidates,
            &[SmellRule::StrayPrint],
            &[SmellRule::StrayPrint],
        );
        assert!(
            active.is_empty(),
            "turning a rule off must always win, so a shared config cannot force it on"
        );
    }

    #[test]
    fn default_on_rule_can_be_disabled() {
        let candidates = [(SmellRule::BroadExcept, true)];
        let active = resolve_smell_rules(candidates, &[], &[SmellRule::BroadExcept]);
        assert!(active.is_empty());
    }

    // Guards the "no new findings by default" criterion: every rule that ships
    // today must keep running for projects with no [smells] config at all.
    #[test]
    fn shipped_defaults_are_unchanged_without_config() {
        let active = active_smell_rules(&[], &[]);
        for rule in SmellRule::all() {
            assert_eq!(
                active.contains(rule),
                rule.default_enabled(),
                "{} must run by default iff default_enabled() says so",
                rule.as_str()
            );
        }
        // The only default-off rule is the policy family, which produces
        // nothing without [[smells.banned_api]] entries anyway.
        assert!(!active.contains(&SmellRule::BannedApi));
        assert!(!active.contains(&SmellRule::NoExplicitAny));
        assert_eq!(active.len(), SmellRule::all().len() - 2);
    }

    #[test]
    fn paths_mut_exposes_every_path_on_the_issue() {
        let mut issue = Issue::BoundaryViolation {
            from_path: PathBuf::from("/abs/a.py"),
            from_line: 3,
            from_zone: "web".into(),
            to_path: PathBuf::from("/abs/b.py"),
            to_zone: "db".into(),
        };
        for path in issue.paths_mut() {
            *path = PathBuf::from("rewritten");
        }
        match &issue {
            Issue::BoundaryViolation {
                from_path, to_path, ..
            } => {
                assert_eq!(from_path, &PathBuf::from("rewritten"));
                assert_eq!(to_path, &PathBuf::from("rewritten"));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn message_names_the_specific_symbol() {
        let issue = Issue::UnusedImport {
            path: PathBuf::from("a.py"),
            line: 3,
            name: "os".into(),
            module: "os".into(),
        };
        let message = issue.message();
        assert!(message.contains("os"), "message should name the import: {message}");
    }

    #[test]
    fn complexity_range_spans_the_whole_function_body() {
        assert_eq!(complexity(10, 25).range(), Some((10, 25)));
    }

    #[test]
    fn refactor_target_range_spans_the_whole_function_body() {
        let issue = Issue::RefactorTarget {
            path: PathBuf::from("a.py"),
            line: 4,
            end_line: 40,
            function: "handler".into(),
            cyclomatic: 20,
            cognitive: 30,
            effort: Effort::High,
        };
        assert_eq!(issue.range(), Some((4, 40)));
    }

    #[test]
    fn single_line_issue_reports_a_degenerate_range() {
        let issue = Issue::Smell {
            path: PathBuf::from("a.py"),
            line: 7,
            rule: SmellRule::StrayPrint,
            detail: String::new(),
        };
        assert_eq!(issue.range(), Some((7, 7)));
    }

    #[test]
    fn duplicate_range_covers_the_first_occurrence() {
        let issue = Issue::Duplicate {
            token_count: 60,
            occurrences: vec![DuplicateOccurrence {
                path: PathBuf::from("a.py"),
                start_line: 12,
                end_line: 30,
            }],
        };
        assert_eq!(issue.range(), Some((12, 30)));
    }

    #[test]
    fn file_level_issue_has_no_range() {
        let issue = Issue::UnusedFile {
            path: PathBuf::from("a.py"),
        };
        assert_eq!(issue.range(), None);
    }

    // Guards the #6 requirement that a localized finding whose range is
    // unavailable degrades to its own line rather than widening to the file.
    #[test]
    fn missing_end_line_degrades_to_the_declaration_line() {
        assert_eq!(complexity(10, 0).range(), Some((10, 10)));
    }
}
