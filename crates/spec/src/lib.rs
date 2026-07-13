//! Spec compiler: graph → official artifacts (SPEC-00 §7).
//!
//! M2 brought the first artifact (resource/topology map); M3 adds flow
//! dossiers (Markdown + Mermaid sequence + provenance table per flow).
//! Both are portable, renderable anywhere, diffable in a PR. M9 adds the full
//! artifact set (US/AC, data model, ADRs, and registers).

use core_graph::{Edge, Node};
use flowtracer::Flow;
use std::collections::BTreeMap;
use std::fmt::Write;

mod workbench;
pub use workbench::{ExportMode, SpecArtifact, SpecAssertion, SpecBundle, compile_spec};

/// Edge labels that appear on the topology map — callers use this to query
/// exactly the edges the artifact renders.
pub const TOPOLOGY_EDGE_LABELS: &[&str] = &[
    "TRIGGERS",
    "ROUTES",
    "SUBSCRIBES",
    "GRANTS",
    "BACKS",
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
    ("BACKS", "-->"),
    ("DEPENDS_ON", "-.->"),
    ("REFERENCES", "-.->"),
];

/// Mermaid identifier for a graph node id: sanitized for readability plus
/// a short content hash for uniqueness — sanitization alone collapses ids
/// differing only in punctuation (`acme/foo-bar` vs `acme/foo_bar`), which
/// would visually re-merge nodes the graph keeps distinct.
fn mermaid_id(id: &str) -> String {
    let sanitized: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let hash = blake3::hash(id.as_bytes()).to_hex();
    format!("{sanitized}_{}", &hash.as_str()[..8])
}

