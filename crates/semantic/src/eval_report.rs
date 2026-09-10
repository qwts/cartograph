use serde::{Deserialize, Serialize};

/// Calibrated paired-eval result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalReport {
    /// Requested minimum precision.
    pub precision_floor: f32,
    /// Lowest calibrated similarity admitted by the selected operating point.
    pub similarity_threshold: f32,
    /// Precision at that threshold.
    pub precision: f32,
    /// Recall at that threshold.
    pub recall: f32,
    /// True only when the floor is met with at least one true positive.
    pub passed: bool,
    /// True positives.
    pub true_positives: usize,
    /// False positives.
    pub false_positives: usize,
    /// False negatives.
    pub false_negatives: usize,
}
