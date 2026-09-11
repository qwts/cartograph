use super::{AcquisitionSink, HostError, evidence::EvidenceGuards};
use crate::AppState;
use agents::investigation::*;
use agents::{AgentEvidence, TaskFactKey, TaskFactSelection, TaskRangeRole, TaskSourceAssociation};
use context_hub::{
    ContextSnapshot, FactKind, FactReference, QueryCursor, QueryRequest, QueryScope,
};
use core_graph::source::{FactKey, FactSourceSelection, FactSourceState};
use core_graph::{Edge, Node, SnapshotReadLimits};
use core_prov::content_hash;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// One owner holds the original raw graph for exact comparisons and the frozen
/// authorized query projection. Nothing is rebuilt from live context mid-task.
pub(super) struct FrozenContext {
    pub(super) graph: (Vec<Node>, Vec<Edge>),
    pub(super) snapshot: ContextSnapshot,
    pub(super) ledger: InvestigationInputLedger,
    pub(super) query_pages: Vec<String>,
    pub(super) evidence: Vec<AgentEvidence>,
}

#[derive(Serialize)]
struct InvestigationQueryPage {
    context: context_hub::QueryResponse,
    evidence_options: Vec<EvidenceOptions>,
}

#[derive(Serialize)]
pub(super) struct EvidenceOptions {
    pub fact: TaskFactKey,
    pub origin: &'static str,
    pub ranges: Vec<EvidenceOption>,
}

#[derive(Serialize)]
pub(super) struct EvidenceOption {
    pub role: TaskRangeRole,
    pub index: u32,
    pub source: core_prov::EvidenceRef,
}

impl FrozenContext {
    pub(super) fn prepare(
        state: &AppState,
        request: &StartInvestigationRequest,
        history: &[String],
    ) -> Result<Self, HostError> {
        request.validate()?;
        let graph = state
            .graph
            .lock()
            .map_err(|_| HostError::Operational)?
            .read_snapshot_bounded(SnapshotReadLimits::default())
            .map_err(graph_error)?;
        let complete = ContextSnapshot::new(graph.0.clone(), graph.1.clone())
            .map_err(|_| HostError::InvalidInput)?;
        let graph_id = complete.id().to_owned();
        if request
            .expected_graph_revision
            .as_ref()
            .is_some_and(|expected| expected != &graph_id)
        {
            return Err(HostError::InputChanged);
        }
        let snapshot = match &request.scope {
            InvestigationScope::All => complete,
            InvestigationScope::Neighborhood { .. } => {
                // Reuse the service's actual scope selection rather than a
                // subtly different application-specific graph traversal.
                let mut references = BTreeSet::new();
                let mut cursor = None;
                loop {
                    let page = complete
                        .query(QueryRequest {
                            scope: scope(&request.scope),
                            kind: None,
                            labels: vec![],
                            max_facts: context_hub::MAX_FACTS,
                            max_bytes: context_hub::MAX_RESPONSE_BYTES,
                            cursor,
                        })
                        .map_err(context_error)?;
                    references.extend(page.facts.into_iter().map(|f| f.reference));
                    let Some(next) = page.next_cursor else {
                        break;
                    };
                    cursor = Some(next);
                }
                let nodes = graph
                    .0
                    .iter()
                    .filter(|n| references.contains(&FactReference::Node { id: n.id.clone() }))
                    .cloned()
                    .collect();
                let edges = graph
                    .1
                    .iter()
                    .filter(|e| {
                        references.contains(&FactReference::Edge {
                            source: e.src.clone(),
                            label: e.label.clone(),
                            destination: e.dst.clone(),
                        })
                    })
                    .cloned()
                    .collect();
                ContextSnapshot::new(nodes, edges).map_err(|_| HostError::InvalidInput)?
            }
        };
        let history_bytes = if history.is_empty() {
            Vec::new()
        } else {
            bounded_json(history, 16 * 1024)?
        };
        let ledger = InvestigationInputLedger {
            schema_version: 1,
            graph_snapshot_id: graph_id,
            scope_snapshot_id: snapshot.id().into(),
            revision: 0,
            selected_facts: vec![],
            receipt_references: vec![],
            citations: vec![],
            queries: vec![],
            supplied_history_hash: (!history_bytes.is_empty())
                .then(|| content_hash(&history_bytes)),
            supplied_history_bytes: history_bytes.len(),
        };
        ledger.validate()?;
        Ok(Self {
            graph,
            snapshot,
            ledger,
            query_pages: vec![],
            evidence: vec![],
        })
    }

