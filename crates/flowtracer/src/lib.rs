//! Cross-layer flow tracer (SPEC-00 §5, US-0006) — the deterministic (T0)
//! path engine.
//!
//! A flow starts at a trigger — a `Screen` (M4, the user-action anchor),
//! an extension user-action anchor (`ExtensionContext` or manifest
//! `Command`, #165), an `Endpoint` nothing local fetches, or a `Channel`
//! no local producer publishes to — and walks the graph hop by hop:
//! `RENDERS` into components, `FETCHES` into endpoints, `HANDLES` into
//! the handler symbol or context, `ENTRY` into a context's entry file,
//! `DEFINED_IN` in reverse from a file into the symbols it defines,
//! `CALLS` through the call graph, `PUBLISHES` onto channels,
//! `SUBSCRIBES` out to consumers. Each hop records the tier and confidence
//! of the edge that resolved it (AC-0015). A hop into a `Gap` node
//! truncates that branch — the flow is emitted partial, never silently
//! completed (AC-0016, R-INT-4). A flow node that `DEPENDS_ON` an
//! execution Gap (code it runs that T0 could not recover, such as an
//! unproven `eval()`) contributes that Gap as a hop too (AC-0219,
//! ADR-0032). Completed traces score per §5.3
//! (AC-0017). At M3 every resolvable hop is T0; the T1–T3 rungs join the
//! ladder at M6–M8.

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, Provenance, Tier};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

/// Edge labels the tracer walks — callers use this to query exactly the
/// edges a trace needs.
pub const FLOW_EDGE_LABELS: &[&str] = &[
    "RENDERS",
    "FETCHES",
    "HANDLES",
    "ENTRY",
    "DEFINED_IN",
    "CALLS",
    "PUBLISHES",
    "SUBSCRIBES",
    // Walked only into execution Gaps (`owns_execution_gap`), never onward.
    "DEPENDS_ON",
];

/// Node labels the tracer needs (for classification and display names).
pub const FLOW_NODE_LABELS: &[&str] = &[
    "Screen",
    "Component",
    "Endpoint",
    "Symbol",
    "Channel",
    "ExtensionContext",
    "Command",
    "Gap",
    "File",
];

/// Traversal bound (SPEC-00 US-0006 performance note: path queries bounded).
const MAX_DEPTH: usize = 64;

/// Flow confidence status per SPEC-00 §5.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum FlowStatus {
    /// Every hop Confirmed.
    Verified,
    /// At least one Gap hop — emitted with the gap explicit.
    Partial,
    /// No gaps, but at least one inferred hop.
    Inferred,
}

impl FlowStatus {
    /// Display form used by artifacts.
    pub fn as_str(self) -> &'static str {
        match self {
            FlowStatus::Verified => "Verified",
            FlowStatus::Partial => "Partial",
            FlowStatus::Inferred => "Inferred",
        }
    }
}

/// One resolved hop of a flow.
#[derive(Debug, Clone, Serialize)]
pub struct Hop {
    /// Edge label (`HANDLES`, `CALLS`, `PUBLISHES`, `SUBSCRIBES`).
    pub label: String,
    /// Hop source node id (traversal direction, not storage direction:
    /// `SUBSCRIBES` hops run channel → consumer).
    pub src: String,
    /// Hop target node id.
    pub dst: String,
    /// Display name of the source node.
    pub src_name: String,
    /// Display name of the target node.
    pub dst_name: String,
    /// Tier that resolved this hop (from the edge's provenance, AC-0015).
    pub tier: String,
    /// Confidence tier of the hop (R-INT-2: always visible).
    pub confidence: String,
    /// First evidence ref as `path bytes start..end`, if present.
    pub evidence: Option<String>,
    /// Complete edge provenance for inline Workbench/export rendering.
    pub provenance: Provenance,
    /// Gap reason copied from the destination Gap node, when this hop is
    /// unresolved. Kept structured so UI/export surfaces never parse a label.
    pub gap_reason: Option<String>,
    /// Escalation tiers already attempted before the explicit Gap was emitted.
    pub attempted_tiers: Vec<String>,
}

