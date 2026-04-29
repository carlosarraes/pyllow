use pyllow_extract::ast::{self, Stmt};
use pyllow_extract::{line_at_offset, ParsedModule};
use pyllow_types::{FileId, Issue};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use rustpython_parser::lexer::lex;
use rustpython_parser::Mode;
use rustpython_parser::Tok;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy)]
pub struct HealthOptions {
    pub cyclomatic_threshold: u32,
    pub cognitive_threshold: u32,
    pub maintainability_threshold: u32,
    pub min_loc_for_mi: u32,
    pub hotspot_top_n: usize,
}

impl Default for HealthOptions {
    fn default() -> Self {
        Self {
            cyclomatic_threshold: 10,
            cognitive_threshold: 15,
            maintainability_threshold: 30,
            min_loc_for_mi: 50,
            hotspot_top_n: 10,
        }
    }
}

pub fn analyze(
    parsed: &FxHashMap<FileId, ParsedModule>,
    project_root: &Path,
    opts: HealthOptions,
) -> Vec<Issue> {
    let per_file: Vec<FileHealth> = parsed
        .values()
        .par_bridge()
        .map(compute_file_health)
        .collect();

    let mut issues = Vec::new();

    for fh in &per_file {
        for f in &fh.functions {
            if f.cyclomatic > opts.cyclomatic_threshold || f.cognitive > opts.cognitive_threshold {
                issues.push(Issue::Complexity {
                    path: fh.path.clone(),
                    line: f.line,
                    function: f.name.clone(),
                    cyclomatic: f.cyclomatic,
                    cognitive: f.cognitive,
                });
            }
        }
    }

    for fh in &per_file {
        if fh.loc < opts.min_loc_for_mi {
            continue;
        }
        if let Some(mi) = fh.maintainability {
            if mi < opts.maintainability_threshold {
                issues.push(Issue::LowMaintainability {
                    path: fh.path.clone(),
                    score: mi,
                    avg_cyclomatic: fh.avg_cyclomatic(),
                    loc: fh.loc,
                });
            }
        }
    }

    let churn = compute_churn(project_root, &per_file);
    let mut hotspots: Vec<(PathBuf, u32, u32, f32)> = per_file
        .iter()
        .filter_map(|fh| {
            let cc = fh.total_cyclomatic;
            if cc == 0 {
                return None;
            }
            let c = *churn.get(fh.path.as_path()).unwrap_or(&0);
            if c == 0 {
                return None;
            }
            let score = cc as f32 * ((c as f32 + 1.0).ln());
            Some((fh.path.clone(), cc, c, score))
        })
        .collect();
    hotspots.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
    for (path, cc, c, score) in hotspots.into_iter().take(opts.hotspot_top_n) {
        issues.push(Issue::Hotspot {
            path,
            cyclomatic: cc,
            churn: c,
            score,
        });
    }

    issues.sort_by(|a, b| (a.path(), a.line().unwrap_or(0)).cmp(&(b.path(), b.line().unwrap_or(0))));
    issues
}

#[derive(Debug, Clone)]
struct FileHealth {
    path: PathBuf,
    functions: Vec<FunctionHealth>,
    total_cyclomatic: u32,
    loc: u32,
    maintainability: Option<u32>,
}

impl FileHealth {
    fn avg_cyclomatic(&self) -> f32 {
        if self.functions.is_empty() {
            1.0
        } else {
            self.total_cyclomatic as f32 / self.functions.len() as f32
        }
    }
}

#[derive(Debug, Clone)]
struct FunctionHealth {
    name: String,
    line: u32,
    cyclomatic: u32,
    cognitive: u32,
}

fn compute_file_health(module: &ParsedModule) -> FileHealth {
    let source = module.source.as_str();
    let loc = count_loc(source);

    let mut functions = Vec::new();
    for stmt in &module.suite {
        collect_functions(stmt, 0, source, &mut functions);
    }

    let total_cyclomatic: u32 = functions.iter().map(|f| f.cyclomatic).sum();
    let avg_cyclomatic = if functions.is_empty() {
        1.0
    } else {
        total_cyclomatic as f32 / functions.len() as f32
    };

    let maintainability = if loc == 0 {
        None
    } else {
        Some(maintainability_index(source, avg_cyclomatic, loc))
    };

    FileHealth {
        path: module.path.clone(),
        functions,
        total_cyclomatic,
        loc,
        maintainability,
    }
}

