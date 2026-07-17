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
    /// Explicit auth/IAM findings in the security view.
    pub security_count: usize,
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

/// The validated provenance of a fact, or the explicit-Gap fallback when a
/// fact carries none. Public for the same reason as the register predicates
/// (#116): tier tallies must count with one definition on every surface.
pub fn provenance(props: &serde_json::Value, identity: &str) -> Provenance {
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
    let diagram = topology_mermaid(&topology_nodes, &topology_edges);
    let content = format!("# Resource topology\n\n```mermaid\n{diagram}```\n");
    (content, assertions)
}

const DATA_EDGE_LABELS: &[&str] = &["READS", "WRITES", "MAPS_TO"];

fn data_model(nodes: &[&Node], edges: &[&Edge]) -> (String, Vec<SpecAssertion>) {
    let by_id: BTreeMap<&str, &Node> = nodes.iter().map(|node| (node.id.as_str(), *node)).collect();
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
            entity_ids.contains(edge.src.as_str()) || entity_ids.contains(edge.dst.as_str())
        })
        .filter(|edge| {
            by_id.contains_key(edge.src.as_str()) && by_id.contains_key(edge.dst.as_str())
        })
        .collect();
    let model_ids: BTreeSet<&str> = entity_ids
        .iter()
        .copied()
        .chain(
            mappings
                .iter()
                .flat_map(|edge| [edge.src.as_str(), edge.dst.as_str()]),
        )
        .collect();
    let model_nodes: Vec<&Node> = model_ids
        .iter()
        .filter_map(|id| by_id.get(id).copied())
        .collect();
    let mut aliases = BTreeMap::new();
    for (index, node) in model_nodes.iter().enumerate() {
        aliases.insert(node.id.as_str(), format!("d{index}"));
    }
    let mut content = String::from("# Recovered data model\n\n```mermaid\nflowchart LR\n");
    for node in &model_nodes {
        let display = if node.label == "DataEntity" {
            node_name(node)
        } else {
            format!("{}: {}", node.label, node_name(node))
        };
        writeln!(
            content,
            "    {}[\"{}\"]",
            aliases[node.id.as_str()],
            display.replace(['\r', '\n'], " ").replace('"', "'")
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
    if !mappings.is_empty() {
        content.push_str(
            "\n## Access and mapping relations\n\n| Source | Relation | Target |\n|---|---|---|\n",
        );
        for edge in &mappings {
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
    let mut assertions: Vec<SpecAssertion> = model_nodes.into_iter().map(node_assertion).collect();
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
        if let Some(origin) = adr.props["origin"].as_str() {
            writeln!(content, "**Origin:** {origin}\n").expect("write to string");
        }
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

/// True when `node` is an explicit System Gap. Public so the shell's
/// findings summary counts with the register's own definition (#116) —
/// every surface must reconcile from one predicate.
pub fn is_gap_node(node: &Node) -> bool {
    node.label == "Gap" || provenance(&node.props, &node.id).confidence_tier == ConfidenceTier::Gap
}

/// True when `edge` is an explicit Gap relation (see [`is_gap_node`]).
pub fn is_gap_edge(edge: &Edge) -> bool {
    provenance(&edge.props, &edge_identity(edge)).confidence_tier == ConfidenceTier::Gap
}

/// True when `edge` records ADR/code drift (see [`is_drift_node`]).
pub fn is_drift_edge(edge: &Edge) -> bool {
    matches!(edge.label.as_str(), "CONFLICTS" | "DRIFTS_FROM")
}

fn gap_register(
    nodes: &[&Node],
    edges: &[&Edge],
    flow_assertions: &[SpecAssertion],
) -> (String, Vec<SpecAssertion>) {
    let mut assertions: Vec<SpecAssertion> = nodes
        .iter()
        .filter(|node| is_gap_node(node))
        .map(|node| node_assertion(node))
        .collect();
    assertions.extend(
        edges
            .iter()
            .filter(|edge| is_gap_edge(edge))
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

/// True when `node` records ADR/code drift (see [`is_gap_node`] for why
/// these predicates are public).
pub fn is_drift_node(node: &Node) -> bool {
    node.label == "Drift" || node.props["kind"].as_str() == Some("drift")
}

fn drift_register(nodes: &[&Node], edges: &[&Edge]) -> (String, Vec<SpecAssertion>, usize) {
    let drift_nodes: Vec<&&Node> = nodes.iter().filter(|node| is_drift_node(node)).collect();
    let mut assertions: Vec<SpecAssertion> = drift_nodes
        .iter()
        .map(|node| node_assertion(node))
        .collect();
    assertions.extend(
        edges
            .iter()
            .filter(|edge| is_drift_edge(edge))
            .map(|edge| edge_assertion(edge)),
    );
    let mut content = String::from(
        "# Drift register\n\n| Finding | ADR | Offending edge | Flow triggers | Confidence |\n|---|---|---|---|---|\n",
    );
    for node in &drift_nodes {
        let triggers = node.props["flow_triggers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let node_provenance = provenance(&node.props, &node.id);
        writeln!(
            content,
            "| {} | `{}` | `{}` | {} | {:?} |",
            markdown_safe(&node_name(node)),
            markdown_safe(node.props["adr_id"].as_str().unwrap_or("—")),
            markdown_safe(node.props["offending_edge"].as_str().unwrap_or("—")),
            markdown_safe(if triggers.is_empty() {
                "—"
            } else {
                &triggers
            }),
            node_provenance.confidence_tier,
        )
        .expect("write to string");
    }
    if drift_nodes.is_empty() {
        content.push_str("| — | No ADR/code conflicts recovered |\n");
    }
    (content, assertions, drift_nodes.len())
}

fn security_view(nodes: &[&Node]) -> (String, Vec<SpecAssertion>, usize) {
    let findings = nodes
        .iter()
        .filter(|node| node.label == "Finding" && node.props["kind"].as_str() == Some("security"))
        .collect::<Vec<_>>();
    let assertions = findings
        .iter()
        .map(|finding| node_assertion(finding))
        .collect::<Vec<_>>();
    let mut content = String::from(
        "# Security findings\n\n| Finding | Type | Subject | Resource scope | Actions | US / AC | Confidence |\n|---|---|---|---|---|---|---|\n",
    );
    for finding in &findings {
        let scopes = finding.props["resource_scope"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let actions = finding.props["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let finding_provenance = provenance(&finding.props, &finding.id);
        writeln!(
            content,
            "| {} | `{}` | `{}` | {} | {} | {} / {} | {:?} |",
            markdown_safe(&node_name(finding)),
            markdown_safe(finding.props["category"].as_str().unwrap_or("security")),
            markdown_safe(finding.props["subject_id"].as_str().unwrap_or("—")),
            markdown_safe(if scopes.is_empty() { "—" } else { &scopes }),
            markdown_safe(if actions.is_empty() { "—" } else { &actions }),
            markdown_safe(finding.props["us_id"].as_str().unwrap_or("US-0015")),
            markdown_safe(finding.props["ac_id"].as_str().unwrap_or("—")),
            finding_provenance.confidence_tier,
        )
        .expect("write to string");
    }
    if findings.is_empty() {
        content.push_str("| — | — | — | — | — | No explicit security findings | — |\n");
    }
    (content, assertions, findings.len())
}

/// The toolchain view (#215): what the system is built *with*, as cited
/// facts — `Tool` nodes with their resolved settings and the config files
/// (`DEFINED_IN`) that prove them.
fn toolchain_view(nodes: &[&Node], edges: &[&Edge]) -> (String, Vec<SpecAssertion>) {
    let mut content = String::from(
        "# Toolchain\n\n| Tool | Category | Defined in | Settings | Confidence |\n|---|---|---|---|---|\n",
    );
    let mut assertions = Vec::new();
    let tools: Vec<&&Node> = nodes.iter().filter(|node| node.label == "Tool").collect();
    for tool in &tools {
        let tool_provenance = provenance(&tool.props, &tool.id);
        let display = tool.props["display"].as_str().unwrap_or(&tool.id);
        let category = tool.props["category"].as_str().unwrap_or("—");
        let mut defined_in: Vec<String> = edges
            .iter()
            .filter(|edge| edge.label == "DEFINED_IN" && edge.src == tool.id)
            .map(|edge| {
                edge.dst
                    .split_once('@')
                    .map(|(_, path)| path.to_string())
                    .unwrap_or_else(|| edge.dst.clone())
            })
            .collect();
        defined_in.sort();
        defined_in.dedup();
        let settings = if tool.props["settings_behind_code"].as_bool() == Some(true) {
            "settings live in code — detected by presence only".to_string()
        } else {
            let rendered: Vec<String> = tool.props["settings"]
                .as_object()
                .map(|settings| {
                    settings
                        .iter()
                        .map(|(key, value)| format!("{key}={value}"))
                        .collect()
                })
                .unwrap_or_default();
            if rendered.is_empty() {
                "—".to_string()
            } else {
                rendered.join("; ")
            }
        };
        writeln!(
            content,
            "| {} | {} | {} | {} | {:?} |",
            markdown_safe(display),
            markdown_safe(category),
            markdown_safe(&defined_in.join(", ")),
            markdown_safe(&settings),
            tool_provenance.confidence_tier,
        )
        .expect("write to string");
        assertions.push(SpecAssertion {
            id: format!("tool:{}", tool.id),
            subject_id: tool.id.clone(),
            subject_kind: "Tool".into(),
            summary: format!(
                "Toolchain: {display} ({category}) defined in {}",
                defined_in.join(", ")
            ),
            provenance: tool_provenance,
        });
    }
    if tools.is_empty() {
        content.push_str("| — | — | — | No toolchain facts recovered | — |\n");
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
    // Curation is part of the compilation projection, so derived ADRs may
    // only consume facts that survived the selected export policy. Otherwise
    // a rejected support edge could disappear while a new derived hash kept
    // its conclusion visible.
    let base_nodes = filter_nodes(nodes, mode, rejected_hashes)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    let base_edges = filter_edges(edges, mode, rejected_hashes)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    let projected_flows = project_flows(flows, mode, rejected_hashes);
    let derived = crate::derive_adr_facts(&base_nodes, &base_edges, &projected_flows);
    let security_findings = crate::security::derive_security_findings(&base_nodes, &base_edges);
    let mut projected_nodes = base_nodes;
    projected_nodes.extend(derived.nodes);
    projected_nodes.extend(security_findings);
    let mut projected_edges = base_edges;
    projected_edges.extend(derived.edges);
    let nodes = filter_nodes(&projected_nodes, mode, rejected_hashes);
    let edges = filter_edges(&projected_edges, mode, rejected_hashes);
    let (stories, story_assertions) = recovered_user_stories(&nodes);
    let (matrix, matrix_assertions) = traceability_matrix(&edges);
    let (dossiers, flow_assertions) = flow_artifact(flows, mode, rejected_hashes);
    let (topology, topology_assertions) = topology_artifact(&nodes, &edges);
    let (data, data_assertions) = data_model(&nodes, &edges);
    let (adrs, adr_assertions) = adr_set(&nodes, &edges);
    let (gaps, gap_assertions) = gap_register(&nodes, &edges, &flow_assertions);
    let (drifts, drift_assertions, drift_count) = drift_register(&nodes, &edges);
    let (security, security_assertions, security_count) = security_view(&nodes);
    let (toolchain, toolchain_assertions) = toolchain_view(&nodes, &edges);

    let gap_count = gap_assertions.len();
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
            "topology.md",
            "Resource topology",
            "markdown",
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
        artifact(
            "security-view",
            "security.md",
            "Security findings",
            "markdown",
            security,
            security_assertions,
        ),
        artifact(
            "toolchain",
            "toolchain.md",
            "Toolchain",
            "markdown",
            toolchain,
            toolchain_assertions,
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
        security_count,
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
                "domain:order",
                "DomainEntity",
                Tier::Semantic,
                ConfidenceTier::InferredStrong,
            ),
            node(
                "sym:save-order",
                "Symbol",
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
                "domain:order",
                "data:orders",
                "MAPS_TO",
                Tier::Semantic,
                ConfidenceTier::InferredStrong,
            ),
            edge(
                "sym:save-order",
                "data:orders",
                "WRITES",
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
                "topology.md",
                "data_model.md",
                "adrs.md",
                "gap_register.md",
                "drift_register.md",
                "security.md",
                "toolchain.md",
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
        let topology = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "topology")
            .unwrap();
        assert_eq!(topology.format, "markdown");
        assert!(topology.content.contains("```mermaid\nflowchart LR"));
        assert!(
            topology
                .content
                .contains("Assertions and inline provenance")
        );

        let data_model = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "data-model")
            .unwrap();
        assert!(data_model.content.contains("domain:order"));
        assert!(data_model.content.contains("sym:save-order"));
        assert!(data_model.content.contains("MAPS_TO"));
        assert!(data_model.content.contains("WRITES"));
        assert_eq!(bundle.gap_count, 2);
        assert_eq!(bundle.drift_count, 1);
        assert_eq!(bundle.security_count, 0);
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
        assert_eq!(verified.security_count, 0);
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

    #[test]
    fn compile_derives_inferred_adr_and_flow_mapped_drift() {
        // AC-0037 / AC-0038 (T-0037, T-0038): compilation keeps recovered
        // decisions inferred and maps explicit conflicts to edge and flow.
        let nodes = vec![
            node(
                "chan:orders",
                "Channel",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "sym:publish",
                "Symbol",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "sym:handler",
                "Symbol",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "sym:remote",
                "Symbol",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            Node {
                id: "adr:found:no-sync".into(),
                label: "ADR".into(),
                props: serde_json::json!({
                    "title": "No synchronous calls",
                    "status": "Accepted",
                    "origin": "found",
                    "forbids": ["CALLS"],
                    "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "found-adr"),
                }),
            },
        ];
        let edges = vec![
            edge(
                "sym:publish",
                "chan:orders",
                "PUBLISHES",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            edge(
                "adr:found:no-sync",
                "sym:handler",
                "DECIDES",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            edge(
                "sym:handler",
                "sym:remote",
                "CALLS",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
        ];
        let mut call = flow_hop(ConfidenceTier::Confirmed, "sym:remote");
        call.src = "sym:handler".into();
        call.src_name = "handler".into();
        let flows = vec![Flow {
            trigger: "ep:orders".into(),
            trigger_kind: "Endpoint".into(),
            trigger_name: "POST /orders".into(),
            hops: vec![call],
            status: FlowStatus::Verified,
            score: 1.0,
            depth_limited: false,
        }];
        let bundle = compile_spec(
            &nodes,
            &edges,
            &flows,
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );
        let adrs = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "adrs")
            .unwrap();
        let recovered = adrs
            .assertions
            .iter()
            .find(|assertion| assertion.subject_id.starts_with("adr:recovered:"))
            .unwrap();
        assert_eq!(recovered.provenance.tier, Tier::Semantic);
        assert_eq!(
            recovered.provenance.confidence_tier,
            ConfidenceTier::InferredStrong
        );
        let drift = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "drift-register")
            .unwrap();
        assert!(drift.content.contains("sym:handler CALLS sym:remote"));
        assert!(drift.content.contains("ep:orders"));
        assert_eq!(bundle.drift_count, 1);
    }

    #[test]
    fn rejected_support_cannot_derive_an_adr_or_drift_finding() {
        // AC-0037/AC-0038 (T-0037/T-0038): Workbench rejection is applied
        // before derivation, so a new derived hash cannot bypass curation.
        let nodes = vec![
            node(
                "chan:orders",
                "Channel",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "sym:publish",
                "Symbol",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "sym:handler",
                "Symbol",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "sym:remote",
                "Symbol",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            Node {
                id: "adr:found:no-sync".into(),
                label: "ADR".into(),
                props: serde_json::json!({
                    "title": "No synchronous calls",
                    "origin": "found",
                    "forbids": ["CALLS"],
                    "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "found-adr"),
                }),
            },
        ];
        let publish = edge(
            "sym:publish",
            "chan:orders",
            "PUBLISHES",
            Tier::Agentic,
            ConfidenceTier::InferredWeak,
        );
        let decides = edge(
            "adr:found:no-sync",
            "sym:handler",
            "DECIDES",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        let call = edge(
            "sym:handler",
            "sym:remote",
            "CALLS",
            Tier::Agentic,
            ConfidenceTier::InferredWeak,
        );
        let rejected_hashes = BTreeSet::from([
            provenance(&publish.props, "publish").content_hash,
            provenance(&call.props, "call").content_hash,
        ]);

        let bundle = compile_spec(
            &nodes,
            &[publish, decides, call],
            &[],
            ExportMode::BestEffort,
            &rejected_hashes,
        );
        let adrs = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "adrs")
            .unwrap();
        assert!(
            adrs.assertions
                .iter()
                .all(|assertion| !assertion.subject_id.starts_with("adr:recovered:"))
        );
        assert_eq!(bundle.drift_count, 0);
    }

    #[test]
    fn security_view_maps_findings_and_honors_support_curation() {
        // AC-0041/AC-0042 (T-0041/T-0042): explicit endpoint auth and IAM
        // wildcard support become mapped findings without bypassing R-INT-5.
        let endpoint = Node {
            id: "ep:admin".into(),
            label: "Endpoint".into(),
            props: serde_json::json!({
                "method": "GET",
                "path": "/admin",
                "authenticated": false,
                "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "admin-endpoint"),
            }),
        };
        let policy = node(
            "res:admin-policy",
            "Resource",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        let bucket = node(
            "res:orders",
            "Resource",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        let grant = Edge {
            src: policy.id.clone(),
            dst: bucket.id.clone(),
            label: "GRANTS".into(),
            props: serde_json::json!({
                "actions": ["s3:Get*"],
                "resource_scopes": ["arn:aws:s3:::orders/*"],
                "prov": prov(Tier::Agentic, ConfidenceTier::InferredWeak, "wildcard-grant"),
            }),
        };
        let grant_hash = provenance(&grant.props, "grant").content_hash;

        let bundle = compile_spec(
            &[endpoint.clone(), policy.clone(), bucket.clone()],
            std::slice::from_ref(&grant),
            &[],
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );
        let security = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "security-view")
            .unwrap();
        assert_eq!(bundle.security_count, 2);
        assert!(
            security
                .content
                .contains("Unauthenticated endpoint: GET /admin")
        );
        assert!(security.content.contains("arn:aws:s3:::orders/*"));
        assert!(security.content.contains("US-0015 / AC-0041"));
        assert!(security.content.contains("US-0015 / AC-0042"));
        let grant_finding = security
            .assertions
            .iter()
            .find(|assertion| assertion.summary.contains("Over-broad IAM grant"))
            .unwrap();
        assert_eq!(
            grant_finding.provenance.confidence_tier,
            ConfidenceTier::InferredWeak
        );

        let curated = compile_spec(
            &[endpoint, policy, bucket],
            &[grant],
            &[],
            ExportMode::BestEffort,
            &BTreeSet::from([grant_hash]),
        );
        let curated_security = curated
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "security-view")
            .unwrap();
        assert_eq!(curated.security_count, 1);
        assert!(!curated_security.content.contains("s3:Get*"));
    }

    #[test]
    fn toolchain_artifact_states_what_the_system_is_built_with() {
        // #215 (AC-0096): the spec export gains a toolchain section fed by
        // the same Tool nodes the graph holds — settings cited, config file
        // named, presence-only code configs honestly marked.
        let react = Node {
            id: "tool:local/shop@react".into(),
            label: "Tool".into(),
            props: serde_json::json!({
                "name": "react",
                "display": "React",
                "category": "framework",
                "settings": { "requirement": "^19.0.0" },
                "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "react"),
            }),
        };
        let vite = Node {
            id: "tool:local/shop@vite.config.ts".into(),
            label: "Tool".into(),
            props: serde_json::json!({
                "name": "vite.config.ts",
                "display": "Vite config",
                "category": "bundler",
                "settings": {},
                "settings_behind_code": true,
                "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "vite"),
            }),
        };
        let manifest = Node {
            id: "file:local/shop@package.json".into(),
            label: "File".into(),
            props: serde_json::json!({
                "path": "package.json",
                "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "manifest"),
            }),
        };
        let defined_in = Edge {
            src: react.id.clone(),
            dst: manifest.id.clone(),
            label: "DEFINED_IN".into(),
            props: serde_json::json!({
                "prov": prov(Tier::Deterministic, ConfidenceTier::Confirmed, "defined"),
            }),
        };
        let bundle = compile_spec(
            &[react, vite, manifest],
            &[defined_in],
            &[],
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
        );
        let toolchain = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "toolchain")
            .unwrap();
        assert!(
            toolchain
                .content
                .contains("| React | framework | package.json |")
        );
        assert!(toolchain.content.contains(r#"requirement="^19.0.0""#));
        assert!(
            toolchain
                .content
                .contains("settings live in code — detected by presence only")
        );
        let assertion = toolchain
            .assertions
            .iter()
            .find(|assertion| assertion.subject_id == "tool:local/shop@react")
            .unwrap();
        assert_eq!(assertion.subject_kind, "Tool");
        assert_eq!(
            assertion.provenance.confidence_tier,
            ConfidenceTier::Confirmed
        );
        // An empty graph renders the honest empty row instead.
        let empty = compile_spec(&[], &[], &[], ExportMode::VerifiedOnly, &BTreeSet::new());
        assert!(
            empty
                .artifacts
                .iter()
                .find(|artifact| artifact.id == "toolchain")
                .unwrap()
                .content
                .contains("No toolchain facts recovered")
        );
    }
}
