//! Immutable, transport-independent recovered-graph reads (SPEC-01, issue #380).
//!
//! The snapshot owns its facts, identifies their complete canonical input content,
//! and serves deterministic pages under caller budgets. It performs no model,
//! network, evidence-file, or target-code operations. Accepted proposals are not
//! part of this first recovered-graph projection.

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, Provenance};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Maximum number of returned facts in a page.
pub const MAX_FACTS: usize = 500;
/// Maximum serialized successful response size, including its envelope and cursor.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// Maximum radius of an undirected neighborhood query.
pub const MAX_NEIGHBORHOOD_HOPS: usize = 3;

/// A graph fact kind; omitted query kind selects both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactKind {
    /// A node, selected by its exact label.
    Node,
    /// A directed edge, selected by its exact relation label.
    Edge,
}

/// Unambiguous graph identities. Node references sort before edge references.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FactReference {
    /// A node's stable identifier.
    Node {
        /// Node identity within the snapshot.
        id: String,
    },
    /// A directed relationship, ordered by source, label, then destination.
    Edge {
        /// Source node identity.
        source: String,
        /// Exact relation label.
        label: String,
        /// Destination node identity.
        destination: String,
    },
}

impl FactReference {
    fn kind(&self) -> FactKind {
        match self {
            Self::Node { .. } => FactKind::Node,
            Self::Edge { .. } => FactKind::Edge,
        }
    }
}

/// Why a fact's primary provenance cannot be treated as authoritative metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceProblem {
    /// No primary `prov` property was supplied.
    Missing,
    /// The property does not deserialize as the known provenance schema.
    Malformed,
    /// The producing tier asserted a confidence above its permitted ceiling.
    AboveCeiling,
}

/// A read projection of a fact, with provenance separated from its properties.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextFact {
    /// Typed identity, safe even when identifiers contain delimiters.
    pub reference: FactReference,
    /// Node label or edge relation label.
    pub label: String,
    /// Original properties with raw primary `prov` removed.
    pub properties: Value,
    /// Validated primary provenance, or `None` when it could not be validated.
    pub provenance: Option<Provenance>,
    /// Explicit problem when the primary provenance could not be validated.
    pub provenance_problem: Option<ProvenanceProblem>,
    /// Original validated confidence, or `Gap` on any provenance problem.
    pub confidence_tier: ConfidenceTier,
}

impl ContextFact {
    fn new(reference: FactReference, label: String, mut properties: Value) -> Self {
        let raw = properties
            .as_object_mut()
            .and_then(|properties| properties.remove("prov"));
        let validated = match raw {
            None => Err(ProvenanceProblem::Missing),
            Some(value) => serde_json::from_value::<Provenance>(value)
                .map_err(|_| ProvenanceProblem::Malformed)
                .and_then(|provenance| {
                    provenance
                        .validate()
                        .map_err(|_| ProvenanceProblem::AboveCeiling)?;
                    Ok(provenance)
                }),
        };
        let (provenance, provenance_problem, confidence_tier) = match validated {
            Ok(provenance) => {
                let confidence = provenance.confidence_tier;
                (Some(provenance), None, confidence)
            }
            Err(problem) => (None, Some(problem), ConfidenceTier::Gap),
        };
        Self {
            reference,
            label,
            properties,
            provenance,
            provenance_problem,
            confidence_tier,
        }
    }
}

/// The graph scope to select before applying kind and label filters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QueryScope {
    /// All stored graph facts.
    #[default]
    All,
    /// Nodes within the undirected radius and edges induced by those nodes.
    Neighborhood {
        /// A node that must exist even if the output filters select no facts.
        anchor: String,
        /// Radius, from one through [`MAX_NEIGHBORHOOD_HOPS`].
        hops: usize,
    },
}

/// A continuation bound to one immutable snapshot and normalized selection.
///
/// Cursors are consistency tokens, not authorization credentials. Budgets may
/// change between pages; scope, kind, and normalized exact labels may not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryCursor {
    /// Complete recovered-graph content identity.
    pub snapshot_id: String,
    /// Identity of the normalized scope, kind, and labels.
    pub selection_id: String,
    /// Number of selected facts preceding the next page.
    pub offset: usize,
}

