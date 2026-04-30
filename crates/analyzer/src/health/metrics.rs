//! Halstead volume, maintainability index, lines-of-code counters.

use rustc_hash::FxHashSet;
use rustpython_parser::lexer::lex;
use rustpython_parser::Mode;
use rustpython_parser::Tok;

pub(super) fn count_loc(source: &str) -> u32 {
    source
        .lines()
        .filter(|l| {
            let trimmed = l.trim();
            !trimmed.is_empty() && !trimmed.starts_with('#')
        })
        .count() as u32
}

pub(super) fn maintainability_index(source: &str, avg_cc: f32, loc: u32) -> u32 {
    let (volume, _) = halstead_volume(source);
    let hv = if volume <= 1.0 { 1.0 } else { volume };
    let cc = avg_cc.max(1.0);
    let ln_loc = (loc.max(1) as f32).ln();
    let raw = 171.0 - 5.2 * hv.ln() - 0.23 * cc - 16.2 * ln_loc;
    let scaled = (raw / 171.0 * 100.0).max(0.0).min(100.0);
    scaled.round() as u32
}

pub(super) fn halstead_volume(source: &str) -> (f32, usize) {
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
