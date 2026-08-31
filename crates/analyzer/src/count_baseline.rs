//! Strict per-rule count baselines (issue #7) — the downward ratchet.
//!
//! Distinct from fingerprint baselines (`baseline.rs`): a fingerprint baseline
//! *hides* known findings; a count baseline *fails* when the recorded
//! allowance no longer matches reality in either direction, forcing cleanup
//! programs to bank their progress.

use pyllow_types::Issue;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use thiserror::Error;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum CountBaselineError {
    #[error("io error reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid count baseline in {path}: {detail}")]
    Invalid { path: String, detail: String },
}

/// One per-rule outcome of comparing current counts against the baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// current > baseline: new findings slipped in.
    Regression { rule: String, current: u64, baseline: u64 },
    /// current < baseline: debt was paid down but the allowance was not
    /// lowered. Fails so the exact lower value gets committed.
    Stale { rule: String, current: u64, baseline: u64 },
}

/// On-disk shape. `deny_unknown_fields` + `u64` counts give the schema
/// rejections (#7): unknown fields, booleans, floats, and negative counts all
/// fail deserialization; a missing `counts` map fails on the required field.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FileShape {
    schema_version: u32,
    counts: BTreeMap<String, u64>,
}

#[derive(Debug)]
pub struct CountBaseline {
    counts: BTreeMap<String, u64>,
}

impl CountBaseline {
    pub fn counts(&self) -> &BTreeMap<String, u64> {
        &self.counts
    }
}

/// Count final reported issues per stable rule key.
pub fn count_by_rule(issues: &[Issue]) -> BTreeMap<String, u64> {
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for issue in issues {
        *counts.entry(issue.rule_key().into_owned()).or_default() += 1;
    }
    counts
}

pub fn load_str(raw: &str, path: &str) -> Result<CountBaseline, CountBaselineError> {
    let shape: FileShape = serde_json::from_str(raw).map_err(|e| CountBaselineError::Invalid {
        path: path.to_string(),
        detail: e.to_string(),
    })?;
    if shape.schema_version != SCHEMA_VERSION {
        return Err(CountBaselineError::Invalid {
            path: path.to_string(),
            detail: format!(
                "unsupported schemaVersion {} (expected {SCHEMA_VERSION})",
                shape.schema_version
            ),
        });
    }
    Ok(CountBaseline {
        counts: shape.counts,
    })
}

pub fn load(path: &Path) -> Result<CountBaseline, CountBaselineError> {
    let raw = std::fs::read_to_string(path).map_err(|source| CountBaselineError::Io {
        path: path.display().to_string(),
        source,
    })?;
    load_str(&raw, &path.display().to_string())
}

/// Write the exact current counts — the explicit update the ratchet demands.
pub fn save(path: &Path, issues: &[Issue]) -> Result<(), CountBaselineError> {
    let shape = FileShape {
        schema_version: SCHEMA_VERSION,
        counts: count_by_rule(issues),
    };
    let body = serde_json::to_string_pretty(&shape).expect("count baseline serializes");
    std::fs::write(path, body + "\n").map_err(|source| CountBaselineError::Io {
        path: path.display().to_string(),
        source,
    })
}

/// The four-outcome ratchet. Equal counts pass (produce nothing); every
/// deviation is an [`Outcome`]. A rule absent from one side counts as zero on
/// that side, so new rules regress against allowance 0 and fully-paid debt
/// shows as stale until the allowance is removed.
pub fn compare(current: &BTreeMap<String, u64>, baseline: &CountBaseline) -> Vec<Outcome> {
    let mut rules: Vec<&String> = current.keys().chain(baseline.counts.keys()).collect();
    rules.sort();
    rules.dedup();
    rules
        .into_iter()
        .filter_map(|rule| {
            let cur = current.get(rule).copied().unwrap_or(0);
            let base = baseline.counts.get(rule).copied().unwrap_or(0);
            match cur.cmp(&base) {
                std::cmp::Ordering::Greater => Some(Outcome::Regression {
                    rule: rule.clone(),
                    current: cur,
                    baseline: base,
                }),
                std::cmp::Ordering::Less => Some(Outcome::Stale {
                    rule: rule.clone(),
                    current: cur,
                    baseline: base,
                }),
                std::cmp::Ordering::Equal => None,
            }
        })
        .collect()
}

