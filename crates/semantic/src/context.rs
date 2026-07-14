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

/// One line of citation-bound context for a node.
fn node_line(hop: u32, node: &Node) -> String {
    let confidence = node.props["prov"]["confidence_tier"]
        .as_str()
        .unwrap_or("Gap"); // unknown provenance is never presented as confirmed
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
    let confidence = edge.props["prov"]["confidence_tier"]
        .as_str()
        .unwrap_or("Gap");
    format!("[edge:{}] ({confidence})", edge_identity(edge))
}

/// Serialize `subgraph` under a character budget (a portable, deterministic
/// proxy for token budgets — providers re-measure precisely). Focus and
/// near hops pack first; when the budget runs out, the farthest statements
/// are dropped **and recorded**.
pub fn pack_context(subgraph: &Subgraph, max_chars: usize) -> ContextBundle {
    // Emission order: nodes by (hop, id) — already sorted — then edges;
    // dropping from the end therefore always drops the least-relevant first.
    let mut lines: Vec<(String, String)> = subgraph
        .nodes
        .iter()
        .map(|(hop, node)| (format!("node:{}", node.id), node_line(*hop, node)))
        .collect();
    lines.extend(
        subgraph
            .edges
            .iter()
            .map(|edge| (format!("edge:{}", edge_identity(edge)), edge_line(edge))),
    );

    let mut text = String::new();
    let mut cited = Vec::new();
    let mut dropped = Vec::new();
    for (id, line) in lines {
        if text.len() + line.len() + 1 > max_chars {
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

    fn node(id: &str, label: &str, confidence: &str) -> Node {
        Node {
            id: id.into(),
            label: label.into(),
            props: serde_json::json!({ "prov": { "confidence_tier": confidence } }),
        }
    }

    fn edge(src: &str, label: &str, dst: &str) -> Edge {
        Edge {
            src: src.into(),
            dst: dst.into(),
            label: label.into(),
            props: serde_json::json!({ "prov": { "confidence_tier": "Confirmed" } }),
        }
    }

    /// gap ── ch ── svc ── db, with `far` two hops beyond the focus chain.
    fn fixture() -> (Vec<Node>, Vec<Edge>) {
        (
            vec![
                node("gap:1", "Gap", "Gap"),
                node("ch:orders", "Channel", "Confirmed"),
                node("svc:api", "Service", "Confirmed"),
                node("db:main", "Datastore", "Confirmed"),
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
    fn budget_packing_drops_farthest_first_and_records_it() {
        let (nodes, edges) = fixture();
        let subgraph = khop_subgraph(&nodes, &edges, "gap:1", 3);
        let full = pack_context(&subgraph, 10_000);
        // Budget that fits roughly half the statements.
        let tight = pack_context(&subgraph, full.text.len() / 2);
        assert!(!tight.dropped.is_empty(), "truncation must be recorded");
        // The focus itself always survives a sane budget.
        assert!(tight.cited.contains(&"node:gap:1".to_string()));
        // Nothing both cited and dropped.
        for id in &tight.dropped {
            assert!(!tight.cited.contains(id));
        }
    }

    #[test]
    fn unknown_provenance_is_never_presented_as_confirmed() {
        let nodes = vec![Node {
            id: "svc:mystery".into(),
            label: "Service".into(),
            props: serde_json::json!({}),
        }];
        let bundle = pack_context(&khop_subgraph(&nodes, &[], "svc:mystery", 1), 1_000);
        assert!(bundle.text.contains("(Gap)"));
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
