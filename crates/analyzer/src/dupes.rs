use pyllow_extract::line_at_offset;
use pyllow_types::{DuplicateOccurrence, Issue};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use rustpython_parser::lexer::lex;
use rustpython_parser::Mode;
use rustpython_parser::Tok;
use std::fs;
use std::path::{Path, PathBuf};
use xxhash_rust::xxh3::Xxh3;

const DEFAULT_WINDOW: usize = 50;
const MIN_UNIQUE_TOKENS_PER_WINDOW: usize = 6;

#[derive(Debug, Clone, Copy)]
pub struct DupesOptions {
    pub window: usize,
    pub min_unique: usize,
}

impl Default for DupesOptions {
    fn default() -> Self {
        Self {
            window: DEFAULT_WINDOW,
            min_unique: MIN_UNIQUE_TOKENS_PER_WINDOW,
        }
    }
}

pub fn detect(files: &[PathBuf], opts: DupesOptions) -> Vec<Issue> {
    let tokenized: Vec<(PathBuf, Vec<(String, u32)>)> = files
        .par_iter()
        .filter_map(|path| {
            let source = fs::read_to_string(path).ok()?;
            Some((path.clone(), tokenize(&source)))
        })
        .collect();

    let mut buckets: FxHashMap<u64, Vec<(PathBuf, u32, u32)>> = FxHashMap::default();

    for (path, tokens) in &tokenized {
        if tokens.len() < opts.window {
            continue;
        }
        for i in 0..=(tokens.len() - opts.window) {
            let window = &tokens[i..i + opts.window];
            if !window_has_signal(window, opts.min_unique) {
                continue;
            }
            let mut hasher = Xxh3::new();
            for (k, _) in window {
                hasher.update(k.as_bytes());
                hasher.update(b"|");
            }
            let h = hasher.digest();
            let start_line = window[0].1;
            let end_line = window[opts.window - 1].1;
            buckets
                .entry(h)
                .or_default()
                .push((path.clone(), start_line, end_line));
        }
    }

    // Per pair-of-files, collapse all overlapping windows into the first occurrence.
    // A pair is identified by sorted (path_a, path_b). Within each pair, only the
    // earliest start line per file is emitted, so a 100-line clone produces one issue
    // instead of one per sliding window.
    let mut pair_first: FxHashMap<(PathBuf, PathBuf), Vec<DuplicateOccurrence>> =
        FxHashMap::default();

    for (_, occurrences) in buckets {
        let distinct_files: FxHashSet<&PathBuf> = occurrences.iter().map(|(p, _, _)| p).collect();
        if distinct_files.len() < 2 {
            continue;
        }
        let mut by_path: FxHashMap<PathBuf, (u32, u32)> = FxHashMap::default();
        for (path, start, end) in &occurrences {
            by_path
                .entry(path.clone())
                .and_modify(|existing| {
                    if *start < existing.0 {
                        *existing = (*start, *end);
                    }
                })
                .or_insert((*start, *end));
        }
        let mut paths: Vec<&PathBuf> = by_path.keys().collect();
        paths.sort();
        for i in 0..paths.len() {
            for j in (i + 1)..paths.len() {
                let key = (paths[i].clone(), paths[j].clone());
                pair_first.entry(key).or_insert_with(|| {
                    vec![
                        DuplicateOccurrence {
                            path: paths[i].clone(),
                            start_line: by_path[paths[i]].0,
                            end_line: by_path[paths[i]].1,
                        },
                        DuplicateOccurrence {
                            path: paths[j].clone(),
                            start_line: by_path[paths[j]].0,
                            end_line: by_path[paths[j]].1,
                        },
                    ]
                });
            }
        }
    }

    let mut issues: Vec<Issue> = pair_first
        .into_values()
        .map(|occurrences| Issue::Duplicate {
            token_count: opts.window as u32,
            occurrences,
        })
        .collect();
    issues.sort_by(|a, b| a.path().cmp(b.path()));
    issues
}

fn tokenize(source: &str) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    for result in lex(source, Mode::Module) {
        let Ok((tok, range)) = result else { continue };
        if matches!(tok, Tok::EndOfFile) {
            continue;
        }
        let line = line_at_offset(source, range.start().to_usize());
        out.push((tok_key(&tok), line));
    }
    out
}

fn tok_key(t: &Tok) -> String {
    match t {
        Tok::Name { name } => format!("Name:{}", name.as_str()),
        Tok::Int { value } => format!("Int:{value}"),
        Tok::Float { value } => format!("Float:{value}"),
        Tok::Complex { real, imag } => format!("Complex:{real}+{imag}j"),
        Tok::String { value, .. } => format!("Str:{value}"),
        other => format!("{:?}", other),
    }
}

fn window_has_signal(window: &[(String, u32)], min_unique: usize) -> bool {
    let unique: FxHashSet<&str> = window.iter().map(|(k, _)| k.as_str()).collect();
    unique.len() >= min_unique
}

pub fn run(project_root: &Path, opts: DupesOptions) -> Vec<Issue> {
    let mut files = Vec::new();
    for entry in ignore::WalkBuilder::new(project_root)
        .git_ignore(true)
        .build()
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("py") {
            files.push(path.to_path_buf());
        }
    }
    detect(&files, opts)
}

pub fn run_with_files(files: &[PathBuf], opts: DupesOptions) -> Vec<Issue> {
    detect(files, opts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn detects_clone_across_two_files() {
        let dir = tempdir().unwrap();
        let snippet = "def foo(x, y):\n    if x > y:\n        result = x + y\n    elif x == y:\n        result = x * 2\n    else:\n        result = y - x\n    return result\n\nprint(foo(1, 2))\n";
        fs::write(dir.path().join("a.py"), snippet).unwrap();
        fs::write(dir.path().join("b.py"), snippet).unwrap();

        let issues = run(
            dir.path(),
            DupesOptions {
                window: 30,
                min_unique: 4,
            },
        );
        assert!(!issues.is_empty(), "expected at least one duplicate group");
    }

    #[test]
    fn ignores_low_signal_windows() {
        let dir = tempdir().unwrap();
        let imports = "import a\nimport b\nimport c\nimport d\nimport e\nimport f\nimport g\nimport h\n";
        fs::write(dir.path().join("a.py"), imports).unwrap();
        fs::write(dir.path().join("b.py"), imports).unwrap();
        let issues = run(
            dir.path(),
            DupesOptions {
                window: 10,
                min_unique: 8,
            },
        );
        assert!(
            issues.is_empty(),
            "import-only files should not register as duplicates"
        );
    }

    #[test]
    fn unrelated_files_no_dupes() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.py"), "def alpha(x):\n    return x + 1\n").unwrap();
        fs::write(
            dir.path().join("b.py"),
            "class Beta:\n    def method(self):\n        return self.x * 2\n",
        )
        .unwrap();
        let issues = run(dir.path(), DupesOptions::default());
        assert!(issues.is_empty());
    }
}
