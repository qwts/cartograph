//! Deterministic, citation-bound graph context assembly (#121).
//!
//! The GraphRAG layer between the knowledge graph and any model tier: a
//! gap-centered (or any focus) k-hop subgraph, serialized so that **every
//! statement carries the graph identity it came from**. Model outputs must
//! cite these identities, and citations resolve back to nodes/edges — the
//! T3 broker's closed candidate set builds on exactly this property.
//!
//! Invariants:
//! - **Deterministic**: identical inputs yield byte-identical context
//!   (adjacency and output ordered by stable ids, never hash order).
//! - **No silent truncation**: packing under a budget records what was
//!   dropped; a consumer can always tell the context was partial.
//! - **No LLM here**: this layer *feeds* providers and stays pure.

use core_graph::{Edge, Node};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// A k-hop neighborhood around one focus identity.
#[derive(Debug, Clone, Serialize)]
pub struct Subgraph {
    /// The node id (or edge identity) the context is centered on.
    pub focus: String,
    /// Member nodes with their hop distance from the focus.
    pub nodes: Vec<(u32, Node)>,
    /// Edges whose endpoints are both members.
    pub edges: Vec<Edge>,
}

/// Citation-bound prompt context plus its truncation record.
#[derive(Debug, Clone, Serialize)]
pub struct ContextBundle {
    /// One statement per line, each prefixed with its citation id
    /// (`[node:…]` / `[edge:…]`).
    pub text: String,
    /// Citation ids present in `text`, in order.
    pub cited: Vec<String>,
    /// Identities dropped to fit the budget — never silent (#121).
    pub dropped: Vec<String>,
}

/// Stable citation identity for an edge.
pub fn edge_identity(edge: &Edge) -> String {
    format!("{} {} {}", edge.src, edge.label, edge.dst)
}

/// Extract the k-hop neighborhood of `focus` (a node id) over an undirected
/// view of the graph. Nodes and edges come back in stable id order.
pub fn khop_subgraph(nodes: &[Node], edges: &[Edge], focus: &str, k: u32) -> Subgraph {
    let mut adjacency: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for edge in edges {
        adjacency.entry(&edge.src).or_default().insert(&edge.dst);
        adjacency.entry(&edge.dst).or_default().insert(&edge.src);
    }

    let mut distance: BTreeMap<&str, u32> = BTreeMap::new();
    let mut frontier: Vec<&str> = vec![focus];
    distance.insert(focus, 0);
    for hop in 1..=k {
        let mut next = Vec::new();
        for id in frontier {
            let Some(neighbors) = adjacency.get(id) else {
                continue;
            };
            for neighbor in neighbors {
                if !distance.contains_key(neighbor) {
                    distance.insert(neighbor, hop);
                    next.push(*neighbor);
                }
            }
        }
        frontier = next;
    }

    let members: BTreeSet<&str> = distance.keys().copied().collect();
    let mut member_nodes: Vec<(u32, Node)> = nodes
        .iter()
        .filter(|node| members.contains(node.id.as_str()))
        .map(|node| (distance[node.id.as_str()], node.clone()))
        .collect();
    member_nodes.sort_by(|(da, a), (db, b)| da.cmp(db).then_with(|| a.id.cmp(&b.id)));

    let mut member_edges: Vec<Edge> = edges
        .iter()
        .filter(|edge| members.contains(edge.src.as_str()) && members.contains(edge.dst.as_str()))
        .cloned()
        .collect();
    member_edges.sort_by_key(edge_identity);

    Subgraph {
        focus: focus.to_string(),
        nodes: member_nodes,
        edges: member_edges,
    }
}

/// Merge structural candidates (graph traversal) with similarity candidates
/// (ANN hits) into one deduplicated, order-preserving list: structure first
/// (it is deterministic), then similar-but-unconnected identities.
pub fn merge_candidates(structural: &[String], similar: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    structural
        .iter()
        .chain(similar.iter())
        .filter(|id| seen.insert(id.as_str().to_string()))
        .cloned()
        .collect()
}

/// Validated confidence for prompt context: props are arbitrary JSON, so
/// the provenance must deserialize **and** pass the integrity validator
/// before its confidence is repeated to a model — anything else renders as
/// Gap (unknown is never presented as confirmed; R-INT-2 extended to
/// prompts).
fn validated_confidence(props: &serde_json::Value) -> &'static str {
    let confidence = serde_json::from_value::<core_prov::Provenance>(props["prov"].clone())
        .ok()
        .filter(|provenance| provenance.validate().is_ok())
        .map(|provenance| provenance.confidence_tier);
    match confidence {
        Some(core_prov::ConfidenceTier::Confirmed) => "Confirmed",
        Some(core_prov::ConfidenceTier::InferredStrong) => "InferredStrong",
        Some(core_prov::ConfidenceTier::InferredWeak) => "InferredWeak",
        Some(core_prov::ConfidenceTier::Gap) | None => "Gap",
    }
}