/// One traced flow: a trigger and every hop reachable from it.
#[derive(Debug, Clone, Serialize)]
pub struct Flow {
    /// Trigger node id.
    pub trigger: String,
    /// Trigger node label (`Screen`, `ExtensionContext`, `Command`,
    /// `Endpoint`, or `Channel`).
    pub trigger_kind: String,
    /// Trigger display name.
    pub trigger_name: String,
    /// Hops in traversal order (deduplicated).
    pub hops: Vec<Hop>,
    /// Status per §5.3.
    pub status: FlowStatus,
    /// Mean hop weight per §5.3 (Confirmed 1.0 … Gap 0.0).
    pub score: f64,
    /// True when the traversal depth bound cut the walk short — downstream
    /// hops exist that are not in `hops`. Forces `Partial` (R-INT-4: a
    /// bounded trace is an unresolved continuation, never a silent finish).
    pub depth_limited: bool,
}

fn display_name(node: &Node) -> String {
    let p = &node.props;
    match node.label.as_str() {
        "Screen" => p["route"]
            .as_str()
            .map(|r| format!("Screen {r}"))
            .unwrap_or_else(|| node.id.clone()),
        "Endpoint" => match (p["method"].as_str(), p["path"].as_str()) {
            (Some(m), Some(path)) => format!("{m} {path}"),
            _ => node.id.clone(),
        },
        "Symbol" | "Component" => p["name"].as_str().map(String::from).unwrap_or_else(|| {
            node.id
                .split('#')
                .next_back()
                .unwrap_or(&node.id)
                .to_string()
        }),
        "Channel" => match (p["kind"].as_str(), p["identity"].as_str()) {
            (Some(k), Some(i)) => format!("{k}:{i}"),
            _ => node.id.clone(),
        },
        "ExtensionContext" => {
            let kind = p["kind"].as_str().unwrap_or("context");
            let detail = p["entries"]
                .as_array()
                .and_then(|entries| entries.first())
                .and_then(|entry| entry.as_str())
                .or_else(|| p["title"].as_str().filter(|title| !title.is_empty()));
            match detail {
                Some(detail) => format!("{kind} · {detail}"),
                None => kind.to_string(),
            }
        }
        "Command" => p["name"]
            .as_str()
            .map(|name| format!("Command {name}"))
            .unwrap_or_else(|| node.id.clone()),
        "Gap" => format!("GAP: {}", p["reason"].as_str().unwrap_or("unresolved")),
        "File" => p["path"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| node.id.clone()),
        _ => node.id.clone(),
    }
}

fn hop_weight(confidence: &str) -> f64 {
    match confidence {
        "Confirmed" => 1.0,
        "InferredStrong" => 0.6,
        "InferredWeak" => 0.3,
        _ => 0.0, // Gap and anything unknown counts for nothing.
    }
}

fn edge_prov(edge: &Edge) -> (String, String, Option<String>, Provenance) {
    let provenance = serde_json::from_value::<Provenance>(edge.props["prov"].clone())
        .ok()
        .filter(|provenance| provenance.validate().is_ok())
        .unwrap_or_else(|| {
            let canonical = serde_json::to_vec(&(&edge.src, &edge.label, &edge.dst))
                .expect("edge identity serializes");
            Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Gap,
                vec![],
                "flowtracer.invalid-provenance",
                &canonical,
            )
            .expect("Gap is within the deterministic confidence ceiling")
        });
    let tier = format!("{:?}", provenance.tier);
    let confidence = format!("{:?}", provenance.confidence_tier);
    let evidence = provenance
        .evidence
        .first()
        .map(|ev| format!("{} bytes {}..{}", ev.path, ev.byte_start, ev.byte_end,));
    (tier, confidence, evidence, provenance)
}

