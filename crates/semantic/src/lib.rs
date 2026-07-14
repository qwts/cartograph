//! Eval-gated semantic hop resolver (T2, SPEC-00 §2, §13; US-0008).
//!
//! Only explicit `Gap` destinations are eligible. Candidate links first enter
//! a staging [`SemanticProposal`] list, carry Semantic/InferredStrong
//! provenance with evidence from both sides, and become an ephemeral graph
//! overlay only after a paired eval clears its precision floor. Confirmed T0/T1
//! facts are never mutation targets (R-INT-1).

pub mod context;

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier, may_overwrite};
use llm::{Embedding, LlmProvider, ProviderError};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

/// Stable semantic resolver identifier stored in proposal provenance.
pub const EXTRACTOR_ID: &str = "t2.semantic-ann";

/// Supported unresolved flow-hop families at the M7 exit gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum HopKind {
    /// Producer/consumer to a channel whose runtime identity was unresolved.
    Channel,
    /// Caller to a callee that the deterministic type/import graph could not
    /// establish.
    Call,
}

/// A graph Gap plus the incoming edge slot that a higher tier may fill.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnresolvedHop {
    /// Explicit Gap node id.
    pub gap_id: String,
    /// Existing edge source.
    pub source_id: String,
    /// Existing edge label (`PUBLISHES`, `SUBSCRIBES`, or `CALLS`).
    pub edge_label: String,
    /// Candidate family.
    pub kind: HopKind,
    /// Optional subtype, such as `sqs-queue`, used as a deterministic filter.
    pub subtype: Option<String>,
    /// Text embedded for similarity search.
    pub query: String,
    /// T0/T1 evidence that created the unresolved slot.
    pub evidence: Vec<EvidenceRef>,
}

/// Searchable graph target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    /// Existing graph node id, or stable id for a staged resource-backed
    /// Channel that is materialized only in an approved overlay.
    pub node_id: String,
    /// Candidate family.
    pub kind: HopKind,
    /// Optional deterministic subtype.
    pub subtype: Option<String>,
    /// Text embedded into the ANN index.
    pub text: String,
    /// Evidence already attached to the target node.
    pub evidence: Vec<EvidenceRef>,
    /// A target node that does not yet exist in the confirmed graph. This is
    /// used for IaC Resource candidates and never enters the stored graph.
    pub materialized_node: Option<Node>,
}

/// Staged T2 link. It is not a graph fact until an eval gate approves it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticProposal {
    /// Gap this proposal would fill.
    pub gap_id: String,
    /// Proposed edge source.
    pub source_id: String,
    /// Proposed edge destination.
    pub target_id: String,
    /// Proposed edge label.
    pub edge_label: String,
    /// Cosine similarity in `[0, 1]`.
    pub similarity: f32,
    /// Semantic/InferredStrong provenance with cited evidence.
    pub provenance: Provenance,
    /// Optional inferred target node added only to the ephemeral overlay.
    pub target_node: Option<Node>,
}

impl SemanticProposal {
    fn as_edge(&self, eval: &EvalReport) -> Edge {
        Edge {
            src: self.source_id.clone(),
            dst: self.target_id.clone(),
            label: self.edge_label.clone(),
            props: serde_json::json!({
                "resolver": EXTRACTOR_ID,
                "fills_gap": self.gap_id,
                "similarity": self.similarity,
                "eval": {
                    "precision": eval.precision,
                    "recall": eval.recall,
                    "precision_floor": eval.precision_floor,
                    "similarity_threshold": eval.similarity_threshold,
                },
                "prov": self.provenance,
            }),
        }
    }
}

/// One labeled semantic pair used for held-out evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabeledPair {
    /// Unresolved-side text.
    pub query: String,
    /// Candidate-side text.
    pub candidate: String,
    /// Ground-truth label.
    pub is_match: bool,
}

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

/// Result of applying approved proposals as an in-memory best-effort overlay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Overlay {
    /// Nodes with approved Gap nodes removed.
    pub nodes: Vec<Node>,
    /// Edges with approved Gap edges replaced by inferred edges.
    pub edges: Vec<Edge>,
    /// Number of distinct gaps filled.
    pub gaps_filled: usize,
}

