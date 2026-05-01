use pyllow_types::{Issue, SmellRule};
use std::path::Path;

pub(in crate::smells) fn check(source: &str, path: &Path, threshold: u32, out: &mut Vec<Issue>) {
    let mut count = 0u32;
    for line in source.lines() {
        let Some(comment) = line.split_once('#') else {
            continue;
        };
        let body = comment.1;
        for marker in &["TODO", "FIXME", "XXX", "HACK"] {
            if body.contains(marker) {
                count += 1;
                break;
            }
        }
    }
    if count >= threshold {
        out.push(Issue::Smell {
            path: path.to_path_buf(),
            line: 1,
            rule: SmellRule::HighTodoDensity,
            detail: format!("{count} TODO/FIXME markers in this file (threshold {threshold})"),
        });
    }
}
