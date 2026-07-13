//! Dynamic tier (T1): execution-derived evidence (SPEC-00 §2 ladder, §3.1).
//!
//! M6 slice 1: `terraform show -json` output — state or plan — is observed
//! reality. Its resolved attribute values enrich the T0 resource graph
//! (AC-0009: observation supersedes ambiguous static refs), sensitive
//! values are redacted before anything is stored, and observed channel
//! identities join infra `Resource` nodes to code-layer `Channel` nodes
//! via `BACKS` — the cross-layer seam the T0 tiers cannot see. OTel trace
//! ingest is the next rung (issue #54).
//!
//! R-INT-1 shape: T1 never rewrites a T0 fact. T0 props and `prov` stay
//! untouched; observation lands beside them under `observed` with its own
//! `observed_prov` (Tier::Dynamic, Confirmed — the tier's ceiling).

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

/// Extractor id stamped on every observed fact.
pub const EXTRACTOR_ID: &str = "t1.terraform-state";

/// Replacement for values `terraform show -json` marks sensitive. The
/// secret itself never enters the graph (US-0003 Security).
pub const REDACTED: &str = "[redacted]";

/// State/plan JSON errors.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// Not JSON at all.
    #[error("state json: {0}")]
    Json(#[from] serde_json::Error),
    /// JSON, but not `terraform show -json` output.
    #[error("state shape: {0}")]
    Shape(String),
}

/// One resource instance observed in state/plan values.
#[derive(Debug)]
pub struct ObservedResource {
    /// Module-qualified address (`module.vpc.aws_subnet.a`).
    pub address: String,
    /// Terraform type (`aws_sqs_queue`).
    pub rtype: String,
    /// Top-level scalar attributes; sensitive ones hold [`REDACTED`].
    pub values: BTreeMap<String, serde_json::Value>,
    /// Keys whose values were redacted.
    pub redacted: BTreeSet<String>,
}

/// Everything observed in one `terraform show -json` document.
#[derive(Debug, Default)]
pub struct ObservedState {
    /// Resources across all modules, in document order.
    pub resources: Vec<ObservedResource>,
    /// Module addresses present (`module.vpc`) → direct resource count.
    pub modules: BTreeMap<String, usize>,
}

/// Parse `terraform show -json` output. Accepts both shapes: state
/// (`values.root_module`) and plan (`planned_values.root_module`) — the
/// module tree inside is identical (verified at M6 per SPEC-00 §15).
pub fn parse_state(json: &str) -> Result<ObservedState, StateError> {
    let doc: serde_json::Value = serde_json::from_str(json)?;
    let root = doc
        .get("values")
        .or_else(|| doc.get("planned_values"))
        .and_then(|v| v.get("root_module"))
        .ok_or_else(|| {
            StateError::Shape(
                "expected `terraform show -json` output \
                 (values.root_module or planned_values.root_module)"
                    .into(),
            )
        })?;
    let mut out = ObservedState::default();
    walk_module(root, &mut out);
    Ok(out)
}

fn walk_module(module: &serde_json::Value, out: &mut ObservedState) {
    let empty = Vec::new();
    let resources = module
        .get("resources")
        .and_then(|r| r.as_array())
        .unwrap_or(&empty);
    if let Some(addr) = module.get("address").and_then(|a| a.as_str()) {
        out.modules.insert(addr.to_string(), resources.len());
    }
    for res in resources {
        let (Some(address), Some(rtype)) = (
            res.get("address").and_then(|v| v.as_str()),
            res.get("type").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        let sensitive = res.get("sensitive_values");
        let mut values = BTreeMap::new();
        let mut redacted = BTreeSet::new();
        if let Some(obj) = res.get("values").and_then(|v| v.as_object()) {
            for (key, val) in obj {
                // Scalars only: identity-bearing attributes (id/arn/url/
                // name) are scalar; nested blocks join with OTel/env work.
                if val.is_null() || val.is_object() || val.is_array() {
                    continue;
                }
                if sensitive.and_then(|s| s.get(key)).and_then(|b| b.as_bool()) == Some(true) {
                    values.insert(key.clone(), serde_json::Value::String(REDACTED.into()));
                    redacted.insert(key.clone());
                } else {
                    values.insert(key.clone(), val.clone());
                }
            }
        }
        out.resources.push(ObservedResource {
            address: address.into(),
            rtype: rtype.into(),
            values,
            redacted,
        });
    }
    for child in module
        .get("child_modules")
        .and_then(|c| c.as_array())
        .unwrap_or(&empty)
    {
        walk_module(child, out);
    }
}

/// Counts from an enrichment pass (surfaced in job summaries and tests).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Enrichment {
    /// Resource nodes that gained observed attributes.
    pub resources_enriched: usize,
    /// Placeholder nodes the observation resolved (AC-0009 supersede).
    pub placeholders_resolved: usize,
}

