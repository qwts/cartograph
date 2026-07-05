//! Provenance and confidence model — the integrity spine (SPEC-00 §2, §4.3).
//!
//! Every node/edge in the graph carries a [`Provenance`]: which tier produced
//! it, its confidence tier, the evidence spans behind it, and a content hash
//! that makes re-ingest idempotent (US-0007, US-0014).

use serde::{Deserialize, Serialize};

/// Producing tier of a fact — the escalation ladder, lowest wins (SPEC-00 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Tier {
    /// T0 — static parse, IaC graph, framework adapters.
    Deterministic,
    /// T1 — execution-derived evidence (state exports, traces, test runs).
    Dynamic,
    /// T2 — embeddings/similarity/name-contract matching.
    Semantic,
    /// T3 — bounded LLM agent proposals with cited evidence.
    Agentic,
}

/// Confidence tier carried by every fact (SPEC-00 §13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfidenceTier {
    /// Established deterministically or by observation (T0/T1).
    Confirmed,
    /// Inferred by the semantic tier (T2).
    InferredStrong,
    /// Proposed by the agentic tier (T3).
    InferredWeak,
    /// Explicitly unresolved — preferred over an unsupported assertion.
    Gap,
}

impl Tier {
    /// The highest confidence a producer at this tier may assert (SPEC-00 §2).
    pub fn confidence_ceiling(self) -> ConfidenceTier {
        match self {
            Tier::Deterministic | Tier::Dynamic => ConfidenceTier::Confirmed,
            Tier::Semantic => ConfidenceTier::InferredStrong,
            Tier::Agentic => ConfidenceTier::InferredWeak,
        }
    }
}

/// R-INT-1: may `producer` write over a fact currently held at `existing`?
///
/// T2/T3 never overwrite or upgrade a T0/T1 (`Confirmed`) fact — they only
/// fill unresolved slots (`Gap`) or revise their own tier's output.
pub fn may_overwrite(producer: Tier, existing: ConfidenceTier) -> bool {
    match producer {
        Tier::Deterministic | Tier::Dynamic => true,
        Tier::Semantic => matches!(
            existing,
            ConfidenceTier::InferredStrong | ConfidenceTier::InferredWeak | ConfidenceTier::Gap
        ),
        Tier::Agentic => matches!(existing, ConfidenceTier::InferredWeak | ConfidenceTier::Gap),
    }
}

/// A source span backing a fact (SPEC-00 §4.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    /// Repository the evidence lives in (e.g. `owner/name`).
    pub repo: String,
    /// Path within the repository.
    pub path: String,
    /// Byte span start (inclusive).
    pub byte_start: u64,
    /// Byte span end (exclusive).
    pub byte_end: u64,
    /// Commit the span was read at.
    pub commit_sha: String,
}

/// Provenance attached to every node and edge (SPEC-00 §4.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// Producing tier.
    pub tier: Tier,
    /// Confidence tier (never above `tier.confidence_ceiling()`).
    pub confidence_tier: ConfidenceTier,
    /// Evidence spans (0..n).
    pub evidence: Vec<EvidenceRef>,
    /// Which adapter/resolver produced the fact.
    pub extractor_id: String,
    /// Content-addressed hash for idempotent re-ingest. Derived only from the
    /// fact's canonical bytes — never timestamps (US-0014).
    pub content_hash: String,
}

impl Provenance {
    /// Build provenance for a fact, deriving the content hash from the fact's
    /// canonical byte representation. Enforces the tier's confidence ceiling.
    pub fn new(
        tier: Tier,
        confidence_tier: ConfidenceTier,
        evidence: Vec<EvidenceRef>,
        extractor_id: impl Into<String>,
        fact_bytes: &[u8],
    ) -> Result<Self, IntegrityError> {
        if confidence_above_ceiling(tier, confidence_tier) {
            return Err(IntegrityError::AboveCeiling {
                tier,
                confidence_tier,
            });
        }
        Ok(Self {
            tier,
            confidence_tier,
            evidence,
            extractor_id: extractor_id.into(),
            content_hash: content_hash(fact_bytes),
        })
    }
}