    pub(super) fn query(
        &mut self,
        state: &AppState,
        query: InvestigationQuery,
        sink: &mut impl AcquisitionSink,
    ) -> Result<(), HostError> {
        query.validate()?;
        let page = self
            .snapshot
            .query(QueryRequest {
                scope: scope(&query.scope),
                kind: query.kind.map(|k| match k {
                    InvestigationFactKind::Node => FactKind::Node,
                    InvestigationFactKind::Edge => FactKind::Edge,
                }),
                labels: query.labels.clone(),
                max_facts: query.max_facts,
                max_bytes: query.max_bytes,
                cursor: query.cursor.as_ref().map(|c| QueryCursor {
                    snapshot_id: c.snapshot_id.clone(),
                    selection_id: c.selection_id.clone(),
                    offset: c.offset,
                }),
            })
            .map_err(context_error)?;
        let mut keys = self
            .ledger
            .selected_facts
            .iter()
            .map(|s| crate::task_evidence::graph_fact(&s.fact))
            .collect::<BTreeSet<_>>();
        keys.extend(page.facts.iter().map(|f| graph_reference(&f.reference)));
        if keys.len() > MAX_SELECTED_FACTS {
            return Err(HostError::LimitExceeded);
        }
        let keys = keys.into_iter().collect::<Vec<_>>();
        let preliminary = self.select(state, &keys)?;
        let guards = EvidenceGuards::acquire(state, &preliminary)?;
        let authoritative = self.select(state, &keys)?;
        if authoritative != preliminary {
            return Err(HostError::InputChanged);
        }
        let mut ledger = self.ledger.clone();
        ledger.revision = ledger
            .revision
            .checked_add(1)
            .ok_or(HostError::LimitExceeded)?;
        ledger.selected_facts = authoritative.values().cloned().collect();
        ledger.receipt_references = guards.references();
        let mut options = Vec::new();
        for fact in &page.facts {
            let key = graph_reference(&fact.reference);
            let selected = authoritative.get(&key).ok_or(HostError::InvalidInput)?;
            if !ledger.citations.iter().any(|c| {
                c.fact == selected.fact
                    && matches!(c.origin, InvestigationEvidenceOrigin::GraphMetadata)
            }) {
                let index = ledger
                    .citations
                    .iter()
                    .filter(|c| matches!(c.origin, InvestigationEvidenceOrigin::GraphMetadata))
                    .count()
                    + 1;
                ledger.citations.push(InvestigationCitation {
                    citation_id: format!("fact-{index}"),
                    fact: selected.fact.clone(),
                    fact_digest: selected.fact_digest.clone(),
                    source: None,
                    role: None,
                    index: None,
                    text_hash: None,
                    origin: InvestigationEvidenceOrigin::GraphMetadata,
                });
            }
            options.push(guards.options(state, self, selected)?);
        }
        let response = InvestigationQueryPage {
            context: page,
            evidence_options: options,
        };
        let raw = bounded_json(&response, query.max_bytes)?;
        let total_bytes = ledger
            .queries
            .iter()
            .map(|q| q.response_bytes)
            .sum::<usize>()
            .checked_add(raw.len())
            .ok_or(HostError::LimitExceeded)?;
        if total_bytes > 96 * 1024 {
            return Err(HostError::LimitExceeded);
        }
        ledger.queries.push(InvestigationQueryManifest {
            query,
            response_hash: content_hash(&raw),
            response_bytes: raw.len(),
            returned_facts: response.context.facts.len(),
            total_selected: response.context.total_selected,
            has_more: response.context.next_cursor.is_some(),
        });
        ledger.validate()?;
        let mut usage = sink.usage().clone();
        usage.selected_facts = ledger.selected_facts.len();
        sink.publish_input(&ledger, usage)?;
        // The guard remains live through the durable receipt-reference commit.
        self.ledger = ledger;
        self.query_pages
            .push(String::from_utf8(raw).map_err(|_| HostError::InvalidInput)?);
        Ok(())
    }