/// One line of citation-bound context for a node.
fn node_line(hop: u32, node: &Node) -> String {
    let confidence = validated_confidence(&node.props);
    let name = ["title", "name", "identity", "path", "reason"]
        .iter()
        .find_map(|key| node.props[*key].as_str())
        .unwrap_or(&node.id);
    format!(
        "[node:{}] hop={hop} {} {name} ({confidence})",
        node.id, node.label
    )
}

/// One line of citation-bound context for an edge.
fn edge_line(edge: &Edge) -> String {
    format!(
        "[edge:{}] ({})",
        edge_identity(edge),
        validated_confidence(&edge.props)
    )
}

/// Serialize `subgraph` under a character budget (a portable, deterministic
/// proxy for token budgets — providers re-measure precisely). Statements
/// pack strictly by relevance — nodes and edges interleaved by hop distance
/// (an edge's hop is its farthest endpoint) — and packing stops at the first
/// overflow, so a tight budget always keeps a relevance **prefix**: the
/// focus and its nearest facts, with their connecting edges, never a farther
/// statement in place of a nearer one. Everything cut is recorded.
pub fn pack_context(subgraph: &Subgraph, max_chars: usize) -> ContextBundle {
    let hop_of = |id: &str| -> u32 {
        subgraph
            .nodes
            .iter()
            .find(|(_, node)| node.id == id)
            .map(|(hop, _)| *hop)
            .unwrap_or(u32::MAX)
    };
    // (hop, node-before-edge, id) — deterministic relevance order.
    let mut lines: Vec<(u32, u8, String, String)> = subgraph
        .nodes
        .iter()
        .map(|(hop, node)| {
            (
                *hop,
                0u8,
                format!("node:{}", node.id),
                node_line(*hop, node),
            )
        })
        .collect();
    lines.extend(subgraph.edges.iter().map(|edge| {
        let hop = hop_of(&edge.src).max(hop_of(&edge.dst));
        (
            hop,
            1u8,
            format!("edge:{}", edge_identity(edge)),
            edge_line(edge),
        )
    }));
    lines.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));

    let mut text = String::new();
    let mut cited = Vec::new();
    let mut dropped = Vec::new();
    let mut overflowed = false;
    for (_, _, id, line) in lines {
        if overflowed || text.len() + line.len() + 1 > max_chars {
            // First overflow ends packing entirely: skipping ahead to
            // shorter, farther statements would break the relevance prefix.
            overflowed = true;
            dropped.push(id);
            continue;
        }
        text.push_str(&line);
        text.push('\n');
        cited.push(id);
    }
    ContextBundle {
        text,
        cited,
        dropped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real, validator-passing provenance — the only thing the serializer
    /// may present as anything other than Gap.
    fn prov(confidence: core_prov::ConfidenceTier) -> serde_json::Value {
        let tier = match confidence {
            core_prov::ConfidenceTier::Confirmed => core_prov::Tier::Deterministic,
            core_prov::ConfidenceTier::InferredStrong => core_prov::Tier::Semantic,
            core_prov::ConfidenceTier::InferredWeak => core_prov::Tier::Agentic,
            core_prov::ConfidenceTier::Gap => core_prov::Tier::Deterministic,
        };
        serde_json::to_value(
            core_prov::Provenance::new(tier, confidence, vec![], "t0.test", b"fixture")
                .expect("within ceiling"),
        )
        .expect("serializes")
    }

    fn node(id: &str, label: &str, confidence: core_prov::ConfidenceTier) -> Node {
        Node {
            id: id.into(),
            label: label.into(),
            props: serde_json::json!({ "prov": prov(confidence) }),
        }
    }

    fn edge(src: &str, label: &str, dst: &str) -> Edge {
        Edge {
            src: src.into(),
            dst: dst.into(),
            label: label.into(),
            props: serde_json::json!({ "prov": prov(core_prov::ConfidenceTier::Confirmed) }),
        }
    }

    /// gap ── ch ── svc ── db: a focus chain with one node per hop.
    fn fixture() -> (Vec<Node>, Vec<Edge>) {
        use core_prov::ConfidenceTier::{Confirmed, Gap};
        (
            vec![
                node("gap:1", "Gap", Gap),
                node("ch:orders", "Channel", Confirmed),
                node("svc:api", "Service", Confirmed),
                node("db:main", "Datastore", Confirmed),
            ],
            vec![
                edge("gap:1", "PUBLISHES", "ch:orders"),
                edge("ch:orders", "SUBSCRIBES", "svc:api"),
                edge("svc:api", "WRITES", "db:main"),
            ],
        )
    }

    #[test]
    fn khop_bounds_the_neighborhood_and_orders_by_hop() {
        let (nodes, edges) = fixture();
        let subgraph = khop_subgraph(&nodes, &edges, "gap:1", 2);
        let ids: Vec<_> = subgraph
            .nodes
            .iter()
            .map(|(hop, n)| (*hop, n.id.as_str()))
            .collect();
        // db:main is 3 hops out — excluded at k=2.
        assert_eq!(ids, vec![(0, "gap:1"), (1, "ch:orders"), (2, "svc:api")]);
        // Only edges with both endpoints inside the neighborhood survive.
        assert_eq!(subgraph.edges.len(), 2);
    }

    #[test]
    fn context_is_deterministic_and_citation_bound() {
        let (nodes, edges) = fixture();
        let a = pack_context(&khop_subgraph(&nodes, &edges, "gap:1", 3), 10_000);
        let b = pack_context(&khop_subgraph(&nodes, &edges, "gap:1", 3), 10_000);
        // Byte-identical across runs (re-ingest determinism extends to prompts).
        assert_eq!(a.text, b.text);
        assert!(a.dropped.is_empty());
        // Every cited identity appears verbatim in the text and resolves to
        // a member node/edge — citations round-trip.
        for id in &a.cited {
            assert!(a.text.contains(&format!("[{id}]")));
        }
        assert!(a.cited.contains(&"node:gap:1".to_string()));
        assert!(a.cited.contains(&"edge:svc:api WRITES db:main".to_string()));
    }

    #[test]
    fn budget_packing_keeps_a_strict_relevance_prefix() {
        let (nodes, edges) = fixture();
        let subgraph = khop_subgraph(&nodes, &edges, "gap:1", 3);
        let full = pack_context(&subgraph, 10_000);
        // Edges interleave with nodes by hop: the focus→hop-1 edge is cited
        // before the hop-2 node, so a tight budget keeps connections, not
        // just islands.
        let hop1_edge = full
            .cited
            .iter()
            .position(|id| id == "edge:gap:1 PUBLISHES ch:orders")
            .expect("hop-1 edge cited");
        let hop2_node = full
            .cited
            .iter()
            .position(|id| id == "node:svc:api")
            .expect("hop-2 node cited");
        assert!(hop1_edge < hop2_node);

        // Budget that fits roughly half the statements.
        let tight = pack_context(&subgraph, full.text.len() / 2);
        assert!(!tight.dropped.is_empty(), "truncation must be recorded");
        // Strict prefix: what survives is exactly the head of the full
        // relevance order — never a farther statement in place of a nearer
        // one that overflowed.
        assert_eq!(tight.cited, full.cited[..tight.cited.len()]);
        assert!(tight.cited.contains(&"node:gap:1".to_string()));
        for id in &tight.dropped {
            assert!(!tight.cited.contains(id));
        }
    }

    #[test]
    fn unvalidated_provenance_is_never_presented_as_confirmed() {
        // Missing prov entirely, and — the sharper case from review — a
        // malformed prov that *claims* Confirmed but does not deserialize
        // and validate: both must render as Gap.
        let nodes = vec![
            Node {
                id: "svc:mystery".into(),
                label: "Service".into(),
                props: serde_json::json!({}),
            },
            Node {
                id: "svc:liar".into(),
                label: "Service".into(),
                props: serde_json::json!({ "prov": { "confidence_tier": "Confirmed" } }),
            },
        ];
        for focus in ["svc:mystery", "svc:liar"] {
            let bundle = pack_context(&khop_subgraph(&nodes, &[], focus, 1), 1_000);
            assert!(bundle.text.contains("(Gap)"), "{focus} must render as Gap");
            assert!(!bundle.text.contains("(Confirmed)"));
        }
    }

    #[test]
    fn hybrid_merge_dedupes_preserving_structural_priority() {
        let merged = merge_candidates(
            &["svc:api".into(), "ch:orders".into()],
            &["svc:api".into(), "svc:similar".into()],
        );
        assert_eq!(merged, vec!["svc:api", "ch:orders", "svc:similar"]);
    }
}
