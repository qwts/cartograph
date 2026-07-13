//! Dynamic tier (T1): execution-derived evidence (SPEC-00 §2 ladder, §3.1).
//!
//! M6 slice 1: `terraform show -json` output — state or plan — is observed
//! reality. Its resolved attribute values enrich the T0 resource graph
//! (AC-0009: observation supersedes ambiguous static refs), sensitive
//! values are redacted before anything is stored, and observed channel
//! identities join infra `Resource` nodes to code-layer `Channel` nodes
//! via `BACKS` — the cross-layer seam the T0 tiers cannot see.
//!
//! M6 slice 2: collector file-exporter OTLP/JSON Lines traces recover
//! observed messaging identities and HTTP attributes. A uniquely matched
//! messaging observation fills an explicit T0 channel Gap; ambiguous
//! observations leave the Gap intact with T1 recorded as attempted
//! (AC-0012, R-INT-1, R-INT-4, issue #54).
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

/// Extractor id for OTLP/JSON trace observations (issue #54).
pub const OTEL_EXTRACTOR_ID: &str = "t1.otel-trace";

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

/// OTLP/JSON Lines parsing errors.
#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    /// One JSON Lines record is not valid JSON.
    #[error("OTLP JSONL line {line}: {source}")]
    Json {
        /// One-based line number.
        line: usize,
        /// JSON parser failure.
        source: serde_json::Error,
    },
    /// The file contained JSON records, but no trace export request.
    #[error("OTLP JSONL shape: expected resourceSpans → scopeSpans → spans")]
    Shape,
}

/// One span observed in an OTLP trace export.
#[derive(Debug, Clone)]
pub struct ObservedSpan {
    /// OTLP trace id when present.
    pub trace_id: String,
    /// OTLP span id; required for trace evidence.
    pub span_id: String,
    /// Human-readable span name.
    pub name: String,
    /// Span attributes, decoded from OTLP AnyValue scalars.
    pub attributes: BTreeMap<String, serde_json::Value>,
    /// Resource attributes inherited by the span (for example service.name).
    pub resource_attributes: BTreeMap<String, serde_json::Value>,
    /// Byte span of span_id in the JSON Lines file.
    pub evidence_span: Range<usize>,
}

/// All spans observed in one OTLP/JSON Lines file, in record order.
#[derive(Debug, Default)]
pub struct ObservedTrace {
    /// Parsed spans.
    pub spans: Vec<ObservedSpan>,
}

fn array_field<'a>(value: &'a serde_json::Value, names: &[&str]) -> &'a [serde_json::Value] {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(serde_json::Value::as_array))
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn string_field<'a>(value: &'a serde_json::Value, names: &[&str]) -> &'a str {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(serde_json::Value::as_str))
        .unwrap_or("")
}

fn any_value(value: &serde_json::Value) -> Option<serde_json::Value> {
    for name in ["stringValue", "string_value"] {
        if let Some(v) = value.get(name).and_then(serde_json::Value::as_str) {
            return Some(v.into());
        }
    }
    for name in ["intValue", "int_value"] {
        if let Some(v) = value.get(name) {
            if let Some(n) = v.as_i64() {
                return Some(n.into());
            }
            if let Some(s) = v.as_str()
                && let Ok(n) = s.parse::<i64>()
            {
                return Some(n.into());
            }
        }
    }
    for name in ["doubleValue", "double_value"] {
        if let Some(v) = value.get(name).and_then(serde_json::Value::as_f64) {
            return serde_json::Number::from_f64(v).map(serde_json::Value::Number);
        }
    }
    for name in ["boolValue", "bool_value"] {
        if let Some(v) = value.get(name).and_then(serde_json::Value::as_bool) {
            return Some(v.into());
        }
    }
    None
}

fn attributes(value: &serde_json::Value) -> BTreeMap<String, serde_json::Value> {
    array_field(value, &["attributes"])
        .iter()
        .filter_map(|attribute| {
            let key = attribute.get("key")?.as_str()?;
            let value = any_value(attribute.get("value")?)?;
            Some((key.to_string(), value))
        })
        .collect()
}