/// Bounded query contract shared by UI and future agent transports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryRequest {
    /// Select all facts or a bounded neighborhood.
    #[serde(default)]
    pub scope: QueryScope,
    /// Select nodes, edges, or both when omitted.
    #[serde(default)]
    pub kind: Option<FactKind>,
    /// Exact labels; an empty list selects every label. Order and duplicates
    /// are normalized, while case and whitespace remain significant.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Maximum returned facts, from one through [`MAX_FACTS`].
    pub max_facts: usize,
    /// Maximum complete JSON response bytes, from one through
    /// [`MAX_RESPONSE_BYTES`].
    pub max_bytes: usize,
    /// Continue a prior response without changing its snapshot or selection.
    #[serde(default)]
    pub cursor: Option<QueryCursor>,
}

impl Default for QueryRequest {
    fn default() -> Self {
        Self {
            scope: QueryScope::All,
            kind: None,
            labels: Vec::new(),
            max_facts: MAX_FACTS,
            max_bytes: MAX_RESPONSE_BYTES,
            cursor: None,
        }
    }
}

impl QueryRequest {
    /// Validate caller budgets and radius without loading or copying a graph.
    /// Snapshot-dependent checks, including anchor and cursor validity, occur
    /// when the immutable snapshot executes the query.
    pub fn validate_limits(&self) -> Result<(), ContextError> {
        validate_limit("max_facts", self.max_facts, MAX_FACTS)?;
        validate_limit("max_bytes", self.max_bytes, MAX_RESPONSE_BYTES)?;
        if let QueryScope::Neighborhood { hops, .. } = &self.scope {
            validate_limit("hops", *hops, MAX_NEIGHBORHOOD_HOPS)?;
        }
        Ok(())
    }
}

/// The projection a response represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextView {
    /// Recovered graph only; accepted proposals are not added by this service.
    RecoveredGraph,
}

/// One deterministic page, measured as compact UTF-8 JSON for its byte budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryResponse {
    /// Complete content identity of the immutable snapshot.
    pub snapshot_id: String,
    /// Explicit projection designation.
    pub view: ContextView,
    /// Total matching facts across all pages.
    pub total_selected: usize,
    /// Returned facts, in node-id then edge-(source,label,destination) order.
    pub facts: Vec<ContextFact>,
    /// Continuation when more selected facts remain; otherwise `None`.
    pub next_cursor: Option<QueryCursor>,
}

/// Explicit construction, query, or budget failure.
#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    /// Two input nodes share one identity.
    #[error("duplicate node identity: {0}")]
    DuplicateNode(String),
    /// Two input edges share one directed identity.
    #[error("duplicate edge identity: {0:?}")]
    DuplicateEdge(FactReference),
    /// A budget or neighborhood radius is zero or exceeds its hard cap.
    #[error("{name} must be between 1 and {maximum}, got {requested}")]
    InvalidLimit {
        /// Request field that failed validation.
        name: &'static str,
        /// Supplied limit.
        requested: usize,
        /// Supported upper bound.
        maximum: usize,
    },
    /// A neighborhood anchor was not present in the snapshot.
    #[error("unknown neighborhood anchor: {0}")]
    UnknownAnchor(String),
    /// A cursor belongs to different source content.
    #[error("cursor belongs to a different snapshot")]
    CursorSnapshotMismatch,
    /// A cursor belongs to a different normalized selection.
    #[error("cursor belongs to a different selection")]
    CursorSelectionMismatch,
    /// A cursor points beyond its selection.
    #[error("cursor offset {offset} exceeds selected fact count {total_selected}")]
    InvalidCursorOffset {
        /// Supplied position.
        offset: usize,
        /// Total facts available in the bound selection.
        total_selected: usize,
    },
    /// The next fact plus its envelope, or an empty envelope, cannot fit.
    #[error("response needs {required_bytes} bytes but budget is {max_bytes}")]
    ResponseBudgetExceeded {
        /// Caller-supplied successful response budget.
        max_bytes: usize,
        /// Size of the empty page or next single-fact page, including cursor.
        required_bytes: usize,
    },
    /// JSON encoding failure, kept explicit rather than emitting partial data.
    #[error("context serialization: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Serialize)]
