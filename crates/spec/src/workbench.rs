//! Deterministic full-spec bundle for the M9 Spec Workbench (US-0012).

use crate::{TOPOLOGY_EDGE_LABELS, flow_dossier, topology_mermaid};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, Provenance, Tier};
use flowtracer::{Flow, FlowStatus, Hop};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

/// R-INT-5 projection used by every official artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExportMode {
    /// Confirmed and InferredStrong assertions, with every Gap still listed.
    VerifiedOnly,
    /// Verified-only content plus clearly tagged InferredWeak assertions.
    BestEffort,
}

/// One graph-backed assertion shown with complete inline provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecAssertion {
    /// Stable UI/export identity for this occurrence.
    pub id: String,
    /// Node id or `src label dst` edge identity.
    pub subject_id: String,
    /// Graph node label, edge label, or `FlowHop`.
    pub subject_kind: String,
    /// Human-readable assertion without losing its stable identity.
    pub summary: String,
    /// Producing tier, confidence, evidence, extractor, and content hash.
    pub provenance: Provenance,
}

/// One official spec artifact in the deterministic export bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecArtifact {
    /// Stable artifact id used by the Workbench.
    pub id: String,
    /// Portable output file name.
    pub file_name: String,
    /// Display title.
    pub title: String,
    /// `markdown` or `mermaid`.
    pub format: String,
    /// Complete portable artifact text.
    pub content: String,
    /// Every assertion rendered by the artifact, with inline provenance.
    pub assertions: Vec<SpecAssertion>,
}

/// Full official spec export returned to the Workbench.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecBundle {
    /// R-INT-5 projection applied consistently to all artifacts.
    pub mode: ExportMode,
    /// The complete official artifact set in stable order.
    pub artifacts: Vec<SpecArtifact>,
    /// Number of visible graph-backed assertions across artifacts.
    pub assertion_count: usize,
    /// Explicit unresolved assertions in the Gap register.
    pub gap_count: usize,
    /// Explicit ADR/code conflicts in the Drift register.
    pub drift_count: usize,
}

fn fallback_provenance(identity: &str) -> Provenance {
    Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Gap,
        vec![],
        "spec.invalid-provenance",
        identity.as_bytes(),
    )
    .expect("Gap is within the deterministic confidence ceiling")
}

fn provenance(props: &serde_json::Value, identity: &str) -> Provenance {
    serde_json::from_value::<Provenance>(props["prov"].clone())
        .ok()
        .filter(|provenance| provenance.validate().is_ok())
        .unwrap_or_else(|| fallback_provenance(identity))
}

fn is_inferred(confidence: ConfidenceTier) -> bool {
    matches!(
        confidence,
        ConfidenceTier::InferredStrong | ConfidenceTier::InferredWeak
    )
}

fn included(provenance: &Provenance, mode: ExportMode, rejected_hashes: &BTreeSet<String>) -> bool {
    if is_inferred(provenance.confidence_tier) && rejected_hashes.contains(&provenance.content_hash)
    {
        return false;
    }
    match provenance.confidence_tier {
        ConfidenceTier::Confirmed | ConfidenceTier::InferredStrong | ConfidenceTier::Gap => true,
        ConfidenceTier::InferredWeak => mode == ExportMode::BestEffort,
    }
}

fn text_prop<'a>(node: &'a Node, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| node.props[*key].as_str())
}

fn node_name(node: &Node) -> String {
    if node.label == "Endpoint"
        && let (Some(method), Some(path)) =
            (node.props["method"].as_str(), node.props["path"].as_str())
    {
        return format!("{method} {path}");
    }
    text_prop(
        node,
        &[
            "title",
            "name",
            "identity",
            "logical_id",
            "route",
            "path",
            "reason",
        ],
    )
    .map(String::from)
    .unwrap_or_else(|| node.id.clone())
}

fn node_assertion(node: &Node) -> SpecAssertion {
    SpecAssertion {
        id: format!("node:{}", node.id),
        subject_id: node.id.clone(),
        subject_kind: node.label.clone(),
        summary: format!("{}: {}", node.label, node_name(node)),
        provenance: provenance(&node.props, &format!("node:{}", node.id)),
    }
}

fn edge_identity(edge: &Edge) -> String {
    format!("{} {} {}", edge.src, edge.label, edge.dst)
}

