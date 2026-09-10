//! Gap-escalation orchestration (#120): from a gap id to strategy cards, an
//! exact egress preview, and a propose-only escalation run. Everything here
//! assembles *inputs* for the bounded T3 broker — no code path can mutate
//! the graph (R-INT-1/R-INT-3): proposals are staged for human curation.

use agents::{AgentCandidate, AgentEvidence, AgentTask};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef};
use serde::Serialize;
use std::collections::BTreeSet;

/// How many hops of graph context feed the candidate set.
const CONTEXT_HOPS: u32 = 2;
/// Broker default bound is 20; stay under it with room for evidence spans.
const MAX_CANDIDATES: usize = 8;

/// One runnable escalation option shown as a strategy card (#113 modal).
#[derive(Debug, Clone, Serialize)]
pub struct StrategyCard {
    /// Stable strategy id: `local-slm` | `cloud-opus`.
    pub id: String,
    /// Ladder tier this strategy runs.
    pub tier: String,
    /// Provider identity that would run it.
    pub provider: String,
    /// `local` | `cloud`.
    pub locality: String,
    /// Bytes that would leave the device (0 for local).
    pub egress_bytes: u64,
    /// Rough cost estimate in USD (None for local — it is free).
    pub est_usd: Option<f64>,
    /// Human latency expectation.
    pub latency: String,
    /// Privacy statement for the card.
    pub privacy: String,
    /// R-INT-5 statement: what accepting the proposal changes in exports.
    pub export_impact: String,
    /// False when policy forbids this strategy right now (fail closed).
    pub available: bool,
    /// Why it is unavailable, when it is.
    pub unavailable_reason: Option<String>,
}

/// Everything the Resolution Strategy modal shows before a run.
#[derive(Debug, Clone, Serialize)]
pub struct GapStrategyReport {
    pub gap_id: String,
    /// Human text of the gap.
    pub summary: String,
    /// Why deterministic recovery stopped.
    pub stop_reason: String,
    /// Tiers that already attempted this slot.
    pub attempted_tiers: Vec<String>,
    /// Evidence citations the escalation would carry (ids from the task).
    pub required_evidence: Vec<String>,
    /// Candidate targets the model would be allowed to choose from.
    pub candidates: usize,
    pub strategies: Vec<StrategyCard>,
}

fn node_name(node: &Node) -> String {
    node.props["name"]
        .as_str()
        .or_else(|| node.props["path"].as_str())
        .unwrap_or(&node.id)
        .to_string()
}

fn stop_reason(node: &Node) -> String {
    node.props["gap_reason"]
        .as_str()
        .or_else(|| node.props["reason"].as_str())
        .unwrap_or("deterministic recovery could not statically resolve this target")
        .to_string()
}