fn collect_functions(stmt: &Stmt, depth: u32, source: &str, out: &mut Vec<FunctionHealth>) {
    match stmt {
        Stmt::FunctionDef(f) => {
            let line = line_at_offset(source, f.range.start().to_usize());
            let mut cc = 1u32;
            let mut cog = 0u32;
            for inner in &f.body {
                accumulate_complexity(inner, 0, &mut cc, &mut cog);
            }
            out.push(FunctionHealth {
                name: f.name.as_str().to_string(),
                line,
                cyclomatic: cc,
                cognitive: cog,
            });
            for inner in &f.body {
                collect_functions(inner, depth + 1, source, out);
            }
        }
        Stmt::AsyncFunctionDef(f) => {
            let line = line_at_offset(source, f.range.start().to_usize());
            let mut cc = 1u32;
            let mut cog = 0u32;
            for inner in &f.body {
                accumulate_complexity(inner, 0, &mut cc, &mut cog);
            }
            out.push(FunctionHealth {
                name: f.name.as_str().to_string(),
                line,
                cyclomatic: cc,
                cognitive: cog,
            });
            for inner in &f.body {
                collect_functions(inner, depth + 1, source, out);
            }
        }
        Stmt::ClassDef(c) => {
            for inner in &c.body {
                collect_functions(inner, depth + 1, source, out);
            }
        }
        Stmt::If(s) => {
            for inner in &s.body {
                collect_functions(inner, depth + 1, source, out);
            }
            for inner in &s.orelse {
                collect_functions(inner, depth + 1, source, out);
            }
        }
        Stmt::While(s) => {
            for inner in &s.body {
                collect_functions(inner, depth + 1, source, out);
            }
        }
        Stmt::For(s) => {
            for inner in &s.body {
                collect_functions(inner, depth + 1, source, out);
            }
        }
        Stmt::AsyncFor(s) => {
            for inner in &s.body {
                collect_functions(inner, depth + 1, source, out);
            }
        }
        Stmt::Try(s) => {
            for inner in &s.body {
                collect_functions(inner, depth + 1, source, out);
            }
            for h in &s.handlers {
                let ast::ExceptHandler::ExceptHandler(eh) = h;
                for inner in &eh.body {
                    collect_functions(inner, depth + 1, source, out);
                }
            }
            for inner in &s.finalbody {
                collect_functions(inner, depth + 1, source, out);
            }
        }
        Stmt::With(s) => {
            for inner in &s.body {
                collect_functions(inner, depth + 1, source, out);
            }
        }
        Stmt::AsyncWith(s) => {
            for inner in &s.body {
                collect_functions(inner, depth + 1, source, out);
            }
        }
        _ => {}
    }
}

fn accumulate_complexity(stmt: &Stmt, depth: u32, cc: &mut u32, cog: &mut u32) {
    match stmt {
        Stmt::If(s) => {
            *cc += 1;
            *cog += 1 + depth;
            *cc += count_bool_ops(s.test.as_ref());
            for inner in &s.body {
                accumulate_complexity(inner, depth + 1, cc, cog);
            }
            for inner in &s.orelse {
                accumulate_complexity(inner, depth, cc, cog);
            }
        }
        Stmt::While(s) => {
            *cc += 1;
            *cog += 1 + depth;
            *cc += count_bool_ops(s.test.as_ref());
            for inner in &s.body {
                accumulate_complexity(inner, depth + 1, cc, cog);
            }
            for inner in &s.orelse {
                accumulate_complexity(inner, depth, cc, cog);
            }
        }
        Stmt::For(s) => {
            *cc += 1;
            *cog += 1 + depth;
            for inner in &s.body {
                accumulate_complexity(inner, depth + 1, cc, cog);
            }
        }
        Stmt::AsyncFor(s) => {
            *cc += 1;
            *cog += 1 + depth;
            for inner in &s.body {
                accumulate_complexity(inner, depth + 1, cc, cog);
            }
        }
        Stmt::Try(s) => {
            for inner in &s.body {
                accumulate_complexity(inner, depth + 1, cc, cog);
            }
            for h in &s.handlers {
                let ast::ExceptHandler::ExceptHandler(eh) = h;
                *cc += 1;
                *cog += 1 + depth;
                for inner in &eh.body {
                    accumulate_complexity(inner, depth + 1, cc, cog);
                }
            }
            for inner in &s.orelse {
                accumulate_complexity(inner, depth, cc, cog);
            }
            for inner in &s.finalbody {
                accumulate_complexity(inner, depth, cc, cog);
            }
        }
        Stmt::With(s) => {
            for inner in &s.body {
                accumulate_complexity(inner, depth + 1, cc, cog);
            }
        }
        Stmt::AsyncWith(s) => {
            for inner in &s.body {
                accumulate_complexity(inner, depth + 1, cc, cog);
            }
        }
        Stmt::Match(s) => {
            for case in &s.cases {
                if !is_wildcard_pattern(&case.pattern) {
                    *cc += 1;
                    *cog += 1 + depth;
                }
                for inner in &case.body {
                    accumulate_complexity(inner, depth + 1, cc, cog);
                }
            }
        }
        _ => {}
    }
}

fn count_bool_ops(expr: &ast::Expr) -> u32 {
    match expr {
        ast::Expr::BoolOp(b) => {
            let mut count = if b.values.len() > 1 {
                (b.values.len() - 1) as u32
            } else {
                0
            };
            for v in &b.values {
                count += count_bool_ops(v);
            }
            count
        }
        _ => 0,
    }
}