fn edge_assertion(edge: &Edge) -> SpecAssertion {
    let identity = edge_identity(edge);
    SpecAssertion {
        id: format!("edge:{identity}"),
        subject_id: identity.clone(),
        subject_kind: edge.label.clone(),
        summary: identity.clone(),
        provenance: provenance(&edge.props, &format!("edge:{identity}")),
    }
}

fn hop_assertion(flow: &Flow, hop: &Hop, index: usize) -> SpecAssertion {
    SpecAssertion {
        id: format!("flow:{}:{index}:{}:{}", flow.trigger, hop.label, hop.dst),
        subject_id: format!("{} {} {}", hop.src, hop.label, hop.dst),
        subject_kind: "FlowHop".into(),
        summary: format!("{}: {} → {}", hop.label, hop.src_name, hop.dst_name),
        provenance: hop.provenance.clone(),
    }
}

fn markdown_safe(value: &str) -> String {
    value.replace('|', "\\|").replace(['\r', '\n'], " ")
}

fn evidence_text(provenance: &Provenance) -> String {
    if provenance.evidence.is_empty() {
        return "—".into();
    }
    provenance
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
}

fn append_assertions(content: &mut String, assertions: &[SpecAssertion]) {
    content.push_str("\n## Assertions and inline provenance\n\n");
    if assertions.is_empty() {
        content.push_str("No graph-backed assertions were recovered for this artifact.\n");
        return;
    }
    content.push_str(
        "| Assertion | Tier | Confidence | Evidence | Extractor | Content hash |\n\
         |---|---|---|---|---|---|\n",
    );
    for assertion in assertions {
        let provenance = &assertion.provenance;
        writeln!(
            content,
            "| {} | {:?} | {:?} | {} | `{}` | `{}` |",
            markdown_safe(&assertion.summary),
            provenance.tier,
            provenance.confidence_tier,
            markdown_safe(&evidence_text(provenance)),
            markdown_safe(&provenance.extractor_id),
            markdown_safe(&provenance.content_hash),
        )
        .expect("write to string");
    }
}

fn artifact(
    id: &str,
    file_name: &str,
    title: &str,
    format: &str,
    mut content: String,
    assertions: Vec<SpecAssertion>,
) -> SpecArtifact {
    append_assertions(&mut content, &assertions);
    SpecArtifact {
        id: id.into(),
        file_name: file_name.into(),
        title: title.into(),
        format: format.into(),
        content,
        assertions,
    }
}

fn filter_nodes<'a>(
    nodes: &'a [Node],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
) -> Vec<&'a Node> {
    let mut selected: Vec<&Node> = nodes
        .iter()
        .filter(|node| included(&provenance(&node.props, &node.id), mode, rejected_hashes))
        .collect();
    selected.sort_by(|left, right| left.id.cmp(&right.id));
    selected
}

fn filter_edges<'a>(
    edges: &'a [Edge],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
) -> Vec<&'a Edge> {
    let mut selected: Vec<&Edge> = edges
        .iter()
        .filter(|edge| {
            included(
                &provenance(&edge.props, &edge_identity(edge)),
                mode,
                rejected_hashes,
            )
        })
        .collect();
    selected.sort_by(|left, right| {
        (&left.src, &left.dst, &left.label).cmp(&(&right.src, &right.dst, &right.label))
    });
    selected
}

fn recovered_user_stories(nodes: &[&Node]) -> (String, Vec<SpecAssertion>) {
    let capabilities: Vec<&Node> = nodes
        .iter()
        .copied()
        .filter(|node| node.label == "Capability")
        .filter(|node| provenance(&node.props, &node.id).confidence_tier != ConfidenceTier::Gap)
        .collect();
    let mut content = String::from("# Recovered user stories\n\n");
    if capabilities.is_empty() {
        content.push_str("No Capability facts have been recovered yet.\n");
    } else {
        for (index, capability) in capabilities.iter().enumerate() {
            writeln!(
                content,
                "## US-R-{:04} — {}\n\n- Recovered capability assertion: `{}`\n",
                index + 1,
                node_name(capability),
                capability.id,
            )
            .expect("write to string");
        }
    }
    let assertions = capabilities.into_iter().map(node_assertion).collect();
    (content, assertions)
}

const TRACE_LABELS: &[&str] = &[
    "REALIZES",
    "STEP_OF",
    "GOVERNS",
    "MAPS_TO",
    "DECIDES",
    "TRIGGERED_BY",
    "PERFORMED_BY",
];