fn attempted_tiers(node: &Node) -> Vec<String> {
    let mut tiers: Vec<String> = node.props["attempted_tiers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect();
    if tiers.is_empty() {
        tiers.push("T0".to_string());
    }
    tiers
}

/// Assemble the bounded T3 task for one gap from the whole-graph projection.
/// `read_span` loads the exact evidence text (a stub in tests); a span that
/// cannot be read is skipped rather than invented.
pub fn assemble_task(
    nodes: &[Node],
    edges: &[Edge],
    gap_id: &str,
    action_id: &str,
    read_span: &dyn Fn(&EvidenceRef) -> Option<String>,
) -> Result<AgentTask, String> {
    let gap = nodes
        .iter()
        .find(|node| node.id == gap_id && spec::is_gap_node(node))
        .ok_or_else(|| format!("no gap named '{gap_id}' in the graph"))?;

    // The unresolved slot: the edge into (or out of) the gap names the
    // source and relation the lower tiers left open.
    let slot = edges
        .iter()
        .find(|edge| edge.dst == gap_id)
        .or_else(|| edges.iter().find(|edge| edge.src == gap_id));
    let source_id = slot
        .map(|edge| {
            if edge.dst == gap_id {
                edge.src.clone()
            } else {
                edge.dst.clone()
            }
        })
        .or_else(|| gap.props["source_id"].as_str().map(str::to_string))
        .ok_or_else(|| format!("gap '{gap_id}' has no adjacent edge naming its source"))?;
    let edge_label = slot
        .map(|edge| edge.label.clone())
        .or_else(|| gap.props["edge_label"].as_str().map(str::to_string))
        .unwrap_or_else(|| "CALLS".to_string());

    // Candidates: nodes within k hops that are real (non-gap) facts with
    // evidence — a closed set; the model can never invent a target.
    let subgraph = semantic::context::khop_subgraph(nodes, edges, gap_id, CONTEXT_HOPS);
    let in_context: BTreeSet<&str> = subgraph
        .nodes
        .iter()
        .map(|(_, node)| node.id.as_str())
        .collect();

    let mut evidence: Vec<AgentEvidence> = Vec::new();
    let mut next_evidence_id = 0usize;
    let mut cite = |reference: &EvidenceRef, text: String| -> String {
        next_evidence_id += 1;
        let id = format!("E{next_evidence_id}");
        evidence.push(AgentEvidence {
            id: id.clone(),
            source: reference.clone(),
            text,
        });
        id
    };

    let mut source_evidence_ids = Vec::new();
    if let Some(source_node) = nodes.iter().find(|node| node.id == source_id) {
        let provenance = spec::provenance(&source_node.props, &source_node.id);
        if let Some(reference) = provenance.evidence.first()
            && let Some(text) = read_span(reference)
        {
            source_evidence_ids.push(cite(reference, text));
        }
    }
    // The gap's own evidence anchors why recovery stopped.
    let gap_provenance = spec::provenance(&gap.props, &gap.id);
    if let Some(reference) = gap_provenance.evidence.first()
        && let Some(text) = read_span(reference)
    {
        source_evidence_ids.push(cite(reference, text));
    }
    if source_evidence_ids.is_empty() {
        return Err(format!(
            "gap '{gap_id}' has no readable evidence — escalation would have nothing to cite"
        ));
    }

    let mut candidates = Vec::new();
    for node in nodes {
        if candidates.len() >= MAX_CANDIDATES {
            break;
        }
        if node.id == gap_id || node.id == source_id || !in_context.contains(node.id.as_str()) {
            continue;
        }
        if spec::is_gap_node(node) {
            continue;
        }
        let provenance = spec::provenance(&node.props, &node.id);
        if provenance.confidence_tier == ConfidenceTier::Gap {
            continue;
        }
        let Some(reference) = provenance.evidence.first() else {
            continue;
        };
        let Some(text) = read_span(reference) else {
            continue;
        };
        let evidence_id = cite(reference, text);
        candidates.push(AgentCandidate {
            node_id: node.id.clone(),
            label: node.label.clone(),
            summary: format!("{}: {}", node.label, node_name(node)),
            evidence_ids: vec![evidence_id],
        });
    }
    if candidates.is_empty() {
        return Err(format!(
            "gap '{gap_id}' has no evidence-backed candidates within {CONTEXT_HOPS} hops"
        ));
    }

    Ok(AgentTask {
        action_id: action_id.to_string(),
        gap_id: gap_id.to_string(),
        source_id,
        edge_label,
        existing_confidence: ConfidenceTier::Gap,
        source_evidence_ids,
        evidence,
        candidates,
    })
}

/// Rough token estimate for cost cards: bytes / 4 is the industry shorthand.
fn est_usd(payload_bytes: u64, input_per_mtok: f64, output_per_mtok: f64) -> f64 {
    let input_tokens = payload_bytes as f64 / 4.0;
    // A bounded proposal is small; budget 500 output tokens.
    (input_tokens / 1_000_000.0) * input_per_mtok + (500.0 / 1_000_000.0) * output_per_mtok
}

/// Derive the strategy cards for one assembled task. `cloud_allowed` comes
/// from the persisted settings policy (fail closed); `payload_bytes` is the
/// exact redacted payload size from the firewall preview.
pub fn strategies(
    task: &AgentTask,
    gap: &Node,
    cloud_allowed: bool,
    payload_bytes: u64,
) -> GapStrategyReport {
    let export_impact = "Review decisions are saved with the staged proposal. Accepted proposals \
                         await shared-context reconciliation; they do not yet change exports. \
                         Working-tree evidence is unverified against its cited revision. \
                         Proposals retain T3/InferredWeak and never modify T0/T1 facts."
        .to_string();
    let disclosure = llm::anthropic::disclosure(llm::anthropic::ClaudeLane::Opus);
    let strategies = vec![
        StrategyCard {
            id: "local-slm".into(),
            tier: "T3".into(),
            provider: format!(
                "ollama:{}",
                llm::catalog::model_for(llm::catalog::ModelAction::Proposal).model
            ),
            locality: "local".into(),
            egress_bytes: 0,
            est_usd: None,
            latency: "seconds to a minute on-device".into(),
            privacy: "payload never leaves the device".into(),
            export_impact: export_impact.clone(),
            available: true,
            unavailable_reason: None,
        },
        StrategyCard {
            id: "cloud-opus".into(),
            tier: "T3".into(),
            provider: format!("{} · {}", disclosure.provider, disclosure.model),
            locality: "cloud".into(),
            egress_bytes: payload_bytes,
            est_usd: Some(est_usd(
                payload_bytes,
                disclosure.input_usd_per_mtok,
                disclosure.output_usd_per_mtok,
            )),
            latency: "a few seconds via API".into(),
            privacy: "redacted payload leaves the device after a per-payload grant".into(),
            export_impact,
            available: cloud_allowed,
            unavailable_reason: (!cloud_allowed).then(|| {
                "T3 is not consented to cloud — enable the provider and grant consent in \
                 Settings (cloud fails closed)"
                    .to_string()
            }),
        },
    ];
    GapStrategyReport {
        gap_id: task.gap_id.clone(),
        summary: node_name(gap),
        stop_reason: stop_reason(gap),
        attempted_tiers: attempted_tiers(gap),
        required_evidence: task
            .evidence
            .iter()
            .map(|evidence| {
                format!(
                    "{} · {}:{}",
                    evidence.id, evidence.source.repo, evidence.source.path
                )
            })
            .collect(),
        candidates: task.candidates.len(),
        strategies,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prov(confidence: &str, path: &str) -> serde_json::Value {
        json!({
            "tier": "Deterministic",
            "confidence_tier": confidence,
            "evidence": [{
                "repo": "local/fixture",
                "path": path,
                "byte_start": 0,
                "byte_end": 24,
                "commit_sha": "workdir",
            }],
            "extractor_id": "t0.adapter-ts",
            "content_hash": "a".repeat(64),
        })
    }

    fn node(id: &str, label: &str, confidence: &str, path: &str) -> Node {
        Node {
            id: id.into(),
            label: label.into(),
            props: json!({"name": id, "prov": prov(confidence, path)}),
        }
    }

    fn edge(src: &str, dst: &str, label: &str) -> Edge {
        Edge {
            src: src.into(),
            dst: dst.into(),
            label: label.into(),
            props: json!({"prov": prov("Confirmed", "src/a.ts")}),
        }
    }

    fn fixture() -> (Vec<Node>, Vec<Edge>) {
        let mut gap = node("gap:sync", "Gap", "Gap", "src/background.ts");
        gap.props["gap_reason"] = json!("endpoint host computed from config at runtime");
        let nodes = vec![
            node("sym:capture", "Symbol", "Confirmed", "src/capture.ts"),
            gap,
            node("ch:events", "Channel", "Confirmed", "src/events.ts"),
            node("res:queue", "Resource", "Confirmed", "infra/queue.tf"),
            node("gap:other", "Gap", "Gap", "src/other.ts"),
        ];
        let edges = vec![
            edge("sym:capture", "gap:sync", "CALLS"),
            edge("gap:sync", "ch:events", "REFERENCES"),
            edge("ch:events", "res:queue", "BACKS"),
        ];
        (nodes, edges)
    }

    fn read_span(reference: &EvidenceRef) -> Option<String> {
        Some(format!("// span from {}", reference.path))
    }

    #[test]
    fn task_derives_slot_candidates_and_citations_from_the_graph() {
        let (nodes, edges) = fixture();
        let task = assemble_task(&nodes, &edges, "gap:sync", "escalate:test", &read_span).unwrap();

        assert_eq!(task.source_id, "sym:capture");
        assert_eq!(task.edge_label, "CALLS");
        assert_eq!(task.existing_confidence, ConfidenceTier::Gap);
        // Candidates are the evidence-backed non-gap context — never the gap
        // itself, the source, or another gap.
        let ids: Vec<&str> = task.candidates.iter().map(|c| c.node_id.as_str()).collect();
        assert_eq!(ids, vec!["ch:events", "res:queue"]);
        // Every candidate and the source carry real citations.
        assert!(!task.source_evidence_ids.is_empty());
        assert!(task.candidates.iter().all(|c| !c.evidence_ids.is_empty()));
        // The task passes the broker's own validation.
        agents::AgentBroker::bounded_default()
            .preview(
                &NoopLocal,
                &llm::EgressFirewall::new(llm::EgressPolicy::default()),
                &task,
            )
            .unwrap();
    }

    #[test]
    fn unreadable_evidence_is_skipped_never_invented() {
        let (nodes, edges) = fixture();
        let none = |_: &EvidenceRef| -> Option<String> { None };
        let error = assemble_task(&nodes, &edges, "gap:sync", "escalate:test", &none).unwrap_err();
        assert!(error.contains("nothing to cite"), "{error}");
    }

    #[test]
    fn strategy_cards_fail_closed_without_cloud_consent() {
        let (nodes, edges) = fixture();
        let task = assemble_task(&nodes, &edges, "gap:sync", "escalate:test", &read_span).unwrap();
        let gap = nodes.iter().find(|n| n.id == "gap:sync").unwrap();

        let report = strategies(&task, gap, false, 2048);
        assert_eq!(
            report.stop_reason,
            "endpoint host computed from config at runtime"
        );
        assert_eq!(report.attempted_tiers, vec!["T0"]);
        let cloud = report
            .strategies
            .iter()
            .find(|s| s.id == "cloud-opus")
            .unwrap();
        assert!(!cloud.available);
        assert!(
            cloud
                .unavailable_reason
                .as_deref()
                .unwrap()
                .contains("fail")
        );
        let local = report
            .strategies
            .iter()
            .find(|s| s.id == "local-slm")
            .unwrap();
        assert!(local.available);
        assert_eq!(local.egress_bytes, 0);

        // With standing consent the cloud card opens and carries estimates.
        let open = strategies(&task, gap, true, 2048);
        let cloud = open
            .strategies
            .iter()
            .find(|s| s.id == "cloud-opus")
            .unwrap();
        assert!(cloud.available);
        assert_eq!(cloud.egress_bytes, 2048);
        assert!(cloud.est_usd.unwrap() > 0.0);
    }

    /// Minimal local provider so broker validation can run without a model.
    struct NoopLocal;
    impl llm::LlmProvider for NoopLocal {
        fn id(&self) -> &str {
            "noop:local"
        }
        fn locality(&self) -> llm::Locality {
            llm::Locality::Local
        }
        fn capabilities(&self) -> llm::ProviderCaps {
            llm::ProviderCaps {
                chat: true,
                embeddings: false,
                tool_use: false,
            }
        }
        fn embed(&self, _batch: &[String]) -> Result<Vec<llm::Embedding>, llm::ProviderError> {
            Err(llm::ProviderError::Unsupported("test provider"))
        }
    }
}