/// Semantic resolver failures.
#[derive(Debug, thiserror::Error)]
pub enum SemanticError {
    /// Embedding provider failed.
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// ANN index rejected its configuration or vectors.
    #[error("ANN index: {0}")]
    Index(String),
    /// Inputs cannot support a meaningful paired eval.
    #[error("invalid eval set: {0}")]
    InvalidEval(String),
    /// Semantic provenance could not be constructed.
    #[error("provenance: {0}")]
    Provenance(#[from] core_prov::IntegrityError),
}

/// Thin verified wrapper around the USearch Rust binding.
pub struct AnnIndex {
    index: Index,
    ids: Vec<String>,
}

impl AnnIndex {
    /// Build a cosine/F32 index. IDs and embeddings must be aligned.
    pub fn build(ids: Vec<String>, embeddings: &[Embedding]) -> Result<Self, SemanticError> {
        if ids.is_empty() || embeddings.is_empty() || ids.len() != embeddings.len() {
            return Err(SemanticError::Index(
                "ids and embeddings must be non-empty and aligned".into(),
            ));
        }
        let dimensions = embeddings[0].len();
        if dimensions == 0
            || embeddings.iter().any(|embedding| {
                embedding.len() != dimensions || embedding.iter().any(|value| !value.is_finite())
            })
        {
            return Err(SemanticError::Index(
                "vectors must be finite and uniformly sized".into(),
            ));
        }
        let options = IndexOptions {
            dimensions,
            metric: MetricKind::Cos,
            quantization: ScalarKind::F32,
            ..IndexOptions::default()
        };
        let index =
            Index::new(&options).map_err(|error| SemanticError::Index(error.to_string()))?;
        index
            .reserve(ids.len())
            .map_err(|error| SemanticError::Index(error.to_string()))?;
        for (key, embedding) in embeddings.iter().enumerate() {
            index
                .add(key as u64, embedding)
                .map_err(|error| SemanticError::Index(error.to_string()))?;
        }
        Ok(Self { index, ids })
    }

