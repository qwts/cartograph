//! Pure deterministic request planning; no source or store access.

use agents::AgentCandidate;
use core_graph::source::{FactKey, edge_digest, node_digest};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef};
use std::collections::BTreeSet;

pub(super) const MAX_REQUESTS: usize = 64;
pub(super) const MAX_CANDIDATES: usize = 8;

pub(super) struct EvidenceRequest {
    pub fact: FactKey,
    pub fact_digest: String,
    pub reference: Option<EvidenceRef>,
    pub candidate: Option<AgentCandidate>,
}

pub(super) struct TaskPlan {
    pub action_id: String,
    pub gap_id: String,
    pub source_id: String,
    pub edge_label: String,
    pub requests: Vec<EvidenceRequest>,
    pub slot: Option<(FactKey, String)>,
    pub unplanned_candidates: usize,
}

fn request(node: &Node, candidate: bool) -> Result<EvidenceRequest, String> {
    let provenance = spec::provenance(&node.props, &node.id);
    let name = node.props["name"]
        .as_str()
        .or_else(|| node.props["path"].as_str())
        .unwrap_or(&node.id);
    Ok(EvidenceRequest {
        fact: FactKey::from_node(node),
        fact_digest: node_digest(node).map_err(|_| "Task fact is invalid.")?,
        reference: provenance.evidence.first().cloned(),
        candidate: candidate.then(|| AgentCandidate {
            node_id: node.id.clone(),
            label: node.label.clone(),
            summary: format!("{}: {name}", node.label),
            evidence_ids: Vec::new(),
        }),
    })
}

pub(super) fn plan(
    nodes: &[Node],
    edges: &[Edge],
    gap_id: &str,
    action_id: &str,
) -> Result<TaskPlan, String> {
    let gap = nodes
        .iter()
        .find(|node| node.id == gap_id && spec::is_gap_node(node))
        .ok_or("The selected gap is unavailable; refresh recovery.")?;
    // Preserve the existing adjacent-slot heuristic. This is not a proof of
    // direction/cardinality and does not extend the broker's relation allowlist.
    let slot = edges
        .iter()
        .find(|edge| edge.dst == gap_id)
        .or_else(|| edges.iter().find(|edge| edge.src == gap_id));
    let source_id = slot
        .map(|edge| {
            if edge.dst == gap_id {
                &edge.src
            } else {
                &edge.dst
            }
        })
        .map(String::as_str)
        .or_else(|| gap.props["source_id"].as_str())
        .ok_or("The selected gap has no source fact.")?;
    let source = nodes
        .iter()
        .find(|node| node.id == source_id)
        .ok_or("The selected gap's source fact is unavailable.")?;
    let edge_label = slot
        .map(|edge| edge.label.as_str())
        .or_else(|| gap.props["edge_label"].as_str())
        .unwrap_or("CALLS");
    let subgraph = semantic::context::khop_subgraph(nodes, edges, gap_id, 2);
    let in_context: BTreeSet<_> = subgraph
        .nodes
        .iter()
        .map(|(_, node)| node.id.as_str())
        .collect();
    let mut requests = vec![request(source, false)?, request(gap, false)?];
    let mut unplanned_candidates = 0usize;
    for node in nodes {
        if node.id == source_id
            || node.id == gap_id
            || !in_context.contains(node.id.as_str())
            || spec::is_gap_node(node)
        {
            continue;
        }
        let provenance = spec::provenance(&node.props, &node.id);
        if provenance.confidence_tier == ConfidenceTier::Gap || provenance.evidence.is_empty() {
            continue;
        }
        if requests.len() == MAX_REQUESTS {
            unplanned_candidates += 1;
        } else {
            requests.push(request(node, true)?);
        }
    }
    Ok(TaskPlan {
        action_id: action_id.to_owned(),
        gap_id: gap_id.to_owned(),
        source_id: source_id.to_owned(),
        edge_label: edge_label.to_owned(),
        requests,
        slot: slot
            .map(|edge| Ok((FactKey::from_edge(edge), edge_digest(edge)?)))
            .transpose()
            .map_err(|_: core_graph::GraphError| "Task slot is invalid.")?,
        unplanned_candidates,
    })
}