fn confidence_above_ceiling(tier: Tier, confidence: ConfidenceTier) -> bool {
    fn rank(c: ConfidenceTier) -> u8 {
        match c {
            ConfidenceTier::Confirmed => 3,
            ConfidenceTier::InferredStrong => 2,
            ConfidenceTier::InferredWeak => 1,
            ConfidenceTier::Gap => 0,
        }
    }
    rank(confidence) > rank(tier.confidence_ceiling())
}

/// Violations of the integrity rules (R-INT-1..5).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IntegrityError {
    /// A producer asserted a confidence above its tier's ceiling.
    #[error("{tier:?} may not assert {confidence_tier:?} (above ceiling)")]
    AboveCeiling {
        /// Producing tier.
        tier: Tier,
        /// Asserted confidence.
        confidence_tier: ConfidenceTier,
    },
}

/// Content-address a byte sequence (BLAKE3, hex). Deterministic by
/// construction — the foundation of idempotent re-ingest (AC-0018, AC-0039).
pub fn content_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_deterministic_and_content_sensitive() {
        assert_eq!(content_hash(b"same input"), content_hash(b"same input"));
        assert_ne!(content_hash(b"same input"), content_hash(b"same input!"));
    }

    #[test]
    fn confidence_ceilings_match_spec() {
        assert_eq!(
            Tier::Deterministic.confidence_ceiling(),
            ConfidenceTier::Confirmed
        );
        assert_eq!(
            Tier::Dynamic.confidence_ceiling(),
            ConfidenceTier::Confirmed
        );
        assert_eq!(
            Tier::Semantic.confidence_ceiling(),
            ConfidenceTier::InferredStrong
        );
        assert_eq!(
            Tier::Agentic.confidence_ceiling(),
            ConfidenceTier::InferredWeak
        );
    }

    #[test]
    fn r_int_1_t2_t3_never_touch_confirmed() {
        // AC-0019: T2/T3 cannot overwrite or upgrade a T0/T1 fact.
        assert!(!may_overwrite(Tier::Semantic, ConfidenceTier::Confirmed));
        assert!(!may_overwrite(Tier::Agentic, ConfidenceTier::Confirmed));
        // Agents also cannot touch the semantic tier's output.
        assert!(!may_overwrite(
            Tier::Agentic,
            ConfidenceTier::InferredStrong
        ));
        // Everyone may fill an explicit Gap.
        for tier in [
            Tier::Deterministic,
            Tier::Dynamic,
            Tier::Semantic,
            Tier::Agentic,
        ] {
            assert!(may_overwrite(tier, ConfidenceTier::Gap));
        }
        // T0/T1 may replace anything.
        assert!(may_overwrite(
            Tier::Deterministic,
            ConfidenceTier::Confirmed
        ));
        assert!(may_overwrite(Tier::Dynamic, ConfidenceTier::InferredStrong));
    }

    #[test]
    fn provenance_rejects_confidence_above_ceiling() {
        // An agent asserting Confirmed is an integrity violation, not a value.
        let err = Provenance::new(
            Tier::Agentic,
            ConfidenceTier::Confirmed,
            vec![],
            "t3.test",
            b"x",
        )
        .unwrap_err();
        assert!(matches!(err, IntegrityError::AboveCeiling { .. }));
    }

    #[test]
    fn provenance_serde_round_trips() {
        let p = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![EvidenceRef {
                repo: "qwtm/example".into(),
                path: "src/index.ts".into(),
                byte_start: 10,
                byte_end: 42,
                commit_sha: "abc123".into(),
            }],
            "t0.ts-adapter",
            b"endpoint GET /health",
        )
        .unwrap();
        let json = serde_json::to_string(&p).unwrap();
        let back: Provenance = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }
}