/// Parse collector file-exporter OTLP/JSON: one ExportTraceServiceRequest
/// per non-empty line. Both canonical lowerCamelCase and protobuf
/// snake_case field spellings are accepted.
pub fn parse_otlp_jsonl(raw: &str) -> Result<ObservedTrace, TraceError> {
    let mut out = ObservedTrace::default();
    let mut offset = 0;
    let mut saw_trace_shape = false;
    for (line_index, line_with_newline) in raw.split_inclusive('\n').enumerate() {
        let line = line_with_newline.trim();
        if line.is_empty() {
            offset += line_with_newline.len();
            continue;
        }
        let document: serde_json::Value =
            serde_json::from_str(line).map_err(|source| TraceError::Json {
                line: line_index + 1,
                source,
            })?;
        let resources = array_field(&document, &["resourceSpans", "resource_spans"]);
        saw_trace_shape |= !resources.is_empty();
        for resource_span in resources {
            let resource_attributes = resource_span
                .get("resource")
                .map(attributes)
                .unwrap_or_default();
            for scope_span in array_field(resource_span, &["scopeSpans", "scope_spans"]) {
                for span in array_field(scope_span, &["spans"]) {
                    let span_id = string_field(span, &["spanId", "span_id"]).to_string();
                    if span_id.is_empty() {
                        continue;
                    }
                    let local_start = line_with_newline.find(&span_id).unwrap_or(0);
                    out.spans.push(ObservedSpan {
                        trace_id: string_field(span, &["traceId", "trace_id"]).to_string(),
                        span_id: span_id.clone(),
                        name: string_field(span, &["name"]).to_string(),
                        attributes: attributes(span),
                        resource_attributes: resource_attributes.clone(),
                        evidence_span: (offset + local_start)
                            ..(offset + local_start + span_id.len()),
                    });
                }
            }
        }
        offset += line_with_newline.len();
    }
    if !saw_trace_shape {
        return Err(TraceError::Shape);
    }
    Ok(out)
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

/// Counts from one OTLP trace-enrichment pass.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TraceEnrichment {
    /// Channel Gap nodes replaced by observed Channel nodes.
    pub channel_gaps_resolved: usize,
    /// T0 Endpoint nodes that gained observed HTTP facts.
    pub endpoints_enriched: usize,
    /// Channel Gaps that remained because observation was missing or ambiguous.
    pub channel_gaps_unresolved: usize,
}

fn attr_str<'a>(span: &'a ObservedSpan, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| {
        span.attributes
            .get(*key)
            .or_else(|| span.resource_attributes.get(*key))
            .and_then(serde_json::Value::as_str)
    })
}

fn channel_kind(system: &str) -> Option<&'static str> {
    match system.to_ascii_lowercase().as_str() {
        "aws_sqs" | "sqs" => Some("sqs-queue"),
        "aws_sns" | "sns" => Some("sns-topic"),
        "aws_eventbridge" | "eventbridge" => Some("eventbridge-bus"),
        "kafka" => Some("kafka-topic"),
        "inproc" | "in-process" => Some("inproc-event"),
        _ => None,
    }
}

fn observed_channel(span: &ObservedSpan) -> Option<(&str, &str)> {
    let system = attr_str(span, &["messaging.system"])?;
    let kind = channel_kind(system)?;
    let identity = attr_str(
        span,
        &[
            "messaging.destination.name",
            "messaging.destination_name",
            "messaging.destination",
        ],
    )?;
    (!identity.is_empty()).then_some((kind, identity))
}

fn trace_prov(trace_path: &str, span: &ObservedSpan, fact: &str) -> serde_json::Value {
    let provenance = Provenance::new(
        Tier::Dynamic,
        ConfidenceTier::Confirmed,
        vec![EvidenceRef {
            repo: String::new(),
            path: trace_path.into(),
            byte_start: span.evidence_span.start as u64,
            byte_end: span.evidence_span.end as u64,
            commit_sha: String::new(),
        }],
        OTEL_EXTRACTOR_ID,
        fact.as_bytes(),
    )
    .expect("Dynamic/Confirmed is exactly the ceiling");
    serde_json::to_value(provenance).expect("provenance serializes")
}

fn normalized_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_string()
}

fn paths_match(observed: &str, source: &str) -> bool {
    let observed = normalized_path(observed);
    let source = normalized_path(source);
    observed == source || observed.ends_with(&format!("/{source}"))
}

