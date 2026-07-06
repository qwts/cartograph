//! Spec compiler: graph → official artifacts (SPEC-00 §7).
//!
//! M2 brought the first artifact (resource/topology map); M3 adds flow
//! dossiers (Markdown + Mermaid sequence + provenance table per flow).
//! Both are portable, renderable anywhere, diffable in a PR. The full
//! artifact set (US/AC, registers) arrives at M9.

use core_graph::{Edge, Node};
use flowtracer::Flow;
use std::collections::BTreeMap;
use std::fmt::Write;

/// Edge labels that appear on the topology map — callers use this to query
/// exactly the edges the artifact renders.
pub const TOPOLOGY_EDGE_LABELS: &[&str] = &[
    "TRIGGERS",
    "ROUTES",
    "SUBSCRIBES",
    "GRANTS",
    "DEPENDS_ON",
    "REFERENCES",
];

/// Edge labels that appear on the topology map, with their arrow style.
/// Capability edges are solid and labeled; the raw reference DAG is dotted.
const TOPOLOGY_EDGES: &[(&str, &str)] = &[
    ("TRIGGERS", "-->"),
    ("ROUTES", "-->"),
    ("SUBSCRIBES", "-->"),
    ("GRANTS", "-->"),
    ("DEPENDS_ON", "-.->"),
    ("REFERENCES", "-.->"),
];