/// True when `edge` is a `DEPENDS_ON` from its owner into a Gap that stands
/// for code the owner runs but T0 could not recover (AC-0219, ADR-0032):
/// an unproven `eval()` / `new Function()` argument, or any future Gap an
/// adapter hangs off its owner this way — fail closed, so an unknown kind
/// counts. A Gap flagged `rule_evidence_gap` is excluded: it scopes where
/// source-rule *evidence* stops (including decoded eval code awaiting a
/// span mapping), while the owner's calls were still recovered. Every other
/// `DEPENDS_ON` (e.g. Terraform ordering between resources) is not a flow
/// hop at all.
fn owns_execution_gap(edge: &Edge, by_id: &BTreeMap<&str, &Node>) -> bool {
    let node = by_id.get(edge.dst.as_str());
    let is_gap = node.map_or(edge.dst.starts_with("gap:"), |node| node.label == "Gap");
    let rule_scoped = |props: &serde_json::Value| props["rule_evidence_gap"] == true;
    is_gap && !rule_scoped(&edge.props) && !node.is_some_and(|node| rule_scoped(&node.props))
}

fn gap_context(node: Option<&&Node>) -> (Option<String>, Vec<String>) {
    let Some(node) = node.filter(|node| node.label == "Gap") else {
        return (None, Vec::new());
    };
    let reason = node.props["reason"].as_str().map(String::from);
    let attempted_tiers = node.props["attempted_tiers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tier| tier.as_str().map(String::from))
        .collect();
    (reason, attempted_tiers)
}

/// Trace every flow in the graph slice. Deterministic: triggers in sorted
/// id order, hops in traversal order with sorted expansion (US-0014).
pub fn trace(nodes: &[Node], edges: &[Edge]) -> Vec<Flow> {
    let by_id: BTreeMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    // Adjacency, sorted for deterministic expansion. SUBSCRIBES is stored
    // consumer → channel and DEFINED_IN symbol → file; the tracer walks
    // them reversed (channel → consumer, entry file → its symbols).
    let mut out_edges: BTreeMap<&str, Vec<&Edge>> = BTreeMap::new();
    let mut chan_consumers: BTreeMap<&str, Vec<&Edge>> = BTreeMap::new();
    let mut file_symbols: BTreeMap<&str, Vec<&Edge>> = BTreeMap::new();
    let mut published_to: HashSet<&str> = HashSet::new();
    for edge in edges {
        match edge.label.as_str() {
            "RENDERS" | "FETCHES" | "HANDLES" | "ENTRY" | "CALLS" | "PUBLISHES" => {
                out_edges.entry(edge.src.as_str()).or_default().push(edge);
                if edge.label == "PUBLISHES" {
                    published_to.insert(edge.dst.as_str());
                }
            }
            "SUBSCRIBES" => {
                chan_consumers
                    .entry(edge.dst.as_str())
                    .or_default()
                    .push(edge);
            }
            "DEPENDS_ON" if owns_execution_gap(edge, &by_id) => {
                out_edges.entry(edge.src.as_str()).or_default().push(edge);
            }
            "DEFINED_IN" => {
                file_symbols
                    .entry(edge.dst.as_str())
                    .or_default()
                    .push(edge);
            }
            _ => {}
        }
    }
    for v in out_edges.values_mut() {
        v.sort_by(|a, b| (&a.label, &a.dst).cmp(&(&b.label, &b.dst)));
    }
    for v in chan_consumers.values_mut() {
        v.sort_by(|a, b| a.src.cmp(&b.src));
    }
    for v in file_symbols.values_mut() {
        v.sort_by(|a, b| a.src.cmp(&b.src));
    }

    // Triggers (SPEC-00 §5.1): every user-action anchor — a Screen, an
    // ExtensionContext (popup, content script, background), or a manifest
    // Command (#165) — is traced first; an endpoint keeps its own flow
    // unless an anchor's traversal actually *reached* it (a FETCHES edge
    // from an unrendered component must not make the server flow
    // disappear); channels nothing local publishes to are external events
    // entering the slice.
    let mut anchors: Vec<&Node> = nodes
        .iter()
        .filter(|n| matches!(n.label.as_str(), "Screen" | "ExtensionContext" | "Command"))
        .collect();
    anchors.sort_by(|a, b| a.id.cmp(&b.id));
    let mut flows: Vec<Flow> = anchors
        .iter()
        .map(|t| trace_one(t, &by_id, &out_edges, &chan_consumers, &file_symbols))
        .collect();
    let covered: HashSet<&str> = flows
        .iter()
        .flat_map(|f| f.hops.iter())
        .filter(|h| h.label == "FETCHES")
        .map(|h| h.dst.as_str())
        .collect();

    let mut rest: Vec<&Node> = nodes
        .iter()
        .filter(|n| n.label == "Endpoint" && !covered.contains(n.id.as_str()))
        .collect();
    rest.extend(nodes.iter().filter(|n| {
        n.label == "Channel"
            && !published_to.contains(n.id.as_str())
            && chan_consumers.contains_key(n.id.as_str())
    }));
    rest.sort_by(|a, b| a.id.cmp(&b.id));
    rest.dedup_by(|a, b| a.id == b.id);
    flows.extend(
        rest.iter()
            .map(|t| trace_one(t, &by_id, &out_edges, &chan_consumers, &file_symbols)),
    );
    flows.sort_by(|a, b| a.trigger.cmp(&b.trigger));
    flows
}

/// One anchor kind the tracer sought and how many the slice actually holds
/// (#165) — zero flows must name what was looked for, never shrug.
#[derive(Debug, Clone, Serialize)]
pub struct AnchorProbe {
    /// Human-readable anchor kind, stable and display-ready.
    pub kind: String,
    /// Nodes of that kind present in the traced slice.
    pub found: usize,
}

/// Count every anchor kind `trace` uses, in the order it seeks them.
/// Deterministic for a given slice; independent of input order.
pub fn anchor_probes(nodes: &[Node], edges: &[Edge]) -> Vec<AnchorProbe> {
    let mut published_to: HashSet<&str> = HashSet::new();
    let mut consumed: HashSet<&str> = HashSet::new();
    for edge in edges {
        match edge.label.as_str() {
            "PUBLISHES" => {
                published_to.insert(edge.dst.as_str());
            }
            "SUBSCRIBES" => {
                consumed.insert(edge.dst.as_str());
            }
            _ => {}
        }
    }
    let count = |label: &str| nodes.iter().filter(|n| n.label == label).count();
    let external_channels = nodes
        .iter()
        .filter(|n| {
            n.label == "Channel"
                && consumed.contains(n.id.as_str())
                && !published_to.contains(n.id.as_str())
        })
        .count();
    vec![
        AnchorProbe {
            kind: "screens (routes/pages)".into(),
            found: count("Screen"),
        },
        AnchorProbe {
            kind: "extension contexts (popup, background, content scripts)".into(),
            found: count("ExtensionContext"),
        },
        AnchorProbe {
            kind: "extension commands (keyboard shortcuts)".into(),
            found: count("Command"),
        },
        AnchorProbe {
            kind: "HTTP endpoints".into(),
            found: count("Endpoint"),
        },
        AnchorProbe {
            kind: "externally published event channels".into(),
            found: external_channels,
        },
    ]
}

fn trace_one(
    trigger: &Node,
    by_id: &BTreeMap<&str, &Node>,
    out_edges: &BTreeMap<&str, Vec<&Edge>>,
    chan_consumers: &BTreeMap<&str, Vec<&Edge>>,
    file_symbols: &BTreeMap<&str, Vec<&Edge>>,
) -> Flow {
    let mut hops: Vec<Hop> = Vec::new();
    let mut seen_hops: HashSet<(String, String, String)> = HashSet::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut stack: Vec<(String, usize)> = vec![(trigger.id.clone(), 0)];
    let mut depth_limited = false;

    while let Some((id, depth)) = stack.pop() {
        if depth >= MAX_DEPTH {
            // The bound cut the walk while this node still had somewhere to
            // go — that is a truncation, not a completion.
            if out_edges.contains_key(id.as_str())
                || chan_consumers.contains_key(id.as_str())
                || file_symbols.contains_key(id.as_str())
            {
                depth_limited = true;
            }
            continue;
        }
        if !visited.insert(id.clone()) {
            continue;
        }
        let label = by_id.get(id.as_str()).map(|n| n.label.as_str());
        // A Gap node truncates its branch: it is a terminal by definition
        // (AC-0016) — nothing is walked past it.
        if label == Some("Gap") {
            continue;
        }

        let name_of = |nid: &str| -> String {
            by_id
                .get(nid)
                .map(|n| display_name(n))
                .unwrap_or_else(|| nid.to_string())
        };

        // Forward edges (HANDLES/CALLS/PUBLISHES from this node), then
        // consumer fan-out when this node is a channel. Expansion order is
        // deterministic; the stack is LIFO so push in reverse.
        let mut next: Vec<(Hop, String)> = Vec::new();
        if let Some(outs) = out_edges.get(id.as_str()) {
            for edge in outs {
                let (tier, confidence, evidence, provenance) = edge_prov(edge);
                let (gap_reason, attempted_tiers) = gap_context(by_id.get(edge.dst.as_str()));
                next.push((
                    Hop {
                        label: edge.label.clone(),
                        src: edge.src.clone(),
                        dst: edge.dst.clone(),
                        src_name: name_of(&edge.src),
                        dst_name: name_of(&edge.dst),
                        tier,
                        confidence,
                        evidence,
                        provenance,
                        gap_reason,
                        attempted_tiers,
                    },
                    edge.dst.clone(),
                ));
            }
        }
        // Reversed edges: SUBSCRIBES walks channel → consumer, DEFINED_IN
        // walks entry file → the symbols it defines (#165).
        for reversed in [
            chan_consumers.get(id.as_str()),
            file_symbols.get(id.as_str()),
        ]
        .into_iter()
        .flatten()
        {
            for edge in reversed {
                let (tier, confidence, evidence, provenance) = edge_prov(edge);
                let (gap_reason, attempted_tiers) = gap_context(by_id.get(edge.src.as_str()));
                next.push((
                    Hop {
                        label: edge.label.clone(),
                        src: edge.dst.clone(),
                        dst: edge.src.clone(),
                        src_name: name_of(&edge.dst),
                        dst_name: name_of(&edge.src),
                        tier,
                        confidence,
                        evidence,
                        provenance,
                        gap_reason,
                        attempted_tiers,
                    },
                    edge.src.clone(),
                ));
            }
        }

        // Record hops in sorted expansion order (the dossier's narrative
        // order); the stack gets them reversed so LIFO pops match.
        let mut dsts = Vec::with_capacity(next.len());
        for (hop, dst) in next {
            if seen_hops.insert((hop.label.clone(), hop.src.clone(), hop.dst.clone())) {
                hops.push(hop);
            }
            dsts.push(dst);
        }
        for dst in dsts.into_iter().rev() {
            stack.push((dst, depth + 1));
        }
    }

    let has_gap = hops.iter().any(|h| {
        h.confidence == "Gap" || by_id.get(h.dst.as_str()).is_some_and(|n| n.label == "Gap")
    });
    let all_confirmed = !hops.is_empty() && hops.iter().all(|h| h.confidence == "Confirmed");
    let status = if has_gap || hops.is_empty() || depth_limited {
        FlowStatus::Partial
    } else if all_confirmed {
        FlowStatus::Verified
    } else {
        FlowStatus::Inferred
    };
    let score = if hops.is_empty() {
        0.0
    } else {
        hops.iter().map(|h| hop_weight(&h.confidence)).sum::<f64>() / hops.len() as f64
    };

    Flow {
        trigger: trigger.id.clone(),
        trigger_kind: trigger.label.clone(),
        trigger_name: display_name(trigger),
        hops,
        status,
        score,
        depth_limited,
    }
}

#[cfg(test)]
mod tests;