fn gap_source_path(node: &Node) -> Option<&str> {
    node.props
        .get("prov")?
        .get("evidence")?
        .as_array()?
        .first()?
        .get("path")?
        .as_str()
}

fn http_path(value: &str) -> String {
    let without_query = value.split(['?', '#']).next().unwrap_or(value);
    if let Some(scheme) = without_query.find("://") {
        let after_host = &without_query[scheme + 3..];
        return after_host
            .find('/')
            .map(|slash| after_host[slash..].to_string())
            .unwrap_or_else(|| "/".into());
    }
    without_query.to_string()
}

fn enrich_http_endpoints(nodes: &mut [Node], trace: &ObservedTrace, trace_path: &str) -> usize {
    let mut enriched = 0;
    for span in &trace.spans {
        let Some(method) = attr_str(span, &["http.request.method", "http.method"]) else {
            continue;
        };
        let Some(path) = attr_str(span, &["http.route", "url.path", "http.target", "http.url"])
        else {
            continue;
        };
        let path = http_path(path);
        let matching: Vec<usize> = nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                node.label == "Endpoint"
                    && node.props.get("method").and_then(serde_json::Value::as_str)
                        == Some(method.to_ascii_uppercase().as_str())
                    && node.props.get("path").and_then(serde_json::Value::as_str)
                        == Some(path.as_str())
            })
            .map(|(index, _)| index)
            .collect();
        if matching.len() != 1 {
            continue;
        }
        let node = &mut nodes[matching[0]];
        if node
            .props
            .get("observed_prov")
            .and_then(|p| p.get("extractor_id"))
            .and_then(serde_json::Value::as_str)
            == Some(OTEL_EXTRACTOR_ID)
        {
            continue;
        }
        let http: BTreeMap<String, serde_json::Value> = span
            .attributes
            .iter()
            .filter(|(key, _)| key.starts_with("http.") || key.starts_with("url."))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let fact = format!("Observed HTTP endpoint {}", node.id);
        let props = node
            .props
            .as_object_mut()
            .expect("endpoint props are an object");
        props.insert(
            "observed".into(),
            serde_json::json!({
                "trace_id": span.trace_id,
                "span_id": span.span_id,
                "span_name": span.name,
                "http": http,
            }),
        );
        props.insert("observed_prov".into(), trace_prov(trace_path, span, &fact));
        enriched += 1;
    }
    enriched
}