fn traceability_matrix(edges: &[&Edge]) -> (String, Vec<SpecAssertion>) {
    let relevant: Vec<&Edge> = edges
        .iter()
        .copied()
        .filter(|edge| TRACE_LABELS.contains(&edge.label.as_str()))
        .filter(|edge| {
            provenance(&edge.props, &edge_identity(edge)).confidence_tier != ConfidenceTier::Gap
        })
        .collect();
    let mut content = String::from(
        "# Recovered US traceability matrix\n\n| Source | Relation | Target |\n|---|---|---|\n",
    );
    for edge in &relevant {
        writeln!(
            content,
            "| `{}` | {} | `{}` |",
            markdown_safe(&edge.src),
            edge.label,
            markdown_safe(&edge.dst)
        )
        .expect("write to string");
    }
    if relevant.is_empty() {
        content.push_str("| — | No recovered mappings | — |\n");
    }
    let assertions = relevant.into_iter().map(edge_assertion).collect();
    (content, assertions)
}

fn project_flows(
    flows: &[Flow],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
) -> Vec<Flow> {
    let mut projected: Vec<Flow> = flows
        .iter()
        .map(|flow| {
            let hops: Vec<Hop> = flow
                .hops
                .iter()
                .map(|hop| {
                    if included(&hop.provenance, mode, rejected_hashes) {
                        hop.clone()
                    } else {
                        let reason = if rejected_hashes.contains(&hop.provenance.content_hash) {
                            "inference rejected by Workbench curation"
                        } else {
                            "weak inference excluded by verified-only export"
                        };
                        projection_gap(hop, reason)
                    }
                })
                .collect();
            let has_gap = hops
                .iter()
                .any(|hop| hop.provenance.confidence_tier == ConfidenceTier::Gap);
            let all_confirmed = !hops.is_empty()
                && hops
                    .iter()
                    .all(|hop| hop.provenance.confidence_tier == ConfidenceTier::Confirmed);
            let status = if has_gap || hops.is_empty() || flow.depth_limited {
                FlowStatus::Partial
            } else if all_confirmed {
                FlowStatus::Verified
            } else {
                FlowStatus::Inferred
            };
            let score = if hops.is_empty() {
                0.0
            } else {
                hops.iter()
                    .map(|hop| match hop.provenance.confidence_tier {
                        ConfidenceTier::Confirmed => 1.0,
                        ConfidenceTier::InferredStrong => 0.6,
                        ConfidenceTier::InferredWeak => 0.3,
                        ConfidenceTier::Gap => 0.0,
                    })
                    .sum::<f64>()
                    / hops.len() as f64
            };
            Flow {
                trigger: flow.trigger.clone(),
                trigger_kind: flow.trigger_kind.clone(),
                trigger_name: flow.trigger_name.clone(),
                hops,
                status,
                score,
                depth_limited: flow.depth_limited,
            }
        })
        .collect();
    projected.sort_by(|left, right| left.trigger.cmp(&right.trigger));
    projected
}

/// Replace a suppressed inference with a deterministic unresolved hop. This
/// keeps R-INT-4 intact and prevents filtering from upgrading a partial or
/// inferred flow to Verified.
fn projection_gap(hop: &Hop, reason: &str) -> Hop {
    let canonical = serde_json::to_vec(&(
        "projection-gap",
        &hop.src,
        &hop.label,
        &hop.dst,
        &hop.provenance.content_hash,
        reason,
    ))
    .expect("projection Gap identity serializes");
    let provenance = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Gap,
        vec![],
        "spec.projection-gap",
        &canonical,
    )
    .expect("Gap is within the deterministic confidence ceiling");
    Hop {
        label: "UNRESOLVED".into(),
        src: hop.src.clone(),
        dst: format!("gap:projection:{}", &provenance.content_hash[..16]),
        src_name: hop.src_name.clone(),
        dst_name: format!("GAP: {reason}"),
        tier: "Deterministic".into(),
        confidence: "Gap".into(),
        evidence: None,
        provenance,
        gap_reason: Some(reason.into()),
        attempted_tiers: vec![hop.tier.clone()],
    }
}