/// The branch-vs-merge-base ratchet: a branch may lower a committed
/// allowance, never raise it. `merge_base` is the counts file as committed at
/// the merge-base; `None` (file absent there) allows adoption.
pub fn ratchet_violations(
    branch: &CountBaseline,
    merge_base: Option<&CountBaseline>,
) -> Vec<Outcome> {
    let Some(mb) = merge_base else {
        return Vec::new();
    };
    branch
        .counts
        .iter()
        .filter_map(|(rule, value)| {
            let allowed = mb.counts.get(rule).copied().unwrap_or(0);
            (*value > allowed).then(|| Outcome::Regression {
                rule: rule.clone(),
                current: *value,
                baseline: allowed,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyllow_types::SmellRule;
    use std::path::PathBuf;

    fn smell(rule: SmellRule) -> Issue {
        Issue::Smell {
            path: PathBuf::from("a.py"),
            line: 1,
            rule,
            detail: String::new(),
        }
    }

    fn counts(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    fn baseline(pairs: &[(&str, u64)]) -> CountBaseline {
        let body = serde_json::json!({
            "schemaVersion": 1,
            "counts": pairs.iter().map(|(k, v)| (k.to_string(), v)).collect::<BTreeMap<_, _>>(),
        });
        load_str(&body.to_string(), "test.json").unwrap()
    }

    // ---- schema ----

    #[test]
    fn well_formed_file_loads() {
        let b = baseline(&[("broad-except", 40)]);
        assert_eq!(b.counts()["broad-except"], 40);
    }

    #[test]
    fn malformed_json_is_rejected() {
        assert!(load_str("{not json", "x.json").is_err());
    }

    #[test]
    fn missing_counts_field_is_rejected() {
        assert!(load_str(r#"{"schemaVersion": 1}"#, "x.json").is_err());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let raw = r#"{"schemaVersion": 1, "counts": {}, "extra": true}"#;
        assert!(load_str(raw, "x.json").is_err());
    }

    #[test]
    fn boolean_count_is_rejected() {
        let raw = r#"{"schemaVersion": 1, "counts": {"broad-except": true}}"#;
        assert!(load_str(raw, "x.json").is_err());
    }

    #[test]
    fn negative_count_is_rejected() {
        let raw = r#"{"schemaVersion": 1, "counts": {"broad-except": -1}}"#;
        assert!(load_str(raw, "x.json").is_err());
    }

    #[test]
    fn unsupported_schema_version_is_rejected() {
        let raw = r#"{"schemaVersion": 99, "counts": {}}"#;
        let err = load_str(raw, "x.json").unwrap_err();
        assert!(err.to_string().contains("99"), "{err}");
    }

    // ---- compare ----

    #[test]
    fn equal_counts_pass() {
        let out = compare(&counts(&[("broad-except", 3)]), &baseline(&[("broad-except", 3)]));
        assert!(out.is_empty());
    }

    #[test]
    fn increase_is_a_regression() {
        let out = compare(&counts(&[("broad-except", 4)]), &baseline(&[("broad-except", 3)]));
        assert_eq!(
            out,
            vec![Outcome::Regression { rule: "broad-except".into(), current: 4, baseline: 3 }]
        );
    }

    #[test]
    fn decrease_is_stale_and_carries_the_exact_lower_value() {
        let out = compare(&counts(&[("broad-except", 1)]), &baseline(&[("broad-except", 3)]));
        assert_eq!(
            out,
            vec![Outcome::Stale { rule: "broad-except".into(), current: 1, baseline: 3 }]
        );
    }

    #[test]
    fn rule_absent_from_baseline_has_allowance_zero() {
        let out = compare(&counts(&[("stray-print", 2)]), &baseline(&[]));
        assert_eq!(
            out,
            vec![Outcome::Regression { rule: "stray-print".into(), current: 2, baseline: 0 }]
        );
    }

    #[test]
    fn baselined_rule_now_at_zero_is_stale() {
        // The debt is fully paid; the allowance must be deleted or zeroed.
        let out = compare(&counts(&[]), &baseline(&[("broad-except", 3)]));
        assert_eq!(
            out,
            vec![Outcome::Stale { rule: "broad-except".into(), current: 0, baseline: 3 }]
        );
    }

    #[test]
    fn zero_allowance_with_zero_findings_passes() {
        let out = compare(&counts(&[]), &baseline(&[("broad-except", 0)]));
        assert!(out.is_empty());
    }

    // ---- counting ----

    #[test]
    fn counts_group_by_rule_key() {
        let issues = vec![
            smell(SmellRule::BroadExcept),
            smell(SmellRule::BroadExcept),
            smell(SmellRule::StrayPrint),
        ];
        assert_eq!(
            count_by_rule(&issues),
            counts(&[("broad-except", 2), ("stray-print", 1)])
        );
    }

    // ---- merge-base ratchet ----

    #[test]
    fn branch_raising_a_committed_allowance_fails() {
        let out = ratchet_violations(&baseline(&[("broad-except", 45)]), Some(&baseline(&[("broad-except", 40)])));
        assert_eq!(
            out,
            vec![Outcome::Regression { rule: "broad-except".into(), current: 45, baseline: 40 }]
        );
    }

    #[test]
    fn branch_lowering_or_keeping_allowances_passes() {
        let mb = baseline(&[("broad-except", 40), ("stray-print", 2)]);
        let out = ratchet_violations(&baseline(&[("broad-except", 30), ("stray-print", 2)]), Some(&mb));
        assert!(out.is_empty());
    }

    #[test]
    fn allowance_for_a_rule_unknown_at_merge_base_fails() {
        // A brand-new allowance is an attempted increase from zero.
        let out = ratchet_violations(&baseline(&[("new-rule", 5)]), Some(&baseline(&[])));
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn no_baseline_at_merge_base_allows_adoption() {
        assert!(ratchet_violations(&baseline(&[("broad-except", 40)]), None).is_empty());
    }

    // ---- round trip ----

    #[test]
    fn save_then_load_round_trips_exact_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("counts.json");
        let issues = vec![smell(SmellRule::BroadExcept), smell(SmellRule::BroadExcept)];
        save(&path, &issues).unwrap();
        let b = load(&path).unwrap();
        assert_eq!(b.counts()["broad-except"], 2);
        assert_eq!(b.counts().len(), 1);
    }
}