/// Compile the resource/topology map (SPEC-00 §7 artifact table) from
/// `Resource` nodes and their infra edges. Channels join the map only via
/// an observed `BACKS` edge (M6: deployed resource → the code-layer
/// channel it backs) and render as cylinders. Deterministic output: same
/// graph, same text (US-0014).
pub fn topology_mermaid(nodes: &[Node], edges: &[Edge]) -> String {
    let resources: BTreeMap<&str, &Node> = nodes
        .iter()
        .filter(|n| n.label == "Resource")
        .map(|n| (n.id.as_str(), n))
        .collect();
    let channels: BTreeMap<&str, &Node> = nodes
        .iter()
        .filter(|n| n.label == "Channel")
        .map(|n| (n.id.as_str(), n))
        .collect();
    let backed: std::collections::BTreeSet<&str> = edges
        .iter()
        .filter(|e| {
            e.label == "BACKS"
                && resources.contains_key(e.src.as_str())
                && channels.contains_key(e.dst.as_str())
        })
        .map(|e| e.dst.as_str())
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

    for id in &backed {
        let display = id.strip_prefix("chan:").unwrap_or(id);
        writeln!(out, "    {}[(\"{}\")]", mermaid_id(id), display).expect("write to string");
    }

    let mut lines = Vec::new();
    for edge in edges {
        let Some((_, arrow)) = TOPOLOGY_EDGES.iter().find(|(l, _)| *l == edge.label) else {
            continue;
        };
        let endpoints_known = resources.contains_key(edge.src.as_str())
            && (resources.contains_key(edge.dst.as_str())
                || (edge.label == "BACKS" && backed.contains(edge.dst.as_str())));
        if !endpoints_known {
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
        if flow.depth_limited {
            out.push_str(
                "\n**Truncated at the traversal depth bound** — downstream hops exist that are not shown.\n",
            );
        }

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
        out.push_str("\n| # | Hop | Tier | Confidence | Evidence | Extractor | Content hash |\n");
        out.push_str("|---|-----|------|------------|----------|-----------|--------------|\n");
        for (i, hop) in flow.hops.iter().enumerate() {
            let evidence = if hop.provenance.evidence.is_empty() {
                "—".into()
            } else {
                hop.provenance
                    .evidence
                    .iter()
                    .map(|evidence| {
                        format!(
                            "{}:{} bytes {}..{} @ {}",
                            evidence.repo,
                            evidence.path,
                            evidence.byte_start,
                            evidence.byte_end,
                            evidence.commit_sha
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            writeln!(
                out,
                "| {} | {} `{}` → `{}` | {} | {} | {} | `{}` | `{}` |",
                i + 1,
                hop.label,
                hop.src,
                hop.dst,
                hop.tier,
                hop.confidence,
                evidence,
                hop.provenance.extractor_id,
                hop.provenance.content_hash,
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
        let q = mermaid_id("res:aws_sqs_queue.orders");
        let f = mermaid_id("res:aws_lambda_function.fulfill");
        assert!(mmd.contains(&format!(r#"{q}["aws_sqs_queue.orders"]"#)));
        assert!(mmd.contains(&format!("{q} -->|TRIGGERS| {f}")));
        assert!(mmd.contains("-.->|REFERENCES|"));
        assert!(!mmd.contains("CALLS"));
    }

    #[test]
    fn placeholders_render_distinctly() {
        // R-INT-2 at the artifact level: unresolved is visibly unresolved.
        let mut unresolved = resource("res:module.vpc", "module.vpc");
        unresolved.props = serde_json::json!({ "placeholder": true });
        let mmd = topology_mermaid(&[unresolved], &[]);
        assert!(mmd.contains(&format!(
            r#"{}(["module.vpc ?"])"#,
            mermaid_id("res:module.vpc")
        )));
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
            provenance: core_prov::Provenance::new(
                core_prov::Tier::Deterministic,
                match confidence {
                    "Confirmed" => core_prov::ConfidenceTier::Confirmed,
                    "InferredStrong" => core_prov::ConfidenceTier::InferredStrong,
                    "InferredWeak" => core_prov::ConfidenceTier::InferredWeak,
                    _ => core_prov::ConfidenceTier::Gap,
                },
                vec![core_prov::EvidenceRef {
                    repo: "local/test".into(),
                    path: "src/app.ts".into(),
                    byte_start: 1,
                    byte_end: 9,
                    commit_sha: "abc123".into(),
                }],
                "spec.test",
                format!("{src}:{label}:{dst}").as_bytes(),
            )
            .unwrap(),
            gap_reason: None,
            attempted_tiers: Vec::new(),
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
            depth_limited: false,
        }];
        let dossier = flow_dossier(&flows);
        assert!(dossier.starts_with("# Flow dossier\n"));
        assert!(dossier.contains("## GET /users — Partial (score 0.50)"));
        assert!(dossier.contains("sequenceDiagram"));
        // Confirmed hops are solid, Gap hops visibly broken (R-INT-4).
        assert!(dossier.contains("p0->>p1: HANDLES [Confirmed]"));
        assert!(dossier.contains("p1--xp2: PUBLISHES [Gap]"));
        // Provenance table carries tier + confidence + evidence (R-INT-2).
        assert!(dossier.contains("| 1 | HANDLES `ep:GET:/users` → `sym:app.ts#list` | Deterministic | Confirmed | local/test:src/app.ts bytes 1..9 @ abc123 | `spec.test` |"));
        // Deterministic (US-0014).
        assert_eq!(dossier, flow_dossier(&flows));
    }

    #[test]
    fn backs_edge_renders_the_channel_as_a_cylinder() {
        // M6: the infra↔code join is visible on the topology map — but a
        // channel appears only through an observed BACKS edge.
        let queue = resource(
            "res:local/infra@aws_sqs_queue.orders",
            "aws_sqs_queue.orders",
        );
        let chan = Node {
            id: "chan:sqs-queue:https://sqs.us-east-1.amazonaws.com/1/orders".into(),
            label: "Channel".into(),
            props: serde_json::json!({}),
        };
        let orphan_chan = Node {
            id: "chan:sqs-queue:https://sqs.us-east-1.amazonaws.com/1/other".into(),
            label: "Channel".into(),
            props: serde_json::json!({}),
        };
        let backs = edge(
            "res:local/infra@aws_sqs_queue.orders",
            "chan:sqs-queue:https://sqs.us-east-1.amazonaws.com/1/orders",
            "BACKS",
        );
        let mmd = topology_mermaid(&[queue, chan, orphan_chan], &[backs]);
        assert!(mmd.contains(r#"[("sqs-queue:https://sqs.us-east-1.amazonaws.com/1/orders")]"#));
        assert!(mmd.contains("-->|BACKS|"));
        // No BACKS edge, no cylinder: channels are not infra topology on
        // their own.
        assert!(!mmd.contains("1/other"));
    }

    #[test]
    fn punctuation_variant_repos_render_distinctly() {
        // Sanitization alone maps acme/foo-bar and acme/foo_bar to the same
        // Mermaid identifier; the hash suffix keeps them apart.
        let a = mermaid_id("res:acme/foo-bar@aws_sqs_queue.q");
        let b = mermaid_id("res:acme/foo_bar@aws_sqs_queue.q");
        assert_ne!(a, b);
        // Deterministic: same id, same key.
        assert_eq!(a, mermaid_id("res:acme/foo-bar@aws_sqs_queue.q"));
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
        let a_pos = mmd.find(&format!("{}[", mermaid_id("res:a.a"))).unwrap();
        let b_pos = mmd.find(&format!("{}[", mermaid_id("res:b.b"))).unwrap();
        assert!(a_pos < b_pos);
    }
}