fn flow_artifact(
    flows: &[Flow],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
) -> (String, Vec<SpecAssertion>) {
    let projected = project_flows(flows, mode, rejected_hashes);
    let assertions = projected
        .iter()
        .flat_map(|flow| {
            flow.hops
                .iter()
                .enumerate()
                .map(|(index, hop)| hop_assertion(flow, hop, index))
        })
        .collect();
    (flow_dossier(&projected), assertions)
}

fn topology_artifact(nodes: &[&Node], edges: &[&Edge]) -> (String, Vec<SpecAssertion>) {
    let mut topology_edges: Vec<Edge> = edges
        .iter()
        .filter(|edge| TOPOLOGY_EDGE_LABELS.contains(&edge.label.as_str()))
        .filter(|edge| {
            provenance(&edge.props, &edge_identity(edge)).confidence_tier != ConfidenceTier::Gap
        })
        .map(|edge| (*edge).clone())
        .collect();
    let backed_channels: BTreeSet<&str> = topology_edges
        .iter()
        .filter(|edge| edge.label == "BACKS")
        .map(|edge| edge.dst.as_str())
        .collect();
    let topology_nodes: Vec<Node> = nodes
        .iter()
        .filter(|node| {
            node.label == "Resource"
                || (node.label == "Channel" && backed_channels.contains(node.id.as_str()))
        })
        .filter(|node| provenance(&node.props, &node.id).confidence_tier != ConfidenceTier::Gap)
        .map(|node| (*node).clone())
        .collect();
    let topology_ids: BTreeSet<&str> = topology_nodes.iter().map(|node| node.id.as_str()).collect();
    topology_edges.retain(|edge| {
        topology_ids.contains(edge.src.as_str()) && topology_ids.contains(edge.dst.as_str())
    });
    let mut assertions: Vec<SpecAssertion> = topology_nodes.iter().map(node_assertion).collect();
    assertions.extend(topology_edges.iter().map(edge_assertion));
    (
        topology_mermaid(&topology_nodes, &topology_edges),
        assertions,
    )
}

const DATA_EDGE_LABELS: &[&str] = &["READS", "WRITES", "MAPS_TO"];

fn data_model(nodes: &[&Node], edges: &[&Edge]) -> (String, Vec<SpecAssertion>) {
    let entities: Vec<&Node> = nodes
        .iter()
        .copied()
        .filter(|node| node.label == "DataEntity")
        .filter(|node| provenance(&node.props, &node.id).confidence_tier != ConfidenceTier::Gap)
        .collect();
    let entity_ids: BTreeSet<&str> = entities.iter().map(|node| node.id.as_str()).collect();
    let mappings: Vec<&Edge> = edges
        .iter()
        .copied()
        .filter(|edge| DATA_EDGE_LABELS.contains(&edge.label.as_str()))
        .filter(|edge| {
            provenance(&edge.props, &edge_identity(edge)).confidence_tier != ConfidenceTier::Gap
        })
        .filter(|edge| {
            entity_ids.contains(edge.src.as_str()) && entity_ids.contains(edge.dst.as_str())
        })
        .collect();
    let mut aliases = BTreeMap::new();
    for (index, entity) in entities.iter().enumerate() {
        aliases.insert(entity.id.as_str(), format!("d{index}"));
    }
    let mut content = String::from("# Recovered data model\n\n```mermaid\nflowchart LR\n");
    for entity in &entities {
        writeln!(
            content,
            "    {}[\"{}\"]",
            aliases[entity.id.as_str()],
            node_name(entity).replace('"', "'")
        )
        .expect("write to string");
    }
    for edge in &mappings {
        if let (Some(source), Some(target)) = (
            aliases.get(edge.src.as_str()),
            aliases.get(edge.dst.as_str()),
        ) {
            writeln!(content, "    {source} -->|{}| {target}", edge.label)
                .expect("write to string");
        }
    }
    content.push_str("```\n");
    if entities.is_empty() {
        content.push_str("\nNo DataEntity facts have been recovered yet.\n");
    }
    let mut assertions: Vec<SpecAssertion> = entities.into_iter().map(node_assertion).collect();
    assertions.extend(mappings.into_iter().map(edge_assertion));
    (content, assertions)
}

