use pyllow_types::Issue;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthScore {
    pub value: u8,
    pub grade: char,
}

impl HealthScore {
    pub fn label(&self) -> &'static str {
        match self.grade {
            'A' => "excellent",
            'B' => "good",
            'C' => "fair",
            'D' => "poor",
            _ => "critical",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScoreBreakdown {
    pub total_issues: usize,
    pub unused_files: usize,
    pub unused_imports: usize,
    pub unused_deps: usize,
    pub duplicates: usize,
    pub complexity: usize,
    pub low_maintainability: usize,
    pub hotspots: usize,
    #[serde(default)]
    pub smells: usize,
    #[serde(default)]
    pub circular_deps: usize,
    #[serde(default)]
    pub refactor_targets: usize,
    #[serde(default)]
    pub feature_flags: usize,
    #[serde(default)]
    pub parse_errors: usize,
    pub deduction: f32,
    pub raw_score: f32,
}

impl ScoreBreakdown {
    pub fn from_issues(issues: &[Issue]) -> Self {
        let mut b = Self::default();
        for issue in issues {
            b.total_issues += 1;
            match issue {
                Issue::UnusedFile { .. } => {
                    b.unused_files += 1;
                    b.deduction += 2.0;
                }
                Issue::UnusedImport { .. } => {
                    b.unused_imports += 1;
                    b.deduction += 0.1;
                }
                Issue::UnusedDep { .. } => {
                    b.unused_deps += 1;
                    b.deduction += 1.0;
                }
                Issue::Duplicate { .. } => {
                    b.duplicates += 1;
                    b.deduction += 1.0;
                }
                Issue::Complexity {
                    cyclomatic,
                    cognitive,
                    ..
                } => {
                    b.complexity += 1;
                    let cc_excess = (*cyclomatic).saturating_sub(10) as f32;
                    let cog_excess = (*cognitive).saturating_sub(15) as f32;
                    b.deduction += 0.5 * cc_excess + 0.3 * cog_excess;
                }
                Issue::LowMaintainability { score, .. } => {
                    b.low_maintainability += 1;
                    let mi_gap = (50u32).saturating_sub(*score) as f32;
                    b.deduction += 2.0 + mi_gap * 0.05;
                }
                Issue::Hotspot { score, .. } => {
                    b.hotspots += 1;
                    b.deduction += (score / 100.0).clamp(0.5, 5.0);
                }
                Issue::Smell { rule, .. } => {
                    b.smells += 1;
                    // Per-rule weight: high-confidence anti-patterns deduct more.
                    use pyllow_types::SmellRule::*;
                    b.deduction += match rule {
                        // Highest weight: financial-correctness rule whose
                        // failures are legal/reputational risk.
                        MoneyAsFloat => 2.0,
                        MutableDefault | RaiseFromNone => 1.5,
                        BroadExcept | UnreachableAfterExit => 1.0,
                        SingleMethodClass | PassthroughFunction | StrayPrint => 0.5,
                        SentinelEquality | TruthyLengthCheck | HighTodoDensity => 0.3,
                    };
                }
                Issue::CircularDependency { cycle } => {
                    b.circular_deps += 1;
                    // Larger cycles are worse; clamp so a 50-file cycle doesn't tank the score.
                    b.deduction += (cycle.len() as f32).clamp(2.0, 10.0);
                }
                Issue::RefactorTarget { .. } => {
                    // Targets are advisory — they surface candidates, not failures.
                    // No score deduction.
                    b.refactor_targets += 1;
                }
                Issue::FeatureFlag { .. } => {
                    // Inventory only; flags become deductions only when
                    // cross-referenced with dead code (stale flags).
                    b.feature_flags += 1;
                }
                Issue::ParseError { .. } => {
                    // Heavy weight: an unparseable file silently disappears
                    // from every other check, so a single parse error
                    // poisons the whole report's reliability.
                    b.parse_errors += 1;
                    b.deduction += 5.0;
                }
            }
        }
        b.raw_score = (100.0 - b.deduction).clamp(0.0, 100.0);
        b
    }
}

pub fn compute(issues: &[Issue]) -> HealthScore {
    let breakdown = ScoreBreakdown::from_issues(issues);
    score_from_value(breakdown.raw_score)
}

fn score_from_value(value: f32) -> HealthScore {
    let v = value.round() as i32;
    let v = v.clamp(0, 100) as u8;
    let grade = match v {
        90..=100 => 'A',
        80..=89 => 'B',
        70..=79 => 'C',
        60..=69 => 'D',
        _ => 'F',
    };
    HealthScore { value: v, grade }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn empty_codebase_is_perfect() {
        let s = compute(&[]);
        assert_eq!(s.value, 100);
        assert_eq!(s.grade, 'A');
    }

    #[test]
    fn many_unused_files_drop_score() {
        let issues: Vec<Issue> = (0..30)
            .map(|i| Issue::UnusedFile {
                path: PathBuf::from(format!("/x/orphan_{i}.py")),
            })
            .collect();
        let s = compute(&issues);
        // 30 * 2.0 = 60 deduction → score 40 → F
        assert_eq!(s.value, 40);
        assert_eq!(s.grade, 'F');
    }

    #[test]
    fn complexity_deduction_scales_with_cc() {
        let issues = vec![Issue::Complexity {
            path: PathBuf::from("/x/big.py"),
            line: 1,
            function: "huge".into(),
            cyclomatic: 50,
            cognitive: 80,
        }];
        let s = compute(&issues);
        // (50-10)*0.5 + (80-15)*0.3 = 20 + 19.5 = 39.5 → score 61 → D
        assert_eq!(s.grade, 'D');
    }

    #[test]
    fn grade_boundaries() {
        assert_eq!(score_from_value(95.0).grade, 'A');
        assert_eq!(score_from_value(85.0).grade, 'B');
        assert_eq!(score_from_value(75.0).grade, 'C');
        assert_eq!(score_from_value(65.0).grade, 'D');
        assert_eq!(score_from_value(50.0).grade, 'F');
    }
}