    pub(super) fn select(
        &self,
        state: &AppState,
        keys: &[FactKey],
    ) -> Result<BTreeMap<FactKey, TaskFactSelection>, HostError> {
        let read = state
            .graph
            .lock()
            .map_err(|_| HostError::Operational)?
            .read_source_selection_snapshot_bounded(keys, SnapshotReadLimits::default())
            .map_err(graph_error)?;
        if read.graph != self.graph {
            return Err(HostError::InputChanged);
        }
        let selected = read
            .selections
            .iter()
            .map(|s| Ok((s.fact.clone(), selection(s)?)))
            .collect::<Result<BTreeMap<_, _>, HostError>>()?;
        for prior in &self.ledger.selected_facts {
            if selected.get(&crate::task_evidence::graph_fact(&prior.fact)) != Some(prior) {
                return Err(HostError::InputChanged);
            }
        }
        Ok(selected)
    }

    pub(super) fn props(&self, fact: &TaskFactKey) -> Result<&serde_json::Value, HostError> {
        match fact {
            TaskFactKey::Node { id } => self.graph.0.iter().find(|n| &n.id == id).map(|n| &n.props),
            TaskFactKey::Edge {
                source,
                label,
                destination,
            } => self
                .graph
                .1
                .iter()
                .find(|e| &e.src == source && &e.label == label && &e.dst == destination)
                .map(|e| &e.props),
        }
        .ok_or(HostError::InvalidInput)
    }
}

fn selection(value: &FactSourceSelection) -> Result<TaskFactSelection, HostError> {
    let (fact_digest, binding) = match &value.state {
        FactSourceState::Absent { fact_digest } => (fact_digest.clone(), None),
        FactSourceState::Present {
            fact_digest,
            binding,
        } => (
            fact_digest.clone(),
            Some(TaskSourceAssociation {
                repo_key: binding.repo_key.clone(),
                receipt_id: binding.receipt_id.clone(),
                emitted_fact_digest: binding.emitted_fact_digest.clone(),
            }),
        ),
        _ => return Err(HostError::InvalidInput),
    };
    Ok(TaskFactSelection {
        fact: crate::task_evidence::task_fact(&value.fact),
        fact_digest,
        binding,
    })
}
fn graph_reference(reference: &FactReference) -> FactKey {
    match reference {
        FactReference::Node { id } => FactKey::Node { id: id.clone() },
        FactReference::Edge {
            source,
            label,
            destination,
        } => FactKey::Edge {
            source: source.clone(),
            label: label.clone(),
            destination: destination.clone(),
        },
    }
}
fn scope(value: &InvestigationScope) -> QueryScope {
    match value {
        InvestigationScope::All => QueryScope::All,
        InvestigationScope::Neighborhood { anchor, hops } => QueryScope::Neighborhood {
            anchor: anchor.clone(),
            hops: *hops as usize,
        },
    }
}
fn graph_error(error: core_graph::GraphError) -> HostError {
    match error {
        core_graph::GraphError::SnapshotBounds(
            core_graph::SnapshotBoundsError::InvalidSchema
            | core_graph::SnapshotBoundsError::InvalidRow
            | core_graph::SnapshotBoundsError::InvalidJson
            | core_graph::SnapshotBoundsError::InvalidLimits,
        ) => HostError::InvalidInput,
        core_graph::GraphError::SnapshotBounds(_) => HostError::LimitExceeded,
        _ => HostError::Operational,
    }
}
fn context_error(error: context_hub::ContextError) -> HostError {
    match error {
        context_hub::ContextError::ResponseBudgetExceeded { .. } => HostError::LimitExceeded,
        _ => HostError::InvalidInput,
    }
}
pub(super) fn bounded_json(
    value: &(impl Serialize + ?Sized),
    max: usize,
) -> Result<Vec<u8>, HostError> {
    struct Limited {
        bytes: Vec<u8>,
        max: usize,
    }
    impl std::io::Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.max.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::other("investigation byte limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Limited { bytes: vec![], max };
    serde_json::to_writer(&mut writer, value).map_err(|_| HostError::LimitExceeded)?;
    Ok(writer.bytes)
}