fn adr_set(nodes: &[&Node], edges: &[&Edge]) -> (String, Vec<SpecAssertion>) {
    let adrs: Vec<&Node> = nodes
        .iter()
        .copied()
        .filter(|node| node.label == "ADR")
        .filter(|node| provenance(&node.props, &node.id).confidence_tier != ConfidenceTier::Gap)
        .collect();
    let adr_ids: BTreeSet<&str> = adrs.iter().map(|node| node.id.as_str()).collect();
    let decisions: Vec<&Edge> = edges
        .iter()
        .copied()
        .filter(|edge| edge.label == "DECIDES")
        .filter(|edge| {
            provenance(&edge.props, &edge_identity(edge)).confidence_tier != ConfidenceTier::Gap
        })
        .filter(|edge| adr_ids.contains(edge.src.as_str()))
        .collect();
    let mut content = String::from("# Found and recovered ADRs\n\n");
    for adr in &adrs {
        writeln!(content, "## {}\n", node_name(adr)).expect("write to string");
        if let Some(status) = adr.props["status"].as_str() {
            writeln!(content, "**Status:** {status}\n").expect("write to string");
        }
        if let Some(body) = text_prop(adr, &["body", "content", "decision"]) {
            writeln!(content, "{body}\n").expect("write to string");
        }
    }
    if adrs.is_empty() {
        content.push_str("No found or recovered ADR facts have been recovered yet.\n");
    }
    if !decisions.is_empty() {
        content.push_str("\n## Decision links\n\n| ADR | Relation | Subject |\n|---|---|---|\n");
        for edge in &decisions {
            writeln!(
                content,
                "| `{}` | {} | `{}` |",
                markdown_safe(&edge.src),
                edge.label,
                markdown_safe(&edge.dst),
            )
            .expect("write to string");
        }
    }
    let mut assertions: Vec<SpecAssertion> = adrs.into_iter().map(node_assertion).collect();
    assertions.extend(decisions.into_iter().map(edge_assertion));
    (content, assertions)
}

fn gap_register(
    nodes: &[&Node],
    edges: &[&Edge],
    flow_assertions: &[SpecAssertion],
) -> (String, Vec<SpecAssertion>) {
    let mut assertions: Vec<SpecAssertion> = nodes
        .iter()
        .filter(|node| {
            node.label == "Gap"
                || provenance(&node.props, &node.id).confidence_tier == ConfidenceTier::Gap
        })
        .map(|node| node_assertion(node))
        .collect();
    assertions.extend(
        edges
            .iter()
            .filter(|edge| {
                provenance(&edge.props, &edge_identity(edge)).confidence_tier == ConfidenceTier::Gap
            })
            .map(|edge| edge_assertion(edge)),
    );
    assertions.extend(
        flow_assertions
            .iter()
            .filter(|assertion| assertion.provenance.confidence_tier == ConfidenceTier::Gap)
            .cloned(),
    );
    assertions.sort_by(|left, right| left.id.cmp(&right.id));
    assertions.dedup_by(|left, right| left.id == right.id);
    let mut content = String::from("# Gap register\n\n| Subject | Reason |\n|---|---|\n");
    for assertion in &assertions {
        writeln!(
            content,
            "| `{}` | {} |",
            markdown_safe(&assertion.subject_id),
            markdown_safe(&assertion.summary)
        )
        .expect("write to string");
    }
    if assertions.is_empty() {
        content.push_str("| — | No unresolved facts |\n");
    }
    (content, assertions)
}

fn is_drift(node: &Node) -> bool {
    node.label == "Drift" || node.props["kind"].as_str() == Some("drift")
}

fn drift_register(nodes: &[&Node], edges: &[&Edge]) -> (String, Vec<SpecAssertion>) {
    let mut assertions: Vec<SpecAssertion> = nodes
        .iter()
        .filter(|node| is_drift(node))
        .map(|node| node_assertion(node))
        .collect();
    assertions.extend(
        edges
            .iter()
            .filter(|edge| matches!(edge.label.as_str(), "CONFLICTS" | "DRIFTS_FROM"))
            .map(|edge| edge_assertion(edge)),
    );
    let mut content = String::from("# Drift register\n\n| Subject | Conflict |\n|---|---|\n");
    for assertion in &assertions {
        writeln!(
            content,
            "| `{}` | {} |",
            markdown_safe(&assertion.subject_id),
            markdown_safe(&assertion.summary)
        )
        .expect("write to string");
    }
    if assertions.is_empty() {
        content.push_str("| — | No ADR/code conflicts recovered |\n");
    }
    (content, assertions)
}