    /// Return nearest IDs with cosine similarity, best first.
    pub fn search(
        &self,
        query: &Embedding,
        count: usize,
    ) -> Result<Vec<(String, f32)>, SemanticError> {
        if query.len() != self.index.dimensions() {
            return Err(SemanticError::Index(format!(
                "query dimension {} does not match index dimension {}",
                query.len(),
                self.index.dimensions()
            )));
        }
        let matches = self
            .index
            .search(query, count.min(self.ids.len()))
            .map_err(|error| SemanticError::Index(error.to_string()))?;
        let mut results: Vec<_> = matches
            .keys
            .iter()
            .zip(matches.distances.iter())
            .filter_map(|(key, distance)| {
                self.ids
                    .get(*key as usize)
                    .map(|id| (id.clone(), (1.0_f32 - *distance).clamp(0.0, 1.0)))
            })
            .collect();
        results.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        Ok(results)
    }
}

/// Recover channel/call Gap slots and searchable existing targets.
pub fn graph_inputs(nodes: &[Node], edges: &[Edge]) -> (Vec<UnresolvedHop>, Vec<Candidate>) {
    let gaps: BTreeMap<_, _> = nodes
        .iter()
        .filter(|node| node.label == "Gap")
        .map(|node| (node.id.as_str(), node))
        .collect();
    let mut hops = Vec::new();
    for edge in edges {
        let Some(gap) = gaps.get(edge.dst.as_str()) else {
            continue;
        };
        let kind = match edge.label.as_str() {
            "PUBLISHES" | "SUBSCRIBES" => HopKind::Channel,
            "CALLS" => HopKind::Call,
            _ => continue,
        };
        let query = gap.props["raw"]
            .as_str()
            .or_else(|| gap.props["callee"].as_str())
            .or_else(|| gap.props["reason"].as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let evidence = evidence_from(&gap.props);
        if query.is_empty() || evidence.is_empty() {
            continue;
        }
        hops.push(UnresolvedHop {
            gap_id: gap.id.clone(),
            source_id: edge.src.clone(),
            edge_label: edge.label.clone(),
            kind,
            subtype: (kind == HopKind::Channel)
                .then(|| gap.props["kind"].as_str().map(String::from))
                .flatten(),
            query,
            evidence,
        });
    }
    hops.sort_by(|a, b| a.gap_id.cmp(&b.gap_id));

    let mut candidates = Vec::new();
    for node in nodes {
        let (kind, subtype, text, materialized_node) = match node.label.as_str() {
            "Channel" => (
                HopKind::Channel,
                node.props["kind"].as_str().map(String::from),
                node.props["identity"].as_str().map(String::from),
                None,
            ),
            "Symbol" | "Component" => (
                HopKind::Call,
                None,
                node.props["name"]
                    .as_str()
                    .or_else(|| node.id.split('#').next_back())
                    .map(String::from),
                None,
            ),
            "Resource" => {
                let Some(channel_kind) =
                    node.props["type"].as_str().and_then(resource_channel_kind)
                else {
                    continue;
                };
                let Some(logical_id) = node.props["logical_id"].as_str() else {
                    continue;
                };
                let target_id = format!("chan:{channel_kind}:resource:{}", node.id);
                (
                    HopKind::Channel,
                    Some(channel_kind.to_string()),
                    Some(format!(
                        "{} {}",
                        logical_id.replace(['.', '_', '-'], " "),
                        node.props["type"].as_str().unwrap_or_default()
                    )),
                    Some(Node {
                        id: target_id,
                        label: "Channel".into(),
                        props: serde_json::json!({
                            "kind": channel_kind,
                            "identity": logical_id,
                            "backing_resource": node.id,
                            "staged": true,
                        }),
                    }),
                )
            }
            _ => continue,
        };
        let evidence = evidence_from(&node.props);
        if let Some(text) = text
            && !text.trim().is_empty()
            && !evidence.is_empty()
        {
            candidates.push(Candidate {
                node_id: materialized_node
                    .as_ref()
                    .map(|target| target.id.clone())
                    .unwrap_or_else(|| node.id.clone()),
                kind,
                subtype,
                text,
                evidence,
                materialized_node,
            });
        }
    }
    candidates.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    (hops, candidates)
}

fn resource_channel_kind(resource_type: &str) -> Option<&'static str> {
    match resource_type {
        "aws_sqs_queue" => Some("sqs-queue"),
        "aws_sns_topic" => Some("sns-topic"),
        _ => None,
    }
}

fn evidence_from(props: &serde_json::Value) -> Vec<EvidenceRef> {
    serde_json::from_value(props["prov"]["evidence"].clone()).unwrap_or_default()
}

/// Produce staged ANN proposals. No graph mutation occurs here.
pub fn propose(
    provider: &dyn LlmProvider,
    hops: &[UnresolvedHop],
    candidates: &[Candidate],
    neighbors: usize,
) -> Result<Vec<SemanticProposal>, SemanticError> {
    let mut grouped: BTreeMap<(HopKind, Option<String>), Vec<&Candidate>> = BTreeMap::new();
    for candidate in candidates {
        grouped
            .entry((candidate.kind, candidate.subtype.clone()))
            .or_default()
            .push(candidate);
    }
    let mut proposals = Vec::new();
    for (key, compatible) in grouped {
        let compatible_hops: Vec<&UnresolvedHop> = hops
            .iter()
            .filter(|hop| (hop.kind, hop.subtype.clone()) == key)
            .collect();
        if compatible_hops.is_empty() {
            continue;
        }
        let texts: Vec<String> = compatible.iter().map(|item| item.text.clone()).collect();
        let embeddings = provider.embed(&texts)?;
        let ids: Vec<String> = compatible.iter().map(|item| item.node_id.clone()).collect();
        let index = AnnIndex::build(ids, &embeddings)?;
        let query_texts: Vec<String> = compatible_hops
            .iter()
            .map(|hop| hop.query.clone())
            .collect();
        let query_embeddings = provider.embed(&query_texts)?;
        if query_embeddings.len() != compatible_hops.len() {
            return Err(SemanticError::Index(format!(
                "provider returned {} embeddings for {} unresolved hops",
                query_embeddings.len(),
                compatible_hops.len()
            )));
        }
        let by_id: BTreeMap<_, _> = compatible
            .iter()
            .map(|candidate| (candidate.node_id.as_str(), *candidate))
            .collect();
        for (hop, query) in compatible_hops.into_iter().zip(&query_embeddings) {
            for (target_id, similarity) in index.search(query, neighbors.max(1))? {
                let Some(candidate) = by_id.get(target_id.as_str()) else {
                    continue;
                };
                let mut evidence = hop.evidence.clone();
                evidence.extend(candidate.evidence.clone());
                evidence.sort_by(|a, b| {
                    (&a.repo, &a.path, a.byte_start, a.byte_end, &a.commit_sha).cmp(&(
                        &b.repo,
                        &b.path,
                        b.byte_start,
                        b.byte_end,
                        &b.commit_sha,
                    ))
                });
                evidence.dedup();
                let fact = format!(
                    "{}:{}:{}:{}:{similarity:.6}",
                    hop.gap_id, hop.source_id, hop.edge_label, target_id
                );
                let provenance = Provenance::new(
                    Tier::Semantic,
                    ConfidenceTier::InferredStrong,
                    evidence.clone(),
                    EXTRACTOR_ID,
                    fact.as_bytes(),
                )?;
                let target_node = candidate
                    .materialized_node
                    .as_ref()
                    .map(|template| {
                        let mut node = template.clone();
                        let node_provenance = Provenance::new(
                            Tier::Semantic,
                            ConfidenceTier::InferredStrong,
                            evidence,
                            EXTRACTOR_ID,
                            format!("materialize {}", node.id).as_bytes(),
                        )?;
                        node.props["prov"] = serde_json::to_value(node_provenance)
                            .expect("semantic provenance serializes");
                        Ok::<Node, SemanticError>(node)
                    })
                    .transpose()?;
                proposals.push(SemanticProposal {
                    gap_id: hop.gap_id.clone(),
                    source_id: hop.source_id.clone(),
                    target_id,
                    edge_label: hop.edge_label.clone(),
                    similarity,
                    provenance,
                    target_node,
                });
            }
        }
    }
    proposals.sort_by(|a, b| {
        a.gap_id
            .cmp(&b.gap_id)
            .then_with(|| {
                b.similarity
                    .partial_cmp(&a.similarity)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| a.target_id.cmp(&b.target_id))
    });
    Ok(proposals)
}

/// Measure labeled pairs and choose the highest-recall operating point that
/// clears `precision_floor` (ties prefer higher precision, then threshold).
pub fn evaluate(
    provider: &dyn LlmProvider,
    pairs: &[LabeledPair],
    precision_floor: f32,
) -> Result<EvalReport, SemanticError> {
    if !(0.0..=1.0).contains(&precision_floor) {
        return Err(SemanticError::InvalidEval(
            "precision floor must be between 0 and 1".into(),
        ));
    }
    let positives = pairs.iter().filter(|pair| pair.is_match).count();
    let negatives = pairs.len().saturating_sub(positives);
    if positives == 0 || negatives == 0 {
        return Err(SemanticError::InvalidEval(
            "paired eval requires positive and negative labels".into(),
        ));
    }
    let texts: Vec<String> = pairs
        .iter()
        .flat_map(|pair| [pair.query.clone(), pair.candidate.clone()])
        .collect();
    let embeddings = provider.embed(&texts)?;
    if embeddings.len() != texts.len() {
        return Err(SemanticError::InvalidEval(format!(
            "provider returned {} embeddings for {} eval texts",
            embeddings.len(),
            texts.len()
        )));
    }
    let mut scored = Vec::with_capacity(pairs.len());
    for (index, pair) in pairs.iter().enumerate() {
        let left = &embeddings[index * 2];
        let right = &embeddings[index * 2 + 1];
        scored.push((cosine(left, right)?, pair.is_match));
    }
    let mut thresholds: Vec<f32> = scored.iter().map(|(score, _)| *score).collect();
    thresholds.sort_by(|a, b| b.partial_cmp(a).unwrap_or(Ordering::Equal));
    thresholds.dedup_by(|a, b| (*a - *b).abs() < f32::EPSILON);

    let mut best: Option<EvalReport> = None;
    for threshold in thresholds {
        let (mut tp, mut fp, mut fn_) = (0, 0, 0);
        for (score, expected) in &scored {
            let predicted = *score >= threshold;
            match (predicted, *expected) {
                (true, true) => tp += 1,
                (true, false) => fp += 1,
                (false, true) => fn_ += 1,
                (false, false) => {}
            }
        }
        let precision = tp as f32 / (tp + fp).max(1) as f32;
        let recall = tp as f32 / (tp + fn_).max(1) as f32;
        let report = EvalReport {
            precision_floor,
            similarity_threshold: threshold,
            precision,
            recall,
            passed: tp > 0 && precision >= precision_floor,
            true_positives: tp,
            false_positives: fp,
            false_negatives: fn_,
        };
        if !report.passed {
            continue;
        }
        let replace = best.as_ref().is_none_or(|current| {
            report.recall > current.recall
                || (report.recall == current.recall && report.precision > current.precision)
                || (report.recall == current.recall
                    && report.precision == current.precision
                    && report.similarity_threshold > current.similarity_threshold)
        });
        if replace {
            best = Some(report);
        }
    }
    Ok(best.unwrap_or(EvalReport {
        precision_floor,
        similarity_threshold: 1.0,
        precision: 0.0,
        recall: 0.0,
        passed: false,
        true_positives: 0,
        false_positives: 0,
        false_negatives: positives,
    }))
}

fn cosine(left: &[f32], right: &[f32]) -> Result<f32, SemanticError> {
    if left.is_empty() || left.len() != right.len() {
        return Err(SemanticError::InvalidEval(
            "embedding dimensions must be non-empty and equal".into(),
        ));
    }
    let dot: f32 = left.iter().zip(right).map(|(a, b)| a * b).sum();
    let left_norm: f32 = left.iter().map(|value| value * value).sum::<f32>().sqrt();
    let right_norm: f32 = right.iter().map(|value| value * value).sum::<f32>().sqrt();
    if left_norm == 0.0 || right_norm == 0.0 {
        return Err(SemanticError::InvalidEval(
            "zero-length embedding norm".into(),
        ));
    }
    Ok((dot / (left_norm * right_norm)).clamp(0.0, 1.0))
}

/// Admit only proposals at the calibrated operating point after a passing
/// aggregate precision gate. The best candidate per Gap wins deterministically.
pub fn gated_proposals(proposals: &[SemanticProposal], eval: &EvalReport) -> Vec<SemanticProposal> {
    if !eval.passed || eval.precision < eval.precision_floor {
        return Vec::new();
    }
    let mut ordered = proposals.to_vec();
    ordered.sort_by(|a, b| {
        a.gap_id
            .cmp(&b.gap_id)
            .then_with(|| {
                b.similarity
                    .partial_cmp(&a.similarity)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| a.target_id.cmp(&b.target_id))
    });
    let mut seen = BTreeSet::new();
    ordered
        .into_iter()
        // Calibration uses exact cosine math while ANN proposal scores come
        // from USearch. Permit only the sub-micro rounding difference between
        // those backends; materially sub-threshold links still fail closed.
        .filter(|proposal| proposal.similarity + 1.0e-6 >= eval.similarity_threshold)
        .filter(|proposal| seen.insert(proposal.gap_id.clone()))
        .collect()
}

/// Build a best-effort overlay without mutating the confirmed graph. Only an
/// existing Gap edge is replaced; all other facts are copied unchanged.
pub fn overlay(
    nodes: &[Node],
    edges: &[Edge],
    approved: &[SemanticProposal],
    eval: &EvalReport,
) -> Overlay {
    let gap_ids: HashSet<&str> = nodes
        .iter()
        .filter(|node| node.label == "Gap")
        .map(|node| node.id.as_str())
        .collect();
    debug_assert!(may_overwrite(Tier::Semantic, ConfidenceTier::Gap));
    let approved: BTreeMap<&str, &SemanticProposal> = approved
        .iter()
        .filter(|proposal| gap_ids.contains(proposal.gap_id.as_str()))
        .filter(|proposal| {
            edges.iter().any(|edge| {
                edge.src == proposal.source_id
                    && edge.dst == proposal.gap_id
                    && edge.label == proposal.edge_label
                    && edge.props["prov"]["confidence_tier"] == "Gap"
            })
        })
        .map(|proposal| (proposal.gap_id.as_str(), proposal))
        .collect();
    let resolved: HashSet<&str> = approved.keys().copied().collect();
    let mut overlay_nodes: Vec<Node> = nodes
        .iter()
        .filter(|node| !resolved.contains(node.id.as_str()))
        .cloned()
        .collect();
    let existing_ids: HashSet<String> = overlay_nodes.iter().map(|node| node.id.clone()).collect();
    overlay_nodes.extend(
        approved
            .values()
            .filter_map(|proposal| proposal.target_node.clone())
            .filter(|node| !existing_ids.contains(&node.id)),
    );
    overlay_nodes.sort_by(|a, b| a.id.cmp(&b.id));
    overlay_nodes.dedup_by(|a, b| a.id == b.id);
    let mut overlay_edges: Vec<Edge> = edges
        .iter()
        .filter(|edge| !resolved.contains(edge.dst.as_str()))
        .cloned()
        .collect();
    overlay_edges.extend(approved.values().map(|proposal| proposal.as_edge(eval)));
    overlay_edges.sort_by(|a, b| (&a.src, &a.dst, &a.label).cmp(&(&b.src, &b.dst, &b.label)));
    Overlay {
        nodes: overlay_nodes,
        edges: overlay_edges,
        gaps_filled: approved.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm::{Locality, ProviderCaps};
    use std::time::{Duration, Instant};

    struct KeywordProvider;

    impl LlmProvider for KeywordProvider {
        fn id(&self) -> &str {
            "test-keywords"
        }

        fn locality(&self) -> Locality {
            Locality::Local
        }

        fn capabilities(&self) -> ProviderCaps {
            ProviderCaps {
                embeddings: true,
                chat: false,
                tool_use: false,
            }
        }

        fn embed(&self, batch: &[String]) -> Result<Vec<Embedding>, ProviderError> {
            Ok(batch
                .iter()
                .map(|text| {
                    let text = text.to_ascii_lowercase();
                    vec![
                        f32::from(text.contains("order")),
                        f32::from(text.contains("user")),
                        f32::from(text.contains("billing")),
                        0.01,
                    ]
                })
                .collect())
        }
    }

    fn evidence(path: &str) -> serde_json::Value {
        serde_json::json!({
            "tier": "Deterministic",
            "confidence_tier": "Confirmed",
            "evidence": [{
                "repo": "local/shop",
                "path": path,
                "byte_start": 1,
                "byte_end": 5,
                "commit_sha": "abc123"
            }],
            "extractor_id": "t0.test",
            "content_hash": "hash"
        })
    }

    fn graph() -> (Vec<Node>, Vec<Edge>) {
        let nodes = vec![
            Node {
                id: "sym:shop@send.ts#send".into(),
                label: "Symbol".into(),
                props: serde_json::json!({"name": "send", "prov": evidence("send.ts")}),
            },
            Node {
                id: "gap:chan:shop@send.ts@1".into(),
                label: "Gap".into(),
                props: serde_json::json!({
                    "kind": "sqs-queue",
                    "raw": "computed order destination",
                    "reason": "runtime-computed channel identity",
                    "prov": evidence("send.ts")
                }),
            },
            Node {
                id: "chan:sqs-queue:orders".into(),
                label: "Channel".into(),
                props: serde_json::json!({
                    "kind": "sqs-queue", "identity": "orders queue", "prov": evidence("infra.tf")
                }),
            },
            Node {
                id: "chan:sqs-queue:users".into(),
                label: "Channel".into(),
                props: serde_json::json!({
                    "kind": "sqs-queue", "identity": "users queue", "prov": evidence("infra.tf")
                }),
            },
        ];
        let edges = vec![Edge {
            src: "sym:shop@send.ts#send".into(),
            dst: "gap:chan:shop@send.ts@1".into(),
            label: "PUBLISHES".into(),
            props: serde_json::json!({"prov": {
                "confidence_tier": "Gap", "evidence": evidence("send.ts")["evidence"]
            }}),
        }];
        (nodes, edges)
    }

    fn eval_pairs() -> Vec<LabeledPair> {
        vec![
            LabeledPair {
                query: "order destination".into(),
                candidate: "orders queue".into(),
                is_match: true,
            },
            LabeledPair {
                query: "order destination".into(),
                candidate: "users queue".into(),
                is_match: false,
            },
            LabeledPair {
                query: "billing event".into(),
                candidate: "billing channel".into(),
                is_match: true,
            },
            LabeledPair {
                query: "billing event".into(),
                candidate: "users queue".into(),
                is_match: false,
            },
        ]
    }

    #[test]
    fn usearch_returns_cosine_nearest_neighbor() {
        let index = AnnIndex::build(
            vec!["orders".into(), "users".into()],
            &[vec![1.0, 0.0], vec![0.0, 1.0]],
        )
        .unwrap();
        let hits = index.search(&vec![0.99, 0.01], 2).unwrap();
        assert_eq!(hits[0].0, "orders");
        assert!(hits[0].1 > hits[1].1);
    }

    #[test]
    fn semantic_proposals_are_inferred_strong_and_cite_both_sides() {
        // AC-0021: unresolved hops become staged T2 proposals with evidence.
        let (nodes, edges) = graph();
        let (hops, candidates) = graph_inputs(&nodes, &edges);
        let proposals = propose(&KeywordProvider, &hops, &candidates, 2).unwrap();
        assert_eq!(proposals[0].target_id, "chan:sqs-queue:orders");
        assert_eq!(proposals[0].provenance.tier, Tier::Semantic);
        assert_eq!(
            proposals[0].provenance.confidence_tier,
            ConfidenceTier::InferredStrong
        );
        assert_eq!(proposals[0].provenance.evidence.len(), 2);
    }

    #[test]
    fn infra_resources_stage_channel_nodes_for_computed_gaps() {
        // AC-0021: real T0 ingestion may have only a computed channel Gap and
        // an IaC Resource. The resource is a semantic candidate, while the
        // inferred Channel exists only in the approved best-effort overlay.
        let (mut nodes, edges) = graph();
        nodes.retain(|node| node.label != "Channel");
        nodes.extend([
            Node {
                id: "res:shop@aws_sqs_queue.orders".into(),
                label: "Resource".into(),
                props: serde_json::json!({
                    "type": "aws_sqs_queue",
                    "logical_id": "aws_sqs_queue.orders",
                    "prov": evidence("infra.tf")
                }),
            },
            Node {
                id: "res:shop@aws_sqs_queue.users".into(),
                label: "Resource".into(),
                props: serde_json::json!({
                    "type": "aws_sqs_queue",
                    "logical_id": "aws_sqs_queue.users",
                    "prov": evidence("infra.tf")
                }),
            },
        ]);
        let (hops, candidates) = graph_inputs(&nodes, &edges);
        let channel_candidates: Vec<_> = candidates
            .iter()
            .filter(|candidate| candidate.kind == HopKind::Channel)
            .collect();
        assert_eq!(channel_candidates.len(), 2);
        assert!(
            channel_candidates
                .iter()
                .all(|candidate| candidate.materialized_node.is_some())
        );
        let proposals = propose(&KeywordProvider, &hops, &candidates, 2).unwrap();
        let report = evaluate(&KeywordProvider, &eval_pairs(), 0.95).unwrap();
        let approved = gated_proposals(&proposals, &report);
        let preview = overlay(&nodes, &edges, &approved, &report);
        assert_eq!(preview.gaps_filled, 1);
        let channel = preview
            .nodes
            .iter()
            .find(|node| node.label == "Channel")
            .expect("approved resource target materializes an inferred channel");
        assert_eq!(
            channel.props["backing_resource"],
            "res:shop@aws_sqs_queue.orders"
        );
        assert_eq!(channel.props["prov"]["tier"], "Semantic");
        assert_eq!(channel.props["prov"]["confidence_tier"], "InferredStrong");
        assert!(nodes.iter().all(|node| node.label != "Channel"));
    }

    #[test]
    fn semantic_resolver_handles_explicit_call_gaps() {
        let nodes = vec![
            Node {
                id: "sym:shop@caller.ts#run".into(),
                label: "Symbol".into(),
                props: serde_json::json!({"name": "run", "prov": evidence("caller.ts")}),
            },
            Node {
                id: "gap:call:shop@caller.ts@8".into(),
                label: "Gap".into(),
                props: serde_json::json!({
                    "callee": "process order",
                    "reason": "unresolved call target",
                    "prov": evidence("caller.ts")
                }),
            },
            Node {
                id: "sym:shop@orders.ts#processOrder".into(),
                label: "Symbol".into(),
                props: serde_json::json!({
                    "name": "process order", "prov": evidence("orders.ts")
                }),
            },
            Node {
                id: "sym:shop@users.ts#processUser".into(),
                label: "Symbol".into(),
                props: serde_json::json!({
                    "name": "process user", "prov": evidence("users.ts")
                }),
            },
        ];
        let edges = vec![Edge {
            src: "sym:shop@caller.ts#run".into(),
            dst: "gap:call:shop@caller.ts@8".into(),
            label: "CALLS".into(),
            props: serde_json::json!({"prov": {
                "confidence_tier": "Gap", "evidence": evidence("caller.ts")["evidence"]
            }}),
        }];
        let (hops, candidates) = graph_inputs(&nodes, &edges);
        assert_eq!(hops[0].kind, HopKind::Call);
        let proposals = propose(&KeywordProvider, &hops, &candidates, 2).unwrap();
        assert_eq!(proposals[0].target_id, "sym:shop@orders.ts#processOrder");
    }

    #[test]
    fn paired_eval_gates_best_effort_overlay() {
        // AC-0022: only a calibrated, passing precision floor can replace a
        // Gap in the best-effort overlay; confirmed graph input is untouched.
        let (nodes, edges) = graph();
        let (hops, candidates) = graph_inputs(&nodes, &edges);
        let proposals = propose(&KeywordProvider, &hops, &candidates, 2).unwrap();
        let report = evaluate(&KeywordProvider, &eval_pairs(), 0.95).unwrap();
        assert!(report.passed);
        assert_eq!(report.precision, 1.0);
        let approved = gated_proposals(&proposals, &report);
        assert_eq!(approved.len(), 1);
        let preview = overlay(&nodes, &edges, &approved, &report);
        assert_eq!(preview.gaps_filled, 1);
        assert!(preview.nodes.iter().all(|node| node.label != "Gap"));
        let edge = preview
            .edges
            .iter()
            .find(|edge| edge.label == "PUBLISHES")
            .unwrap();
        assert_eq!(edge.dst, "chan:sqs-queue:orders");
        assert_eq!(edge.props["prov"]["tier"], "Semantic");
        assert_eq!(edge.props["prov"]["confidence_tier"], "InferredStrong");
        assert!(nodes.iter().any(|node| node.label == "Gap"));
    }

    #[test]
    fn gating_tolerates_only_backend_rounding_at_the_calibrated_threshold() {
        // AC-0022: exact evaluator cosine and ANN cosine can differ by one
        // floating-point rounding step without changing the calibrated gate.
        let (nodes, edges) = graph();
        let (hops, candidates) = graph_inputs(&nodes, &edges);
        let mut proposal = propose(&KeywordProvider, &hops, &candidates, 1)
            .unwrap()
            .remove(0);
        let mut report = evaluate(&KeywordProvider, &eval_pairs(), 0.95).unwrap();
        report.similarity_threshold = 1.0;

        proposal.similarity = 0.999_999_94;
        assert_eq!(gated_proposals(&[proposal.clone()], &report).len(), 1);

        proposal.similarity = 0.999;
        assert!(gated_proposals(&[proposal], &report).is_empty());
    }

    #[test]
    fn failed_precision_gate_exports_no_semantic_links() {
        let (nodes, edges) = graph();
        let (hops, candidates) = graph_inputs(&nodes, &edges);
        let proposals = propose(&KeywordProvider, &hops, &candidates, 2).unwrap();
        let misleading = vec![
            LabeledPair {
                query: "order".into(),
                candidate: "orders".into(),
                is_match: false,
            },
            LabeledPair {
                query: "billing".into(),
                candidate: "billing".into(),
                is_match: true,
            },
        ];
        let report = evaluate(&KeywordProvider, &misleading, 1.0).unwrap();
        // A perfect threshold still exists for this tiny set only if the two
        // scores differ; here they tie, so admitting the positive admits the
        // false positive and the gate fails closed.
        assert!(!report.passed);
        assert!(gated_proposals(&proposals, &report).is_empty());
    }

    #[test]
    #[ignore = "requires local Ollama with nomic-embed-text (MT-M7-01)"]
    fn real_ollama_resolves_eval_gated_gap() {
        let provider = llm::OllamaProvider::local_default().unwrap();
        assert_eq!(provider.locality(), Locality::Local);
        let pairs = vec![
            LabeledPair {
                query: "runtime destination for an order event".into(),
                candidate: "orders queue".into(),
                is_match: true,
            },
            LabeledPair {
                query: "runtime destination for a user event".into(),
                candidate: "users queue".into(),
                is_match: true,
            },
            LabeledPair {
                query: "runtime destination for a billing event".into(),
                candidate: "billing channel".into(),
                is_match: true,
            },
            LabeledPair {
                query: "runtime destination for an order event".into(),
                candidate: "users queue".into(),
                is_match: false,
            },
            LabeledPair {
                query: "runtime destination for a billing event".into(),
                candidate: "orders queue".into(),
                is_match: false,
            },
            LabeledPair {
                query: "runtime destination for a user event".into(),
                candidate: "billing channel".into(),
                is_match: false,
            },
        ];
        let report = evaluate(&provider, &pairs, 0.9).unwrap();
        assert!(report.passed, "paired eval: {report:?}");

        let (nodes, edges) = graph();
        let (hops, candidates) = graph_inputs(&nodes, &edges);
        let proposals = propose(&provider, &hops, &candidates, 2).unwrap();
        let approved = gated_proposals(&proposals, &report);
        let preview = overlay(&nodes, &edges, &approved, &report);
        assert_eq!(preview.gaps_filled, 1, "proposals: {proposals:?}");

        let texts: Vec<String> = (0..256).map(|index| format!("candidate {index}")).collect();
        let embeddings = provider.embed(&texts).unwrap();
        let ids = (0..embeddings.len())
            .map(|index| format!("candidate-{index}"))
            .collect();
        let index = AnnIndex::build(ids, &embeddings).unwrap();
        let started = Instant::now();
        let hits = index.search(&embeddings[0], 10).unwrap();
        let elapsed = started.elapsed();
        assert!(!hits.is_empty());
        assert!(
            elapsed < Duration::from_millis(100),
            "ANN query took {elapsed:?}"
        );
        println!(
            "provider={} precision={:.3} recall={:.3} threshold={:.3} ann={elapsed:?} gaps_filled={}",
            provider.id(),
            report.precision,
            report.recall,
            report.similarity_threshold,
            preview.gaps_filled
        );
    }
}
