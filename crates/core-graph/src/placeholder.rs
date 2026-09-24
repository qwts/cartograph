//! Referential closure with provenance (#237, AC-0207, ADR-0031).
//!
//! An adapter's edges can name endpoints no parsed declaration defines: an
//! import of a package the repository does not contain, or an in-system
//! target that did not resolve. The store needs every endpoint as a node, so
//! adapters close over them with flagged `placeholder` nodes. Each one is a
//! fact like any other and carries provenance: the evidence of the first
//! edge (in deterministic extraction order) that referenced it, the adapter
//! as extractor, and a human-readable `reason`.
//!
//! The adapter classifies each endpoint. Only a *proven* boundary is
//! Confirmed — `External` (the repository provably cannot provide it) or
//! `Internal` (a repository-declared package or directory with no single
//! declaring file). Everything else is `Unresolved`: an explicit Gap
//! (R-INT-4). A boundary with no citable evidence fails closed to a Gap.

use crate::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::HashSet;

/// How an adapter classifies one endpoint no parsed declaration defines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Boundary {
    /// Proven outside the recovered system: a stdlib module, or a package
    /// no source in the repository declares or could declare. Confirmed.
    External {
        /// Human-readable proof, e.g. `package not declared in this repository`.
        reason: String,
        /// Evidence beyond the referencing edge's (e.g. the deciding config).
        evidence: Vec<EvidenceRef>,
    },
    /// Proven inside the system but not one parsed declaration — a
    /// repository-declared package or package directory. Confirmed.
    Internal {
        /// Human-readable proof, e.g. `package declared in this repository`.
        reason: String,
        /// Evidence beyond the referencing edge's (e.g. the declaring manifest).
        evidence: Vec<EvidenceRef>,
    },
    /// Should resolve inside the system but did not — an explicit Gap.
    Unresolved {
        /// Human-readable stop reason shown in the Gap register.
        reason: String,
    },
}

impl Boundary {
    fn kind(&self) -> &'static str {
        match self {
            Boundary::External { .. } => "external",
            Boundary::Internal { .. } => "internal",
            Boundary::Unresolved { .. } => "unresolved",
        }
    }
}

/// The node label an endpoint id's scheme implies (`file:` → `File`, …).
pub fn label_for_id(id: &str) -> &'static str {
    match id.split(':').next() {
        Some("file") => "File",
        Some("sym") => "Symbol",
        Some("mod") => "Module",
        Some("ep") => "Endpoint",
        Some("res") => "Resource",
        Some("chan") => "Channel",
        Some("gap") => "Gap",
        Some("screen") => "Screen",
        _ => "Unknown",
    }
}

/// The default stop reason for an endpoint only `edge` names.
pub fn unresolved_reason(edge: &Edge) -> String {
    let relation = match edge.label.as_str() {
        "IMPORTS" => "import",
        "CALLS" => "call",
        "RENDERS" => "render",
        "HANDLES" => "handler",
        "DEFINED_IN" => "definition",
        "DEPENDS_ON" => "dependency",
        other => {
            return format!(
                "unresolved {} target",
                other.to_lowercase().replace('_', " ")
            );
        }
    };
    format!("unresolved {relation} target")
}

fn edge_provenance(edge: &Edge) -> Option<Provenance> {
    edge.props
        .get("prov")
        .cloned()
        .and_then(|value| serde_json::from_value::<Provenance>(value).ok())
}

