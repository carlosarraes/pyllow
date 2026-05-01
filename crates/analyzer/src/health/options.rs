use pyllow_types::Effort;

#[derive(Debug, Clone, Copy)]
pub struct HealthOptions {
    pub cyclomatic_threshold: u32,
    pub cognitive_threshold: u32,
    pub maintainability_threshold: u32,
    pub min_loc_for_mi: u32,
    pub hotspot_top_n: usize,
    /// Minimum git churn (commits touching the file) before pyllow treats
    /// a file as a hotspot. Files below this floor are still subject to
    /// the complexity rule on their functions, but the hotspot signal is
    /// reserved for files that actually change repeatedly. Default 3.
    pub hotspot_min_churn: u32,
    /// When set, replace threshold-based complexity emission with the top N
    /// most complex functions ranked by `cyclomatic + cognitive`.
    pub top: Option<usize>,
    /// Emit `Issue::RefactorTarget` for functions worth refactoring,
    /// classified by [`Effort`] derived from cyclomatic/cognitive complexity.
    pub targets: bool,
    /// When set together with `targets`, only emit targets matching this effort.
    pub target_effort: Option<Effort>,
}

impl Default for HealthOptions {
    fn default() -> Self {
        Self {
            cyclomatic_threshold: 10,
            cognitive_threshold: 15,
            maintainability_threshold: 30,
            min_loc_for_mi: 50,
            hotspot_top_n: 10,
            hotspot_min_churn: 3,
            top: None,
            targets: false,
            target_effort: None,
        }
    }
}

/// Estimate refactoring effort from complexity. Below the lower band, the
/// function is too small to be a meaningful target; above the upper band,
/// it's a multi-day rewrite.
pub(super) fn classify_effort(cyclomatic: u32, cognitive: u32) -> Option<Effort> {
    let composite = cyclomatic + cognitive;
    if composite < 10 {
        None
    } else if cyclomatic <= 15 && cognitive <= 20 {
        Some(Effort::Low)
    } else if cyclomatic <= 25 && cognitive <= 40 {
        Some(Effort::Medium)
    } else {
        Some(Effort::High)
    }
}