fn observed_prov(state_path: &str, span: &Range<usize>, fact: &str) -> serde_json::Value {
    let p = Provenance::new(
        Tier::Dynamic,
        ConfidenceTier::Confirmed,
        vec![EvidenceRef {
            repo: String::new(), // state file is an input artifact, not repo source
            path: state_path.into(),
            byte_start: span.start as u64,
            byte_end: span.end as u64,
            commit_sha: String::new(),
        }],
        EXTRACTOR_ID,
        fact.as_bytes(),
    )
    .expect("Dynamic/Confirmed is exactly the ceiling");
    serde_json::to_value(p).expect("provenance serializes")
}

/// Byte span of the address's first appearance in the raw document — real
/// evidence a human can jump to, not a synthetic 0..0.
fn address_span(raw: &str, address: &str) -> Range<usize> {
    let needle = format!("\"{address}\"");
    raw.find(&needle)
        .map(|i| i..i + needle.len())
        .unwrap_or(0..raw.len())
}

/// Attach observed attributes to matching T0 `Resource` nodes
/// (`res:{repo}@{address}`) and resolve the placeholders observation
/// confirms. A placeholder was an *ambiguous T0 ref* — an edge endpoint no
/// parsed block defined (a module's innards, a resource in an unparsed
/// file). State proves it exists: the flag drops, the observed type and
/// logical id land, and `observed_prov` records where (AC-0009).
pub fn enrich_resources(
    nodes: &mut [Node],
    repo: &str,
    state: &ObservedState,
    state_path: &str,
    state_raw: &str,
) -> Enrichment {
    let by_address: BTreeMap<&str, &ObservedResource> = state
        .resources
        .iter()
        .map(|r| (r.address.as_str(), r))
        .collect();
    let prefix = format!("res:{repo}@");
    let mut out = Enrichment::default();

    for node in nodes.iter_mut() {
        if node.label != "Resource" {
            continue;
        }
        let Some(address) = node.id.strip_prefix(&prefix) else {
            continue;
        };
        let address = address.to_string();

        let observed = if let Some(obs) = by_address.get(address.as_str()) {
            serde_json::to_value(&obs.values).expect("scalar map serializes")
        } else if node.props.get("placeholder").is_some() && state.modules.contains_key(&address) {
            serde_json::json!({ "module_resources": state.modules[&address] })
        } else {
            continue;
        };

        let span = address_span(state_raw, &address);
        let props = node
            .props
            .as_object_mut()
            .expect("resource props are an object");
        props.insert("observed".into(), observed);
        props.insert(
            "observed_prov".into(),
            observed_prov(state_path, &span, &format!("Observed {address}")),
        );
        if props.remove("placeholder").is_some() {
            let rtype = by_address
                .get(address.as_str())
                .map(|o| o.rtype.clone())
                .unwrap_or_else(|| "module".into());
            props.insert("type".into(), rtype.into());
            props.insert("logical_id".into(), address.clone().into());
            props.insert("resolved_by".into(), EXTRACTOR_ID.into());
            out.placeholders_resolved += 1;
        }
        out.resources_enriched += 1;
    }
    out
}

/// Resource types whose observed attribute names a code-layer channel:
/// (terraform type, channel kind, identity attribute). Kinds match the
/// event SDK registry (`adapters-fw::events`) exactly — that equality is
/// what makes the join deterministic.
pub const CHANNEL_BACKINGS: &[(&str, &str, &str)] = &[
    ("aws_sqs_queue", "sqs-queue", "url"),
    ("aws_sns_topic", "sns-topic", "arn"),
];