struct Selection {
    scope: QueryScope,
    kind: Option<FactKind>,
    labels: BTreeSet<String>,
}

/// Immutable owned read projection of a complete recovered graph.
#[derive(Debug)]
pub struct ContextSnapshot {
    id: String,
    facts: Vec<ContextFact>,
    node_ids: BTreeSet<String>,
    neighbors: BTreeMap<String, BTreeSet<String>>,
}

impl ContextSnapshot {
    /// Construct a snapshot from graph copies, rejecting duplicate identities.
    ///
    /// The versioned identity includes every original property, including raw
    /// provenance, before response sanitization. Object-key order and input
    /// fact order are ignored; array order remains part of content identity.
    pub fn new(mut nodes: Vec<Node>, mut edges: Vec<Edge>) -> Result<Self, ContextError> {
        nodes.sort_by(|left, right| left.id.cmp(&right.id));
        edges.sort_by(|left, right| {
            (&left.src, &left.label, &left.dst).cmp(&(&right.src, &right.label, &right.dst))
        });
        if let Some(pair) = nodes.windows(2).find(|pair| pair[0].id == pair[1].id) {
            return Err(ContextError::DuplicateNode(pair[0].id.clone()));
        }
        if let Some(pair) = edges.windows(2).find(|pair| {
            (&pair[0].src, &pair[0].label, &pair[0].dst)
                == (&pair[1].src, &pair[1].label, &pair[1].dst)
        }) {
            return Err(ContextError::DuplicateEdge(edge_reference(&pair[0])));
        }
        let id = content_identity("context-v1", &(&nodes, &edges))?;
        let node_ids: BTreeSet<String> = nodes.iter().map(|node| node.id.clone()).collect();
        let mut neighbors: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for edge in &edges {
            // A graph slice may contain external references. They stay visible
            // in all-fact reads but cannot manufacture neighborhood nodes.
            if node_ids.contains(&edge.src) && node_ids.contains(&edge.dst) {
                neighbors
                    .entry(edge.src.clone())
                    .or_default()
                    .insert(edge.dst.clone());
                neighbors
                    .entry(edge.dst.clone())
                    .or_default()
                    .insert(edge.src.clone());
            }
        }
        let mut facts = Vec::with_capacity(nodes.len() + edges.len());
        facts.extend(nodes.into_iter().map(|node| {
            ContextFact::new(FactReference::Node { id: node.id }, node.label, node.props)
        }));
        facts.extend(
            edges
                .into_iter()
                .map(|edge| ContextFact::new(edge_reference(&edge), edge.label, edge.props)),
        );
        Ok(Self {
            id,
            facts,
            node_ids,
            neighbors,
        })
    }