/// Apply OTLP trace observations to a T0 extraction. T1 may fill an
/// explicit Gap but never replaces a Confirmed T0 node (R-INT-1).
///
/// A channel observation resolves a Gap only when it is unambiguous:
/// `code.file.path` narrows it to the originating source file; without
/// code location there must be exactly one Gap and one observed identity
/// for that channel kind. Otherwise the Gap remains explicit and records
/// that T1 was attempted (R-INT-4).
pub fn apply_trace(
    nodes: &mut Vec<Node>,
    edges: &mut [Edge],
    trace: &ObservedTrace,
    trace_path: &str,
) -> TraceEnrichment {
    let mut report = TraceEnrichment {
        endpoints_enriched: enrich_http_endpoints(nodes, trace, trace_path),
        ..TraceEnrichment::default()
    };
    let gaps: Vec<(String, String, String)> = nodes
        .iter()
        .filter(|node| node.label == "Gap")
        .filter_map(|node| {
            Some((
                node.id.clone(),
                node.props.get("kind")?.as_str()?.to_string(),
                gap_source_path(node)?.to_string(),
            ))
        })
        .collect();
    let mut gaps_per_kind = BTreeMap::<String, usize>::new();
    for (_, kind, _) in &gaps {
        *gaps_per_kind.entry(kind.clone()).or_default() += 1;
    }
    let mut resolved_gap_ids = BTreeSet::new();
    let mut new_channels = BTreeMap::<String, Node>::new();

    for (gap_id, gap_kind, source_path) in gaps {
        let same_kind: Vec<&ObservedSpan> = trace
            .spans
            .iter()
            .filter(|span| observed_channel(span).is_some_and(|(kind, _)| kind == gap_kind))
            .collect();
        let located: Vec<&ObservedSpan> = same_kind
            .iter()
            .copied()
            .filter(|span| {
                attr_str(span, &["code.file.path", "code.filepath", "code.file.name"])
                    .is_some_and(|path| paths_match(path, &source_path))
            })
            .collect();
        let has_locations = same_kind.iter().any(|span| {
            attr_str(span, &["code.file.path", "code.filepath", "code.file.name"]).is_some()
        });
        let candidates = if has_locations {
            located
        } else if gaps_per_kind.get(&gap_kind) == Some(&1) {
            same_kind
        } else {
            Vec::new()
        };
        let identities: BTreeSet<&str> = candidates
            .iter()
            .filter_map(|span| observed_channel(span).map(|(_, identity)| identity))
            .collect();

        if identities.len() != 1 {
            if let Some(gap) = nodes.iter_mut().find(|node| node.id == gap_id)
                && let Some(props) = gap.props.as_object_mut()
            {
                props.insert("attempted_tiers".into(), serde_json::json!(["T0", "T1"]));
                props.insert(
                    "t1_reason".into(),
                    if candidates.is_empty() {
                        "no trace observation uniquely matched this source gap"
                    } else {
                        "multiple observed channel identities matched this source gap"
                    }
                    .into(),
                );
            }
            report.channel_gaps_unresolved += 1;
            continue;
        }

        let identity = *identities.iter().next().expect("one identity");
        let span = candidates
            .iter()
            .find(|span| observed_channel(span).is_some_and(|(_, value)| value == identity))
            .expect("identity came from a candidate");
        let channel_id = format!("chan:{gap_kind}:{identity}");
        if !nodes.iter().any(|node| node.id == channel_id)
            && !new_channels.contains_key(&channel_id)
        {
            let fact = format!("Observed Channel {channel_id}");
            new_channels.insert(
                channel_id.clone(),
                Node {
                    id: channel_id.clone(),
                    label: "Channel".into(),
                    props: serde_json::json!({
                        "kind": gap_kind,
                        "identity": identity,
                        "observed": {
                            "trace_id": span.trace_id,
                            "span_id": span.span_id,
                            "span_name": span.name,
                        },
                        "prov": trace_prov(trace_path, span, &fact),
                    }),
                },
            );
        }
        for edge in edges.iter_mut().filter(|edge| edge.dst == gap_id) {
            edge.dst = channel_id.clone();
            let fact = format!("{} {} -> {channel_id}", edge.label, edge.src);
            edge.props = serde_json::json!({
                "resolver": OTEL_EXTRACTOR_ID,
                "prov": trace_prov(trace_path, span, &fact),
            });
        }
        resolved_gap_ids.insert(gap_id);
        report.channel_gaps_resolved += 1;
    }

    nodes.retain(|node| !resolved_gap_ids.contains(&node.id));
    nodes.extend(new_channels.into_values());
    report
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

    const OTLP: &str = r#"{"resourceSpans":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"checkout"}}]},"scopeSpans":[{"spans":[{"traceId":"trace-1","spanId":"span-msg-1","name":"send order","attributes":[{"key":"messaging.system","value":{"stringValue":"aws_sqs"}},{"key":"messaging.destination.name","value":{"stringValue":"orders-runtime"}},{"key":"code.file.path","value":{"stringValue":"/workspace/src/send.ts"}}]},{"traceId":"trace-1","spanId":"span-http-1","name":"POST /orders","attributes":[{"key":"http.request.method","value":{"stringValue":"POST"}},{"key":"http.route","value":{"stringValue":"/orders"}},{"key":"http.response.status_code","value":{"intValue":"202"}}]}]}]}]}
"#;

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

    #[test]
    fn otlp_jsonl_parses_messaging_and_http_span_attributes() {
        // Issue #54 / AC-0012: collector file-exporter OTLP/JSON Lines is
        // decoded deterministically, including resource attributes.
        let trace = parse_otlp_jsonl(OTLP).unwrap();
        assert_eq!(trace.spans.len(), 2);
        assert_eq!(trace.spans[0].span_id, "span-msg-1");
        assert_eq!(
            trace.spans[0].attributes["messaging.destination.name"],
            "orders-runtime"
        );
        assert_eq!(
            trace.spans[0].resource_attributes["service.name"],
            "checkout"
        );
        assert_eq!(trace.spans[1].attributes["http.response.status_code"], 202);
        assert!(parse_otlp_jsonl("{\"not\":\"traces\"}\n").is_err());
    }

    #[test]
    fn otel_observation_resolves_channel_gap_and_enriches_http_endpoint() {
        // T-0012: a runtime-computed channel Gap is filled by T1 observed
        // identity with span/file provenance; T0 endpoint provenance is
        // preserved beside observed HTTP facts (R-INT-1).
        let trace = parse_otlp_jsonl(OTLP).unwrap();
        let gap_id = "gap:chan:shop@src/send.ts@10";
        let mut nodes = vec![
            Node {
                id: gap_id.into(),
                label: "Gap".into(),
                props: serde_json::json!({
                    "kind": "sqs-queue",
                    "reason": "runtime-computed channel identity",
                    "attempted_tiers": ["T0"],
                    "prov": {"evidence": [{"path": "src/send.ts"}]},
                }),
            },
            Node {
                id: "ep:shop@POST:/orders".into(),
                label: "Endpoint".into(),
                props: serde_json::json!({
                    "method": "POST",
                    "path": "/orders",
                    "prov": {"tier": "Deterministic"},
                }),
            },
        ];
        let mut edges = vec![Edge {
            src: "sym:shop@src/send.ts#enqueue".into(),
            dst: gap_id.into(),
            label: "PUBLISHES".into(),
            props: serde_json::json!({"prov": {"confidence_tier": "Gap"}}),
        }];

        let report = apply_trace(&mut nodes, &mut edges, &trace, "trace.jsonl");
        assert_eq!(report.channel_gaps_resolved, 1);
        assert_eq!(report.endpoints_enriched, 1);
        assert!(nodes.iter().all(|node| node.label != "Gap"));
        let channel = nodes
            .iter()
            .find(|node| node.id == "chan:sqs-queue:orders-runtime")
            .unwrap();
        assert_eq!(channel.props["prov"]["tier"], "Dynamic");
        assert_eq!(channel.props["prov"]["confidence_tier"], "Confirmed");
        assert_eq!(channel.props["observed"]["span_id"], "span-msg-1");
        let start = channel.props["prov"]["evidence"][0]["byte_start"]
            .as_u64()
            .unwrap() as usize;
        assert_eq!(&OTLP[start..start + 10], "span-msg-1");
        assert_eq!(edges[0].dst, "chan:sqs-queue:orders-runtime");
        assert_eq!(edges[0].props["resolver"], OTEL_EXTRACTOR_ID);
        assert_eq!(edges[0].props["prov"]["tier"], "Dynamic");
        let endpoint = nodes.iter().find(|node| node.label == "Endpoint").unwrap();
        assert_eq!(endpoint.props["prov"]["tier"], "Deterministic");
        assert_eq!(endpoint.props["observed"]["span_id"], "span-http-1");
        assert_eq!(
            endpoint.props["observed"]["http"]["http.response.status_code"],
            202
        );
        assert_eq!(endpoint.props["observed_prov"]["tier"], "Dynamic");
    }

    #[test]
    fn ambiguous_trace_identity_keeps_explicit_gaps() {
        // One unlocated observation cannot be stretched across two same-kind
        // source Gaps. T1 records its attempt and leaves both explicit.
        let raw = OTLP.replace(
            ",{\"key\":\"code.file.path\",\"value\":{\"stringValue\":\"/workspace/src/send.ts\"}}",
            "",
        );
        let trace = parse_otlp_jsonl(&raw).unwrap();
        let mut nodes: Vec<Node> = ["src/a.ts", "src/b.ts"]
            .into_iter()
            .enumerate()
            .map(|(index, path)| Node {
                id: format!("gap:chan:shop@{path}@{index}"),
                label: "Gap".into(),
                props: serde_json::json!({
                    "kind": "sqs-queue",
                    "prov": {"evidence": [{"path": path}]},
                }),
            })
            .collect();
        let mut edges = Vec::new();
        let report = apply_trace(&mut nodes, &mut edges, &trace, "trace.jsonl");
        assert_eq!(report.channel_gaps_resolved, 0);
        assert_eq!(report.channel_gaps_unresolved, 2);
        assert!(nodes.iter().all(|node| node.label == "Gap"));
        assert!(
            nodes
                .iter()
                .all(|node| { node.props["attempted_tiers"] == serde_json::json!(["T0", "T1"]) })
        );
    }
}