/// `BACKS` edge candidates from enriched nodes: deployed resource → the
/// channel its observed identity names (`chan:{kind}:{identity}`). The
/// caller inserts a candidate only if that channel node exists — a queue
/// no code publishes or subscribes to is topology, not a channel.
pub fn backing_candidates(nodes: &[Node]) -> Vec<Edge> {
    let mut out = Vec::new();
    for node in nodes {
        if node.label != "Resource" {
            continue;
        }
        let Some(rtype) = node.props.get("type").and_then(|t| t.as_str()) else {
            continue;
        };
        let Some((_, kind, attr)) = CHANNEL_BACKINGS.iter().find(|(t, ..)| *t == rtype) else {
            continue;
        };
        let Some(identity) = node
            .props
            .get("observed")
            .and_then(|o| o.get(*attr))
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        if identity == REDACTED {
            continue;
        }
        // Same observation, new fact: rebuild provenance from the node's
        // observed evidence so the edge's content hash names this edge.
        let Some(prov) = node
            .props
            .get("observed_prov")
            .and_then(|p| serde_json::from_value::<Provenance>(p.clone()).ok())
        else {
            continue;
        };
        let chan_id = format!("chan:{kind}:{identity}");
        let fact = format!("BACKS {} -> {chan_id}", node.id);
        let edge_prov = Provenance::new(
            Tier::Dynamic,
            ConfidenceTier::Confirmed,
            prov.evidence,
            EXTRACTOR_ID,
            fact.as_bytes(),
        )
        .expect("Dynamic/Confirmed is exactly the ceiling");
        out.push(Edge {
            src: node.id.clone(),
            dst: chan_id,
            label: "BACKS".into(),
            props: serde_json::json!({
                "identity_attr": attr,
                "prov": serde_json::to_value(edge_prov).expect("provenance serializes"),
            }),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATE: &str = r#"{
        "format_version": "1.0",
        "values": { "root_module": {
            "resources": [
                {
                    "address": "aws_sqs_queue.orders",
                    "mode": "managed",
                    "type": "aws_sqs_queue",
                    "name": "orders",
                    "values": {
                        "id": "https://sqs.us-east-1.amazonaws.com/1/orders",
                        "url": "https://sqs.us-east-1.amazonaws.com/1/orders",
                        "name": "orders",
                        "tags": {"team": "commerce"},
                        "master_key": "hunter2"
                    },
                    "sensitive_values": { "master_key": true, "tags": {} }
                }
            ],
            "child_modules": [
                { "address": "module.vpc", "resources": [
                    {
                        "address": "module.vpc.aws_subnet.a",
                        "mode": "managed",
                        "type": "aws_subnet",
                        "name": "a",
                        "values": { "id": "subnet-123" },
                        "sensitive_values": {}
                    }
                ] }
            ]
        } }
    }"#;

    fn t0_resource(id: &str, rtype: &str) -> Node {
        Node {
            id: id.into(),
            label: "Resource".into(),
            props: serde_json::json!({
                "type": rtype,
                "logical_id": id.rsplit('@').next().unwrap(),
                "prov": {"tier": "Deterministic"},
            }),
        }
    }

    #[test]
    fn state_and_plan_shapes_both_parse() {
        let state = parse_state(STATE).unwrap();
        assert_eq!(state.resources.len(), 2);
        assert_eq!(state.modules["module.vpc"], 1);

        let plan = STATE.replace(
            "\"values\": { \"root_module\"",
            "\"planned_values\": { \"root_module\"",
        );
        assert_eq!(parse_state(&plan).unwrap().resources.len(), 2);

        let err = parse_state("{\"not\": \"terraform\"}").unwrap_err();
        assert!(err.to_string().contains("terraform show -json"));
    }

    #[test]
    fn sensitive_values_are_redacted_never_stored() {
        // US-0003 Security: secrets in state are redacted.
        let state = parse_state(STATE).unwrap();
        let queue = &state.resources[0];
        assert_eq!(
            queue.values["master_key"],
            serde_json::Value::String(REDACTED.into())
        );
        assert!(queue.redacted.contains("master_key"));
        let serialized = serde_json::to_string(&queue.values).unwrap();
        assert!(!serialized.contains("hunter2"));
    }

    #[test]
    fn observed_values_enrich_t0_resources_with_dynamic_provenance() {
        let state = parse_state(STATE).unwrap();
        let mut nodes = vec![t0_resource(
            "res:local/infra@aws_sqs_queue.orders",
            "aws_sqs_queue",
        )];
        let report = enrich_resources(&mut nodes, "local/infra", &state, "state.json", STATE);
        assert_eq!(report.resources_enriched, 1);
        // T0 fact untouched (R-INT-1); observation lands beside it.
        assert_eq!(nodes[0].props["prov"]["tier"], "Deterministic");
        assert_eq!(
            nodes[0].props["observed"]["url"],
            "https://sqs.us-east-1.amazonaws.com/1/orders"
        );
        assert_eq!(nodes[0].props["observed_prov"]["tier"], "Dynamic");
        assert_eq!(
            nodes[0].props["observed_prov"]["confidence_tier"],
            "Confirmed"
        );
        // Evidence points into the state document at the address, not 0..0.
        let start = nodes[0].props["observed_prov"]["evidence"][0]["byte_start"]
            .as_u64()
            .unwrap() as usize;
        assert_eq!(&STATE[start..start + 22], "\"aws_sqs_queue.orders\"");
    }

    #[test]
    fn observation_supersedes_placeholder_refs() {
        // AC-0009: the module placeholder was an ambiguous T0 ref; state
        // confirms it exists and what it holds.
        let state = parse_state(STATE).unwrap();
        let mut nodes = vec![Node {
            id: "res:local/infra@module.vpc".into(),
            label: "Resource".into(),
            props: serde_json::json!({ "placeholder": true }),
        }];
        let report = enrich_resources(&mut nodes, "local/infra", &state, "state.json", STATE);
        assert_eq!(report.placeholders_resolved, 1);
        assert!(nodes[0].props.get("placeholder").is_none());
        assert_eq!(nodes[0].props["type"], "module");
        assert_eq!(nodes[0].props["logical_id"], "module.vpc");
        assert_eq!(nodes[0].props["observed"]["module_resources"], 1);
        assert_eq!(nodes[0].props["resolved_by"], EXTRACTOR_ID);
    }

    #[test]
    fn backing_candidates_name_the_channel_from_observed_identity() {
        let state = parse_state(STATE).unwrap();
        let mut nodes = vec![t0_resource(
            "res:local/infra@aws_sqs_queue.orders",
            "aws_sqs_queue",
        )];
        enrich_resources(&mut nodes, "local/infra", &state, "state.json", STATE);
        let edges = backing_candidates(&nodes);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].label, "BACKS");
        assert_eq!(edges[0].src, "res:local/infra@aws_sqs_queue.orders");
        assert_eq!(
            edges[0].dst,
            "chan:sqs-queue:https://sqs.us-east-1.amazonaws.com/1/orders"
        );
        assert_eq!(edges[0].props["prov"]["tier"], "Dynamic");

        // No observation, no candidate — T0 alone cannot assert BACKS.
        let bare = vec![t0_resource(
            "res:local/infra@aws_sqs_queue.orders",
            "aws_sqs_queue",
        )];
        assert!(backing_candidates(&bare).is_empty());
    }

    #[test]
    fn redacted_identity_never_becomes_a_channel() {
        // A sensitive identity is redacted; asserting a BACKS edge from
        // "[redacted]" would be an unsupported fact.
        let mut nodes = vec![t0_resource(
            "res:local/infra@aws_sqs_queue.q",
            "aws_sqs_queue",
        )];
        let props = nodes[0].props.as_object_mut().unwrap();
        props.insert("observed".into(), serde_json::json!({ "url": REDACTED }));
        props.insert(
            "observed_prov".into(),
            observed_prov("state.json", &(0..1), "Observed aws_sqs_queue.q"),
        );
        assert!(backing_candidates(&nodes).is_empty());
    }
}