    /// Versioned identity of the complete canonical input graph.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Select a stable page within the requested item and whole-response byte
    /// budgets. Oversized facts are never skipped to fit later facts.
    pub fn query(&self, request: QueryRequest) -> Result<QueryResponse, ContextError> {
        request.validate_limits()?;
        let selection = Selection {
            scope: request.scope,
            kind: request.kind,
            labels: request.labels.into_iter().collect(),
        };
        let neighborhood = match &selection.scope {
            QueryScope::All => None,
            QueryScope::Neighborhood { anchor, hops } => {
                if !self.node_ids.contains(anchor) {
                    return Err(ContextError::UnknownAnchor(anchor.clone()));
                }
                Some(self.neighborhood(anchor, *hops))
            }
        };
        let selection_id = content_identity("selection-v1", &selection)?;
        let selected: Vec<&ContextFact> = self
            .facts
            .iter()
            .filter(|fact| {
                selection
                    .kind
                    .is_none_or(|kind| fact.reference.kind() == kind)
                    && (selection.labels.is_empty() || selection.labels.contains(&fact.label))
                    && neighborhood
                        .as_ref()
                        .is_none_or(|ids| match &fact.reference {
                            FactReference::Node { id } => ids.contains(id),
                            FactReference::Edge {
                                source,
                                destination,
                                ..
                            } => ids.contains(source) && ids.contains(destination),
                        })
            })
            .collect();
        let offset = match request.cursor {
            Some(cursor) => {
                if cursor.snapshot_id != self.id {
                    return Err(ContextError::CursorSnapshotMismatch);
                }
                if cursor.selection_id != selection_id {
                    return Err(ContextError::CursorSelectionMismatch);
                }
                if cursor.offset > selected.len() {
                    return Err(ContextError::InvalidCursorOffset {
                        offset: cursor.offset,
                        total_selected: selected.len(),
                    });
                }
                cursor.offset
            }
            None => 0,
        };
        let cursor_at = |offset| {
            (offset < selected.len()).then(|| QueryCursor {
                snapshot_id: self.id.clone(),
                selection_id: selection_id.clone(),
                offset,
            })
        };
        let mut response = QueryResponse {
            snapshot_id: self.id.clone(),
            view: ContextView::RecoveredGraph,
            total_selected: selected.len(),
            facts: Vec::new(),
            next_cursor: cursor_at(offset),
        };
        for fact in selected.iter().skip(offset).take(request.max_facts) {
            response.facts.push((*fact).clone());
            response.next_cursor = cursor_at(offset + response.facts.len());
            let bytes = serde_json::to_vec(&response)?.len();
            if bytes > request.max_bytes {
                response.facts.pop();
                if response.facts.is_empty() {
                    return Err(ContextError::ResponseBudgetExceeded {
                        max_bytes: request.max_bytes,
                        required_bytes: bytes,
                    });
                }
                response.next_cursor = cursor_at(offset + response.facts.len());
                return Ok(response);
            }
        }
        // Empty selections and end-position cursors still have an envelope.
        let bytes = serde_json::to_vec(&response)?.len();
        if bytes > request.max_bytes {
            return Err(ContextError::ResponseBudgetExceeded {
                max_bytes: request.max_bytes,
                required_bytes: bytes,
            });
        }
        Ok(response)
    }

    fn neighborhood(&self, anchor: &str, hops: usize) -> BTreeSet<String> {
        let mut visited = BTreeSet::from([anchor.to_string()]);
        let mut frontier = visited.clone();
        for _ in 0..hops {
            let mut next = BTreeSet::new();
            for node in frontier {
                if let Some(neighbors) = self.neighbors.get(&node) {
                    for neighbor in neighbors {
                        if visited.insert(neighbor.clone()) {
                            next.insert(neighbor.clone());
                        }
                    }
                }
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }
        visited
    }
}

fn edge_reference(edge: &Edge) -> FactReference {
    FactReference::Edge {
        source: edge.src.clone(),
        label: edge.label.clone(),
        destination: edge.dst.clone(),
    }
}

fn validate_limit(
    name: &'static str,
    requested: usize,
    maximum: usize,
) -> Result<(), ContextError> {
    if requested == 0 || requested > maximum {
        return Err(ContextError::InvalidLimit {
            name,
            requested,
            maximum,
        });
    }
    Ok(())
}

fn content_identity(prefix: &str, content: &impl Serialize) -> Result<String, ContextError> {
    let canonical = canonicalize(serde_json::to_value(content)?);
    let bytes = serde_json::to_vec(&(prefix, canonical))?;
    Ok(format!("{prefix}:{}", blake3::hash(&bytes).to_hex()))
}

fn canonicalize(value: Value) -> Value {
    match value {
        Value::Object(properties) => {
            let sorted: BTreeMap<String, Value> = properties
                .into_iter()
                .map(|(key, value)| (key, canonicalize(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize).collect()),
        scalar => scalar,
    }
}

#[cfg(test)]
mod tests;