/// The placeholder node for `id`, first referenced by `edge`. The node is
/// attributed to the extractor that produced `edge` (so a closure over a
/// merged, multi-adapter extraction still names the real producer), or to
/// `fallback_extractor` when the edge carries no readable provenance.
pub fn placeholder_node(
    id: &str,
    label: &str,
    edge: &Edge,
    boundary: Boundary,
    fallback_extractor: &str,
) -> Node {
    let (mut evidence, extractor_id) = match edge_provenance(edge) {
        Some(provenance) => (provenance.evidence, provenance.extractor_id),
        None => (Vec::new(), fallback_extractor.to_string()),
    };
    // A Confirmed boundary must cite the reference that established it;
    // without evidence it fails closed to an explicit Gap.
    let boundary = match boundary {
        Boundary::External { .. } | Boundary::Internal { .. } if evidence.is_empty() => {
            Boundary::Unresolved {
                reason: unresolved_reason(edge),
            }
        }
        boundary => boundary,
    };
    let kind = boundary.kind();
    let (confidence, reason) = match boundary {
        Boundary::External {
            reason,
            evidence: extra,
        }
        | Boundary::Internal {
            reason,
            evidence: extra,
        } => {
            evidence.extend(extra);
            (ConfidenceTier::Confirmed, reason)
        }
        Boundary::Unresolved { reason } => (ConfidenceTier::Gap, reason),
    };
    let provenance = Provenance::new(
        Tier::Deterministic,
        confidence,
        evidence,
        extractor_id,
        format!("{label} {id} {kind}: {reason}").as_bytes(),
    )
    .expect("Deterministic confidence is within its ceiling");
    let mut props = serde_json::json!({
        "placeholder": true,
        "boundary": kind,
        "reason": reason,
        "prov": provenance,
    });
    if confidence == ConfidenceTier::Gap {
        props["attempted_tiers"] = serde_json::json!(["T0"]);
    }
    Node {
        id: id.to_string(),
        label: label.to_string(),
        props,
    }
}