/// Compile the complete official artifact set with one R-INT-5 policy.
/// Rejected inferred content hashes are suppressed without upgrading any fact.
pub fn compile_spec(
    nodes: &[Node],
    edges: &[Edge],
    flows: &[Flow],
    mode: ExportMode,
    rejected_hashes: &BTreeSet<String>,
) -> SpecBundle {
    let nodes = filter_nodes(nodes, mode, rejected_hashes);
    let edges = filter_edges(edges, mode, rejected_hashes);
    let (stories, story_assertions) = recovered_user_stories(&nodes);
    let (matrix, matrix_assertions) = traceability_matrix(&edges);
    let (dossiers, flow_assertions) = flow_artifact(flows, mode, rejected_hashes);
    let (topology, topology_assertions) = topology_artifact(&nodes, &edges);
    let (data, data_assertions) = data_model(&nodes, &edges);
    let (adrs, adr_assertions) = adr_set(&nodes, &edges);
    let (gaps, gap_assertions) = gap_register(&nodes, &edges, &flow_assertions);
    let (drifts, drift_assertions) = drift_register(&nodes, &edges);

    let gap_count = gap_assertions.len();
    let drift_count = drift_assertions.len();
    let artifacts = vec![
        artifact(
            "user-stories",
            "user_stories.md",
            "User stories",
            "markdown",
            stories,
            story_assertions,
        ),
        artifact(
            "us-tm",
            "US-TM.md",
            "US traceability matrix",
            "markdown",
            matrix,
            matrix_assertions,
        ),
        artifact(
            "flow-dossiers",
            "flow_dossiers.md",
            "Flow dossiers",
            "markdown",
            dossiers,
            flow_assertions,
        ),
        artifact(
            "topology",
            "topology.mmd",
            "Resource topology",
            "mermaid",
            topology,
            topology_assertions,
        ),
        artifact(
            "data-model",
            "data_model.md",
            "Data model",
            "markdown",
            data,
            data_assertions,
        ),
        artifact(
            "adrs",
            "adrs.md",
            "Architecture decisions",
            "markdown",
            adrs,
            adr_assertions,
        ),
        artifact(
            "gap-register",
            "gap_register.md",
            "Gap register",
            "markdown",
            gaps,
            gap_assertions,
        ),
        artifact(
            "drift-register",
            "drift_register.md",
            "Drift register",
            "markdown",
            drifts,
            drift_assertions,
        ),
    ];
    let assertion_count = artifacts
        .iter()
        .map(|artifact| artifact.assertions.len())
        .sum();
    SpecBundle {
        mode,
        artifacts,
        assertion_count,
        gap_count,
        drift_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_prov::EvidenceRef;

    fn prov(tier: Tier, confidence: ConfidenceTier, hash: &str) -> serde_json::Value {
        serde_json::to_value(
            Provenance::new(
                tier,
                confidence,
                vec![EvidenceRef {
                    repo: "local/shop".into(),
                    path: "src/app.ts".into(),
                    byte_start: 10,
                    byte_end: 42,
                    commit_sha: "abc123".into(),
                }],
                "spec.workbench.test",
                hash.as_bytes(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn node(id: &str, label: &str, tier: Tier, confidence: ConfidenceTier) -> Node {
        Node {
            id: id.into(),
            label: label.into(),
            props: serde_json::json!({
                "name": id,
                "reason": if label == "Gap" { "computed identity" } else { "" },
                "prov": prov(tier, confidence, id),
            }),
        }
    }

    fn edge(src: &str, dst: &str, label: &str, tier: Tier, confidence: ConfidenceTier) -> Edge {
        Edge {
            src: src.into(),
            dst: dst.into(),
            label: label.into(),
            props: serde_json::json!({"prov": prov(tier, confidence, &edge_identity(&Edge {
                src: src.into(), dst: dst.into(), label: label.into(), props: serde_json::json!({})
            }))}),
        }
    }

    fn flow_hop(confidence: ConfidenceTier, id: &str) -> Hop {
        let provenance: Provenance = serde_json::from_value(prov(
            match confidence {
                ConfidenceTier::InferredStrong => Tier::Semantic,
                ConfidenceTier::InferredWeak => Tier::Agentic,
                _ => Tier::Deterministic,
            },
            confidence,
            id,
        ))
        .unwrap();
        Hop {
            label: "CALLS".into(),
            src: "sym:a".into(),
            dst: id.into(),
            src_name: "a".into(),
            dst_name: id.into(),
            tier: format!("{:?}", provenance.tier),
            confidence: format!("{:?}", confidence),
            evidence: Some("src/app.ts bytes 10..42".into()),
            provenance,
            gap_reason: (confidence == ConfidenceTier::Gap).then(|| "computed identity".into()),
            attempted_tiers: vec![],
        }
    }

    fn fixture() -> (Vec<Node>, Vec<Edge>, Vec<Flow>) {
        let nodes = vec![
            node(
                "cap:orders",
                "Capability",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "data:orders",
                "DataEntity",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "adr:queue",
                "ADR",
                Tier::Semantic,
                ConfidenceTier::InferredStrong,
            ),
            node(
                "gap:channel",
                "Gap",
                Tier::Deterministic,
                ConfidenceTier::Gap,
            ),
            node(
                "drift:queue",
                "Drift",
                Tier::Semantic,
                ConfidenceTier::InferredStrong,
            ),
            node(
                "weak:rule",
                "BusinessRule",
                Tier::Agentic,
                ConfidenceTier::InferredWeak,
            ),
        ];
        let edges = vec![
            edge(
                "cap:orders",
                "data:orders",
                "MAPS_TO",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            edge(
                "adr:queue",
                "cap:orders",
                "DECIDES",
                Tier::Semantic,
                ConfidenceTier::InferredStrong,
            ),
        ];
        let flows = vec![Flow {
            trigger: "ep:orders".into(),
            trigger_kind: "Endpoint".into(),
            trigger_name: "POST /orders".into(),
            hops: vec![
                flow_hop(ConfidenceTier::Confirmed, "sym:confirmed"),
                flow_hop(ConfidenceTier::InferredWeak, "sym:weak"),
                flow_hop(ConfidenceTier::Gap, "gap:channel"),
            ],
            status: FlowStatus::Partial,
            score: 0.43,
            depth_limited: false,
        }];
        (nodes, edges, flows)
    }

    #[test]
    fn full_bundle_has_every_official_artifact_with_inline_provenance() {
        // AC-0032 / AC-0035 (T-0032, T-0035).
        let (nodes, edges, flows) = fixture();
        let bundle = compile_spec(
            &nodes,
            &edges,
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );
        let names: Vec<&str> = bundle
            .artifacts
            .iter()
            .map(|artifact| artifact.file_name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "user_stories.md",
                "US-TM.md",
                "flow_dossiers.md",
                "topology.mmd",
                "data_model.md",
                "adrs.md",
                "gap_register.md",
                "drift_register.md",
            ]
        );
        for artifact in &bundle.artifacts {
            assert!(
                artifact
                    .content
                    .contains("Assertions and inline provenance")
            );
            for assertion in &artifact.assertions {
                assert!(!assertion.provenance.extractor_id.is_empty());
                assert!(!assertion.provenance.content_hash.is_empty());
            }
        }
        assert_eq!(bundle.gap_count, 2);
        assert_eq!(bundle.drift_count, 1);
    }

    #[test]
    fn verified_only_excludes_weak_but_preserves_gap_and_drift_registers() {
        // AC-0034 / R-INT-5 (T-0034).
        let (nodes, edges, flows) = fixture();
        let verified = compile_spec(
            &nodes,
            &edges,
            &flows,
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
        );
        let dossier = verified
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "flow-dossiers")
            .unwrap();
        assert!(!dossier.content.contains("sym:weak"));
        assert!(dossier.content.contains("gap:channel"));
        assert!(
            dossier
                .content
                .contains("weak inference excluded by verified-only export")
        );
        assert!(!dossier.content.contains("— Verified"));
        assert_eq!(verified.gap_count, 3);
        assert_eq!(verified.drift_count, 1);
    }

    #[test]
    fn rejected_inference_is_suppressed_without_upgrading_other_facts() {
        // AC-0033: curation suppresses by stable content hash.
        let (nodes, edges, flows) = fixture();
        let rejected_hash = flows[0].hops[1].provenance.content_hash.clone();
        let bundle = compile_spec(
            &nodes,
            &edges,
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::from([rejected_hash]),
        );
        let dossier = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "flow-dossiers")
            .unwrap();
        assert!(!dossier.content.contains("sym:weak"));
        assert!(dossier.content.contains("sym:confirmed"));
        assert!(
            dossier
                .content
                .contains("inference rejected by Workbench curation")
        );
    }
}