fn is_wildcard_pattern(p: &ast::Pattern) -> bool {
    matches!(p, ast::Pattern::MatchAs(a) if a.name.is_none() && a.pattern.is_none())
}

fn count_loc(source: &str) -> u32 {
    source
        .lines()
        .filter(|l| {
            let trimmed = l.trim();
            !trimmed.is_empty() && !trimmed.starts_with('#')
        })
        .count() as u32
}

fn maintainability_index(source: &str, avg_cc: f32, loc: u32) -> u32 {
    let (volume, _) = halstead_volume(source);
    let hv = if volume <= 1.0 { 1.0 } else { volume };
    let cc = avg_cc.max(1.0);
    let ln_loc = (loc.max(1) as f32).ln();
    let raw = 171.0 - 5.2 * hv.ln() - 0.23 * cc - 16.2 * ln_loc;
    let scaled = (raw / 171.0 * 100.0).max(0.0).min(100.0);
    scaled.round() as u32
}

fn halstead_volume(source: &str) -> (f32, usize) {
    let mut total = 0usize;
    let mut unique: FxHashSet<String> = FxHashSet::default();
    for result in lex(source, Mode::Module) {
        let Ok((tok, _)) = result else { continue };
        if matches!(tok, Tok::EndOfFile | Tok::Newline | Tok::Indent | Tok::Dedent) {
            continue;
        }
        let key = match &tok {
            Tok::Name { name } => format!("Name:{}", name.as_str()),
            Tok::Int { .. } | Tok::Float { .. } | Tok::Complex { .. } => "Num".to_string(),
            Tok::String { .. } => "Str".to_string(),
            other => format!("{:?}", other),
        };
        unique.insert(key);
        total += 1;
    }
    let vocab = unique.len();
    if total == 0 || vocab == 0 {
        return (1.0, 0);
    }
    let volume = (total as f32) * (vocab as f32).log2();
    (volume.max(1.0), total)
}

fn compute_churn(project_root: &Path, files: &[FileHealth]) -> FxHashMap<PathBuf, u32> {
    let git_root = find_git_root(project_root).unwrap_or_else(|| project_root.to_path_buf());
    let output = Command::new("git")
        .args(["log", "--name-only", "--pretty=format:"])
        .current_dir(&git_root)
        .output();
    let Ok(output) = output else {
        return FxHashMap::default();
    };
    if !output.status.success() {
        return FxHashMap::default();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut counts: FxHashMap<PathBuf, u32> = FxHashMap::default();
    let known: FxHashSet<PathBuf> = files
        .iter()
        .filter_map(|f| f.path.canonicalize().ok())
        .collect();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let abs = git_root.join(trimmed);
        if let Ok(canonical) = abs.canonicalize() {
            if known.contains(&canonical) {
                *counts.entry(canonical).or_insert(0) += 1;
            }
        }
    }
    counts
}

fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_extract::parse_source;
    use std::path::Path;

    fn module_with(src: &str) -> ParsedModule {
        parse_source(Path::new("/tmp/dummy.py"), src).unwrap()
    }

    #[test]
    fn cyclomatic_simple_function_is_one() {
        let m = module_with("def f():\n    return 1\n");
        let mut funcs = Vec::new();
        for s in &m.suite {
            collect_functions(s, 0, "def f():\n    return 1\n", &mut funcs);
        }
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].cyclomatic, 1);
    }

    #[test]
    fn cyclomatic_counts_decisions() {
        let src = "def f(x):\n    if x > 0:\n        return 1\n    elif x == 0:\n        return 0\n    else:\n        return -1\n";
        let m = module_with(src);
        let mut funcs = Vec::new();
        for s in &m.suite {
            collect_functions(s, 0, src, &mut funcs);
        }
        // 1 (base) + 1 (if) + 1 (elif=else-with-if) = 3
        assert!(funcs[0].cyclomatic >= 3);
    }

    #[test]
    fn cognitive_penalizes_nesting() {
        let src = "def f(x):\n    if x:\n        for i in range(10):\n            if i:\n                pass\n";
        let m = module_with(src);
        let mut funcs = Vec::new();
        for s in &m.suite {
            collect_functions(s, 0, src, &mut funcs);
        }
        // outer if depth 0 (+1), for depth 1 (+2), inner if depth 2 (+3) = cognitive 6
        assert!(funcs[0].cognitive >= 6);
    }

    #[test]
    fn loc_excludes_blanks_and_comments() {
        let src = "# header\n\ndef f():\n    pass\n\n# trailing comment\n";
        assert_eq!(count_loc(src), 2);
    }

    #[test]
    fn mi_clamped_in_range() {
        let mi = maintainability_index("def f(): pass\n", 1.0, 1);
        assert!(mi <= 100);
    }
}
