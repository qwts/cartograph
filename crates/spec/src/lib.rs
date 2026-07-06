//! Spec compiler: graph → official artifacts (SPEC-00 §7).
//!
//! M2 brings the first artifact: the resource/topology map as Mermaid
//! (portable, renderable anywhere, diffable in a PR). The full artifact set
//! (US/AC, flow dossiers, registers) arrives at M9.

use core_graph::{Edge, Node};
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