fn mermaid_id(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Compile the resource/topology map (SPEC-00 §7 artifact table) from
/// `Resource` nodes and their infra edges. Deterministic output: same graph,
/// same text (US-0014).
pub fn topology_mermaid(nodes: &[Node], edges: &[Edge]) -> String {
    let resources: BTreeMap<&str, &Node> = nodes
        .iter()
        .filter(|n| n.label == "Resource")
        .map(|n| (n.id.as_str(), n))
        .collect();

    let mut out = String::from("flowchart LR\n");
    for (id, node) in &resources {
        let display = node
            .props
            .get("logical_id")
            .and_then(|v| v.as_str())
            .unwrap_or(id.strip_prefix("res:").unwrap_or(id));
        let placeholder = node.props.get("placeholder").is_some();
        let (open, close) = if placeholder {
            ("([", "])")
        } else {
            ("[", "]")
        };
        let suffix = if placeholder { " ?" } else { "" };
        writeln!(
            out,
            "    {}{}\"{}{}\"{}",
            mermaid_id(id),
            open,
            display,
            suffix,
            close
        )
        .expect("write to string");
    }

    let mut lines = Vec::new();
    for edge in edges {
        let Some((_, arrow)) = TOPOLOGY_EDGES.iter().find(|(l, _)| *l == edge.label) else {
            continue;
        };
        if !resources.contains_key(edge.src.as_str()) || !resources.contains_key(edge.dst.as_str())
        {
            continue;
        }
        lines.push(format!(
            "    {} {}|{}| {}",
            mermaid_id(&edge.src),
            arrow,
            edge.label,
            mermaid_id(&edge.dst)
        ));
    }
    lines.sort();
    lines.dedup();
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Compile the flow-dossier artifact (SPEC-00 §7: Markdown + Mermaid
/// sequence + provenance table per flow). Deterministic for a given trace
/// (US-0014); statuses and tiers are always visible (R-INT-2), Gap hops
/// render with a distinct broken arrow (R-INT-4).
pub fn flow_dossier(flows: &[Flow]) -> String {
    let mut out = String::from("# Flow dossier\n");
    for flow in flows {
        writeln!(
            out,
            "\n## {} — {} (score {:.2})\n",
            flow.trigger_name,
            flow.status.as_str(),
            flow.score
        )
        .expect("write to string");
        writeln!(out, "Trigger: {} `{}`", flow.trigger_kind, flow.trigger)
            .expect("write to string");

        if flow.hops.is_empty() {
            out.push_str("\nNo hops resolved from this trigger.\n");
            continue;
        }

        // Mermaid sequence: participants in first-appearance order, one
        // arrow per hop; Gap hops get the broken arrow.
        let mut participants: Vec<(String, String)> = Vec::new(); // (id, name)
        let alias = |participants: &mut Vec<(String, String)>, id: &str, name: &str| -> String {
            if let Some(i) = participants.iter().position(|(pid, _)| pid == id) {
                format!("p{i}")
            } else {
                participants.push((id.to_string(), name.to_string()));
                format!("p{}", participants.len() - 1)
            }
        };
        let mut arrows = String::new();
        for hop in &flow.hops {
            let a = alias(&mut participants, &hop.src, &hop.src_name);
            let b = alias(&mut participants, &hop.dst, &hop.dst_name);
            let arrow = if hop.confidence == "Gap" {
                "--x"
            } else {
                "->>"
            };
            writeln!(
                arrows,
                "    {}{}{}: {} [{}]",
                a, arrow, b, hop.label, hop.confidence
            )
            .expect("write to string");
        }
        out.push_str("\n```mermaid\nsequenceDiagram\n");
        for (i, (_, name)) in participants.iter().enumerate() {
            writeln!(out, "    participant p{i} as {}", name.replace('\n', " "))
                .expect("write to string");
        }
        out.push_str(&arrows);
        out.push_str("```\n");

        // Provenance table (R-INT-2: tier + confidence on every hop).
        out.push_str("\n| # | Hop | Tier | Confidence | Evidence |\n");
        out.push_str("|---|-----|------|------------|----------|\n");
        for (i, hop) in flow.hops.iter().enumerate() {
            writeln!(
                out,
                "| {} | {} `{}` → `{}` | {} | {} | {} |",
                i + 1,
                hop.label,
                hop.src,
                hop.dst,
                hop.tier,
                hop.confidence,
                hop.evidence.as_deref().unwrap_or("—"),
            )
            .expect("write to string");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(id: &str, logical: &str) -> Node {
        Node {
            id: id.into(),
            label: "Resource".into(),
            props: serde_json::json!({ "logical_id": logical }),
        }
    }

    fn edge(src: &str, dst: &str, label: &str) -> Edge {
        Edge {
            src: src.into(),
            dst: dst.into(),
            label: label.into(),
            props: serde_json::json!({}),
        }
    }

    #[test]
    fn topology_map_renders_capability_and_reference_edges() {
        let nodes = vec![
            resource("res:aws_sqs_queue.orders", "aws_sqs_queue.orders"),
            resource(
                "res:aws_lambda_function.fulfill",
                "aws_lambda_function.fulfill",
            ),
        ];
        let edges = vec![
            edge(
                "res:aws_sqs_queue.orders",
                "res:aws_lambda_function.fulfill",
                "TRIGGERS",
            ),
            edge(
                "res:aws_lambda_function.fulfill",
                "res:aws_sqs_queue.orders",
                "REFERENCES",
            ),
            // Non-topology edges never leak onto the infra map.
            edge(
                "res:aws_sqs_queue.orders",
                "res:aws_lambda_function.fulfill",
                "CALLS",
            ),
        ];
        let mmd = topology_mermaid(&nodes, &edges);
        assert!(mmd.starts_with("flowchart LR\n"));
        assert!(mmd.contains(r#"res_aws_sqs_queue_orders["aws_sqs_queue.orders"]"#));
        assert!(
            mmd.contains("res_aws_sqs_queue_orders -->|TRIGGERS| res_aws_lambda_function_fulfill")
        );
        assert!(mmd.contains("-.->|REFERENCES|"));
        assert!(!mmd.contains("CALLS"));
    }

    #[test]
    fn placeholders_render_distinctly() {
        // R-INT-2 at the artifact level: unresolved is visibly unresolved.
        let mut unresolved = resource("res:module.vpc", "module.vpc");
        unresolved.props = serde_json::json!({ "placeholder": true });
        let mmd = topology_mermaid(&[unresolved], &[]);
        assert!(mmd.contains(r#"res_module_vpc(["module.vpc ?"])"#));
    }

    fn hop(label: &str, src: &str, dst: &str, confidence: &str) -> flowtracer::Hop {
        flowtracer::Hop {
            label: label.into(),
            src: src.into(),
            dst: dst.into(),
            src_name: src.into(),
            dst_name: dst.into(),
            tier: "Deterministic".into(),
            confidence: confidence.into(),
            evidence: Some("src/app.ts bytes 1..9".into()),
        }
    }

    #[test]
    fn flow_dossier_renders_sequence_and_provenance_table() {
        let flows = vec![Flow {
            trigger: "ep:GET:/users".into(),
            trigger_kind: "Endpoint".into(),
            trigger_name: "GET /users".into(),
            hops: vec![
                hop("HANDLES", "ep:GET:/users", "sym:app.ts#list", "Confirmed"),
                hop("PUBLISHES", "sym:app.ts#list", "gap:chan:app.ts@5", "Gap"),
            ],
            status: flowtracer::FlowStatus::Partial,
            score: 0.5,
        }];
        let dossier = flow_dossier(&flows);
        assert!(dossier.starts_with("# Flow dossier\n"));
        assert!(dossier.contains("## GET /users — Partial (score 0.50)"));
        assert!(dossier.contains("sequenceDiagram"));
        // Confirmed hops are solid, Gap hops visibly broken (R-INT-4).
        assert!(dossier.contains("p0->>p1: HANDLES [Confirmed]"));
        assert!(dossier.contains("p1--xp2: PUBLISHES [Gap]"));
        // Provenance table carries tier + confidence + evidence (R-INT-2).
        assert!(dossier.contains("| 1 | HANDLES `ep:GET:/users` → `sym:app.ts#list` | Deterministic | Confirmed | src/app.ts bytes 1..9 |"));
        // Deterministic (US-0014).
        assert_eq!(dossier, flow_dossier(&flows));
    }

    #[test]
    fn output_is_deterministic() {
        let nodes = vec![resource("res:b.b", "b.b"), resource("res:a.a", "a.a")];
        let edges = vec![
            edge("res:b.b", "res:a.a", "REFERENCES"),
            edge("res:a.a", "res:b.b", "TRIGGERS"),
        ];
        assert_eq!(
            topology_mermaid(&nodes, &edges),
            topology_mermaid(&nodes, &edges)
        );
        // Nodes are ordered by id regardless of input order.
        let mmd = topology_mermaid(&nodes, &edges);
        let a_pos = mmd.find("res_a_a[").unwrap();
        let b_pos = mmd.find("res_b_b[").unwrap();
        assert!(a_pos < b_pos);
    }
}