/// Ensure every edge endpoint exists as a node. Each missing endpoint
/// becomes one placeholder, labeled by `label_of` and cited with the
/// evidence of its first referencing edge. `classify` judges it from *every*
/// referencing edge (`None` = [`Boundary::Unresolved`] with
/// [`unresolved_reason`]): one unresolved reference keeps it a Gap, and
/// references that disagree on the boundary kind prove neither — so edge
/// order never lets one proof complete another reference's unresolved hop.
pub fn close_over_endpoints(
    nodes: &mut Vec<Node>,
    edges: &[Edge],
    fallback_extractor: &str,
    label_of: impl Fn(&str) -> &'static str,
    mut classify: impl FnMut(&str, &Edge) -> Option<Boundary>,
) {
    let known: HashSet<&str> = nodes.iter().map(|node| node.id.as_str()).collect();
    let mut order: Vec<&str> = Vec::new();
    let mut pending: std::collections::HashMap<&str, (&Edge, Boundary)> =
        std::collections::HashMap::new();
    for edge in edges {
        for id in [edge.src.as_str(), edge.dst.as_str()] {
            if known.contains(id) {
                continue;
            }
            let boundary = classify(id, edge).unwrap_or_else(|| Boundary::Unresolved {
                reason: unresolved_reason(edge),
            });
            match pending.get_mut(id) {
                None => {
                    order.push(id);
                    pending.insert(id, (edge, boundary));
                }
                Some((first, held)) => match (&*held, &boundary) {
                    (Boundary::Unresolved { .. }, _) => {}
                    (_, Boundary::Unresolved { .. }) => {
                        // Cite the reference that did not resolve.
                        *first = edge;
                        *held = boundary;
                    }
                    (held_kind, _) if held_kind.kind() != boundary.kind() => {
                        *held = Boundary::Unresolved {
                            reason: "references disagree on the boundary".into(),
                        };
                    }
                    _ => {}
                },
            }
        }
    }
    for id in order {
        let (edge, boundary) = pending.remove(id).expect("recorded above");
        nodes.push(placeholder_node(
            id,
            label_of(id),
            edge,
            boundary,
            fallback_extractor,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import_edge(dst: &str, evidence: Vec<EvidenceRef>) -> Edge {
        let prov = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            evidence,
            "t0.test",
            b"IMPORTS",
        )
        .unwrap();
        Edge {
            src: "file:r@a.java".into(),
            dst: dst.into(),
            label: "IMPORTS".into(),
            props: serde_json::json!({ "prov": prov }),
        }
    }

    fn span() -> EvidenceRef {
        EvidenceRef {
            repo: "r".into(),
            path: "a.java".into(),
            byte_start: 3,
            byte_end: 9,
            commit_sha: "c".into(),
        }
    }

    fn provenance(node: &Node) -> Provenance {
        serde_json::from_value(node.props["prov"].clone()).unwrap()
    }

    #[test]
    fn every_placeholder_cites_its_referencing_edge() {
        // AC-0207: no placeholder is minted without provenance.
        let edges = vec![import_edge("mod:x.Y", vec![span()])];
        let mut nodes = vec![Node {
            id: "file:r@a.java".into(),
            label: "File".into(),
            props: serde_json::json!({}),
        }];
        close_over_endpoints(&mut nodes, &edges, "t0.test", label_for_id, |_, _| None);
        let placeholder = &nodes[1];
        assert_eq!(placeholder.label, "Module");
        assert_eq!(placeholder.props["placeholder"], true);
        assert_eq!(placeholder.props["boundary"], "unresolved");
        assert_eq!(placeholder.props["reason"], "unresolved import target");
        let prov = provenance(placeholder);
        assert_eq!(prov.confidence_tier, ConfidenceTier::Gap);
        assert_eq!(prov.evidence, vec![span()]);
        assert_eq!(prov.extractor_id, "t0.test");
    }

    #[test]
    fn a_proven_external_boundary_is_confirmed_with_evidence() {
        // AC-0207: externality is Confirmed only with cited evidence.
        let edges = vec![import_edge("mod:x.Y", vec![span()])];
        let mut nodes = Vec::new();
        close_over_endpoints(&mut nodes, &edges, "t0.test", label_for_id, |id, _| {
            id.starts_with("mod:").then(|| Boundary::External {
                reason: "package not declared in this repository".into(),
                evidence: vec![],
            })
        });
        let module = nodes.iter().find(|node| node.id == "mod:x.Y").unwrap();
        assert_eq!(module.props["boundary"], "external");
        assert!(module.props.get("attempted_tiers").is_none());
        assert_eq!(
            provenance(module).confidence_tier,
            ConfidenceTier::Confirmed
        );
        // The src file was also missing: an unresolved Gap, never Confirmed.
        let file = nodes
            .iter()
            .find(|node| node.id == "file:r@a.java")
            .unwrap();
        assert_eq!(provenance(file).confidence_tier, ConfidenceTier::Gap);
    }

    #[test]
    fn a_boundary_without_evidence_fails_closed_to_a_gap() {
        let edges = vec![import_edge("mod:x.Y", vec![])];
        let mut nodes = Vec::new();
        close_over_endpoints(&mut nodes, &edges, "t0.test", label_for_id, |_, _| {
            Some(Boundary::Internal {
                reason: "package declared in this repository".into(),
                evidence: vec![],
            })
        });
        let module = nodes.iter().find(|node| node.id == "mod:x.Y").unwrap();
        assert_eq!(module.props["boundary"], "unresolved");
        assert_eq!(provenance(module).confidence_tier, ConfidenceTier::Gap);
    }

    #[test]
    fn one_unresolved_reference_keeps_a_shared_endpoint_a_gap() {
        // AC-0207 (#237 review): a later unresolved reference is never
        // completed by an earlier proof, whatever the edge order.
        let mut late = import_edge("mod:foo", vec![span()]);
        late.src = "file:r@nested/b.ts".into();
        for edges in [
            vec![import_edge("mod:foo", vec![span()]), late.clone()],
            vec![late.clone(), import_edge("mod:foo", vec![span()])],
        ] {
            let mut nodes = Vec::new();
            close_over_endpoints(&mut nodes, &edges, "t0.test", label_for_id, |id, edge| {
                (id == "mod:foo").then(|| {
                    if edge.src.contains("nested") {
                        Boundary::Unresolved {
                            reason: "tsconfig paths alias with no proven file".into(),
                        }
                    } else {
                        Boundary::External {
                            reason: "package foo not provided by this repository".into(),
                            evidence: vec![],
                        }
                    }
                })
            });
            let module = nodes.iter().find(|node| node.id == "mod:foo").unwrap();
            assert_eq!(module.props["boundary"], "unresolved");
            assert_eq!(
                module.props["reason"],
                "tsconfig paths alias with no proven file"
            );
            assert_eq!(provenance(module).confidence_tier, ConfidenceTier::Gap);
        }
    }
}
