//! Receipt-qualified preparation for the existing bounded resolver (SPEC-09).
//! Source leases end when preparation returns; providers receive owned data.

use agents::{
    AgentCandidate, AgentEvidence, AgentTask, InputClosureStatus, PreparedAgentTask,
    PrimarySourceScope, SelectionLimits, SelectionOmission, SelectionOmissionReason,
    SelectionReport, SelectionStopReason, TaskCaptureFileRef, TaskCaptureSpanRef, TaskEvidenceKind,
    TaskEvidenceOrigin, TaskFactKey, TaskFactSelection, TaskRangeRole, TaskSourceAssociation,
    TaskSourceBasisV2,
};
use core_graph::source::{FactKey, FactSourceSelection, FactSourceState};
use core_graph::{Edge, Node};
use core_prov::ConfidenceTier;
use std::collections::BTreeMap;

use crate::AppState;
use crate::primary_source::{PinnedReadError, RetentionGuard};
use crate::source_access::SourceOperation;
use crate::sources::RegisteredSource;

pub(crate) mod assessment;
mod planning;

use planning::{MAX_CANDIDATES, MAX_REQUESTS, TaskPlan};

const CHANGED: &str = "Task evidence changed during preparation; refresh and prepare it again.";
const INVALID: &str = "Task source selection is invalid; no source was substituted.";

pub(crate) fn task_fact(fact: &FactKey) -> TaskFactKey {
    match fact {
        FactKey::Node { id } => TaskFactKey::Node { id: id.clone() },
        FactKey::Edge {
            source,
            label,
            destination,
        } => TaskFactKey::Edge {
            source: source.clone(),
            label: label.clone(),
            destination: destination.clone(),
        },
    }
}

pub(crate) fn graph_fact(fact: &TaskFactKey) -> FactKey {
    match fact {
        TaskFactKey::Node { id } => FactKey::Node { id: id.clone() },
        TaskFactKey::Edge {
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

fn selection(value: &FactSourceSelection) -> Result<TaskFactSelection, String> {
    let (fact_digest, binding) = match &value.state {
        FactSourceState::Absent { fact_digest } => (fact_digest, None),
        FactSourceState::Present {
            fact_digest,
            binding,
        } => (
            fact_digest,
            Some(TaskSourceAssociation {
                repo_key: binding.repo_key.clone(),
                receipt_id: binding.receipt_id.clone(),
                emitted_fact_digest: binding.emitted_fact_digest.clone(),
            }),
        ),
        FactSourceState::Missing | FactSourceState::Invalid { .. } => return Err(INVALID.into()),
    };
    Ok(TaskFactSelection {
        fact: task_fact(&value.fact),
        fact_digest: fact_digest.clone(),
        binding,
    })
}

struct SourceGuards {
    registrations: BTreeMap<String, Result<RegisteredSource, PinnedReadError>>,
    legacy: BTreeMap<String, Result<SourceOperation, PinnedReadError>>,
    retained: BTreeMap<String, Result<RetentionGuard, PinnedReadError>>,
}

impl SourceGuards {
    fn acquire(
        state: &AppState,
        plan: &TaskPlan,
        selections: &BTreeMap<FactKey, FactSourceSelection>,
    ) -> Self {
        let mut registrations = BTreeMap::new();
        let mut legacy_sources = BTreeMap::new();
        let mut captured_sources = BTreeMap::new();
        for request in &plan.requests {
            let Some(reference) = &request.reference else {
                continue;
            };
            let Some(selected) = selections.get(&request.fact) else {
                continue;
            };
            let (repo, captured) = match &selected.state {
                FactSourceState::Absent { .. } => (&reference.repo, false),
                FactSourceState::Present { binding, .. } => (&binding.repo_key, true),
                _ => continue,
            };
            let registration = registrations.entry(repo.clone()).or_insert_with(|| {
                state
                    .sources
                    .lock()
                    .map_err(|_| PinnedReadError::Operational)?
                    .get_by_repo(repo)
                    .map_err(|_| PinnedReadError::Operational)?
                    .ok_or(PinnedReadError::Unavailable)
            });
            if let Ok(source) = registration {
                let requested = if captured {
                    &mut captured_sources
                } else {
                    &mut legacy_sources
                };
                requested.insert(source.source_id.clone(), source.clone());
            }
        }
        // Every reservation is independent. A failed unused tail or failed
        // managed checkout cannot invalidate an unrelated retained-byte lease.
        let legacy = legacy_sources
            .into_iter()
            .map(|(id, source)| {
                let guard = SourceOperation::acquire(&state.sources, [(source, false)])
                    .map_err(|_| PinnedReadError::Operational);
                (id, guard)
            })
            .collect();
        let retained = captured_sources
            .into_keys()
            .map(|id| {
                let guard = state.primary_sources.task_guard(&id);
                (id, guard)
            })
            .collect();
        Self {
            registrations,
            legacy,
            retained,
        }
    }

    fn registered(&self, repo: &str) -> Result<&RegisteredSource, PinnedReadError> {
        self.registrations
            .get(repo)
            .ok_or(PinnedReadError::Unavailable)?
            .as_ref()
            .map_err(|error| *error)
    }

    fn captured(
        &self,
        repo: &str,
    ) -> Result<(&RegisteredSource, &RetentionGuard), PinnedReadError> {
        let source = self.registered(repo)?;
        let guard = self
            .retained
            .get(&source.source_id)
            .ok_or(PinnedReadError::Operational)?
            .as_ref()
            .map_err(|error| *error)?;
        Ok((source, guard))
    }

    fn legacy_text(&self, reference: &core_prov::EvidenceRef) -> Option<String> {
        let source = self.registered(&reference.repo).ok()?;
        let operation = self.legacy.get(&source.source_id)?.as_ref().ok()?;
        let root = operation.root(&reference.repo).ok()?;
        crate::evidence::read_span_exact(
            root,
            &reference.path,
            &(reference.byte_start..reference.byte_end),
        )
        .ok()
    }
}

fn report(plan: &TaskPlan) -> SelectionReport {
    SelectionReport {
        metadata_lookahead: plan.requests.len(),
        acquisition_attempts: 0,
        supplied_evidence: 0,
        supplied_candidates: 0,
        captured_validation_bytes: 0,
        limits: SelectionLimits::default(),
        omissions: Vec::new(),
        metadata_preselected_not_read: plan.requests.len(),
        unread_tail: plan.unplanned_candidates > 0,
        stop_reason: SelectionStopReason::NeighborhoodExhausted,
    }
}

#[derive(Debug)]
pub(super) struct TaskPreparationError {
    pub message: String,
    pub selection: Option<Box<SelectionReport>>,
}

impl From<String> for TaskPreparationError {
    fn from(message: String) -> Self {
        Self {
            message,
            selection: None,
        }
    }
}
impl From<&str> for TaskPreparationError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}
impl std::fmt::Display for TaskPreparationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)?;
        if let Some(r) = &self.selection {
            let stop = match r.stop_reason {
                SelectionStopReason::CandidateLimit => "candidate_limit",
                SelectionStopReason::NeighborhoodExhausted => "neighborhood_exhausted",
                SelectionStopReason::AttemptLimit => "attempt_limit",
                SelectionStopReason::ValidationByteLimit => "validation_byte_limit",
                SelectionStopReason::ParticipatingSourceUnavailable => {
                    "participating_source_unavailable"
                }
                SelectionStopReason::InvalidSelection => "invalid_selection",
                SelectionStopReason::RequiredMembershipMissing => "required_membership_missing",
            };
            write!(
                f,
                " Selection stopped: {stop}. Planned metadata lookahead {}/{}; evidence requests {}/{}; supplied evidence {}/{} and candidates {}/{}; captured-file validation {}/{} bytes. Span/combined text caps: {}/{} bytes; selected-fact cap: {}. Preselected evidence not read: {}; uninspected tail: {}.",
                r.metadata_lookahead,
                r.limits.metadata_lookahead,
                r.acquisition_attempts,
                r.limits.acquisition_attempts,
                r.supplied_evidence,
                r.limits.evidence,
                r.supplied_candidates,
                r.limits.candidates,
                r.captured_validation_bytes,
                r.limits.captured_validation_bytes,
                r.limits.span_bytes,
                r.limits.total_evidence_bytes,
                r.limits.selected_facts,
                r.metadata_preselected_not_read,
                r.unread_tail
            )?;
            for omitted in &r.omissions {
                let reason = match omitted.reason {
                    SelectionOmissionReason::MissingCitation => "missing citation",
                    SelectionOmissionReason::LegacyReadUnavailable => {
                        "unverified source unreadable"
                    }
                };
                // Arbitrary fact identifiers are intentionally omitted from the
                // error display; the internal bounded report retains them.
                write!(f, " Omitted request {}: {reason}.", omitted.request_index)?;
            }
        }
        Ok(())
    }
}
impl std::error::Error for TaskPreparationError {}

fn failure(report: &SelectionReport, reason: &str) -> TaskPreparationError {
    let mut selection = report.clone();
    if matches!(
        selection.stop_reason,
        SelectionStopReason::CandidateLimit
            | SelectionStopReason::NeighborhoodExhausted
            | SelectionStopReason::AttemptLimit
    ) {
        selection.stop_reason = if reason == CHANGED || reason == INVALID {
            SelectionStopReason::InvalidSelection
        } else {
            SelectionStopReason::ParticipatingSourceUnavailable
        };
    }
    TaskPreparationError {
        message: reason.into(),
        selection: Some(Box::new(selection)),
    }
}

/// Own one coherent graph observation, then prepare its bounded evidence task.
pub(crate) fn prepare(
    state: &AppState,
    gap_id: &str,
    action_id: &str,
) -> Result<PreparedAgentTask, String> {
    let graph = state
        .graph
        .lock()
        .map_err(|_| INVALID)?
        .read_snapshot()
        .map_err(|_| INVALID)?;
    prepare_from_snapshot(state, graph, gap_id, action_id, || {}).map_err(|error| error.to_string())
}

fn prepare_from_snapshot(
    state: &AppState,
    graph: (Vec<Node>, Vec<Edge>),
    gap_id: &str,
    action_id: &str,
    after_guards: impl FnOnce(),
) -> Result<PreparedAgentTask, TaskPreparationError> {
    let plan = planning::plan(&graph.0, &graph.1, gap_id, action_id)?;
    let mut selection_report = report(&plan);
    let mut keys: Vec<_> = plan
        .requests
        .iter()
        .map(|request| request.fact.clone())
        .collect();
    if let Some((slot, _)) = &plan.slot {
        keys.push(slot.clone());
    }
    let preliminary = state
        .graph
        .lock()
        .map_err(|_| failure(&selection_report, INVALID))?
        .read_source_selection_snapshot(&keys)
        .map_err(|_| failure(&selection_report, INVALID))?;
    if preliminary.graph != graph {
        return Err(failure(&selection_report, CHANGED));
    }
    let preliminary: BTreeMap<_, _> = preliminary
        .selections
        .into_iter()
        .map(|value| (value.fact.clone(), value))
        .collect();
    let guards = SourceGuards::acquire(state, &plan, &preliminary);
    after_guards();
    let authoritative = state
        .graph
        .lock()
        .map_err(|_| failure(&selection_report, INVALID))?
        .read_source_selection_snapshot(&keys)
        .map_err(|_| failure(&selection_report, INVALID))?;
    if authoritative.graph != graph {
        return Err(failure(&selection_report, CHANGED));
    }
    let authoritative: BTreeMap<_, _> = authoritative
        .selections
        .into_iter()
        .map(|value| (value.fact.clone(), value))
        .collect();
    let mut selected = BTreeMap::new();
    let mut include = |fact: &FactKey, digest: &str| -> Result<TaskFactSelection, String> {
        let value = authoritative.get(fact).ok_or(INVALID)?;
        if preliminary.get(fact) != Some(value) {
            return Err(CHANGED.into());
        }
        let entry = selection(value)?;
        if entry.fact_digest != digest {
            return Err(CHANGED.into());
        }
        selected.insert(fact.clone(), entry.clone());
        Ok(entry)
    };
    if let Some((slot, digest)) = &plan.slot {
        include(slot, digest).map_err(|error| failure(&selection_report, &error))?;
    }

    let mut evidence = Vec::new();
    let mut origins = Vec::new();
    let mut candidates: Vec<AgentCandidate> = Vec::new();
    let mut source_evidence_ids = Vec::new();
    for (request_index, request) in plan.requests.iter().enumerate() {
        if candidates.len() == MAX_CANDIDATES {
            selection_report.stop_reason = SelectionStopReason::CandidateLimit;
            break;
        }
        selection_report.acquisition_attempts += 1;
        selection_report.metadata_preselected_not_read -= 1;
        let entry = include(&request.fact, &request.fact_digest)
            .map_err(|error| failure(&selection_report, &error))?;
        let Some(reference) = &request.reference else {
            selection_report.omissions.push(SelectionOmission {
                request_index,
                fact: task_fact(&request.fact),
                reason: SelectionOmissionReason::MissingCitation,
            });
            continue;
        };
        let (text, origin) = if let Some(binding) = &entry.binding {
            let (source, guard) = guards
                .captured(&binding.repo_key)
                .map_err(|error| failure(&selection_report, &error.to_string()))?;
            let receipt = state
                .primary_sources
                .task_receipt(
                    guard,
                    &request.fact,
                    &entry.fact_digest,
                    &binding.repo_key,
                    &binding.receipt_id,
                )
                .map_err(|error| failure(&selection_report, &error.to_string()))?;
            let (inventory_index, range) = receipt
                .ranges()
                .iter()
                .enumerate()
                .find(|(_, range)| {
                    range.role == adapters_lang_ts::captured::RangeRole::Provenance
                        && range.index == 0
                        && &range.evidence == reference
                })
                .ok_or_else(|| failure(&selection_report, INVALID))?;
            let span_bytes = range
                .captured
                .byte_end
                .checked_sub(range.captured.byte_start)
                .filter(|bytes| *bytes > 0 && *bytes <= selection_report.limits.span_bytes as u64)
                .ok_or_else(|| {
                    failure(
                        &selection_report,
                        "The original captured span exceeds the task limit.",
                    )
                })?;
            let charged = selection_report
                .captured_validation_bytes
                .checked_add(range.captured.file.byte_len)
                .filter(|bytes| *bytes <= selection_report.limits.captured_validation_bytes);
            let Some(charged) = charged else {
                selection_report.stop_reason = SelectionStopReason::ValidationByteLimit;
                return Err(failure(
                    &selection_report,
                    "Captured-file validation budget exhausted.",
                ));
            };
            selection_report.captured_validation_bytes = charged;
            let text = state
                .primary_sources
                .task_text(guard, &receipt, inventory_index)
                .map_err(|error| failure(&selection_report, &error.to_string()))?;
            if text.len() as u64 != span_bytes {
                return Err(failure(&selection_report, INVALID));
            }
            let file = &range.captured.file;
            let origin = TaskEvidenceKind::CapturedPrimarySource {
                registered_source_id: source.source_id.clone(),
                receipt_id: receipt.id().into(),
                receipt_inventory_index: inventory_index as u32,
                captured: TaskCaptureSpanRef {
                    file: TaskCaptureFileRef {
                        source_id: file.source_id.as_str().into(),
                        capture_id: file.capture_id.clone(),
                        path: file.path.clone(),
                        digest: file.digest.clone(),
                        byte_len: file.byte_len,
                    },
                    byte_start: range.captured.byte_start,
                    byte_end: range.captured.byte_end,
                },
                scope: PrimarySourceScope::PrimarySourceOnly,
                input_closure: InputClosureStatus::InputClosureNotEstablished,
            };
            (text, origin)
        } else {
            let Some(text) = guards.legacy_text(reference) else {
                selection_report.omissions.push(SelectionOmission {
                    request_index,
                    fact: task_fact(&request.fact),
                    reason: SelectionOmissionReason::LegacyReadUnavailable,
                });
                continue;
            };
            (text, TaskEvidenceKind::WorkingTreeUnverified)
        };
        let evidence_id = format!("E{}", evidence.len() + 1);
        evidence.push(AgentEvidence {
            id: evidence_id.clone(),
            source: reference.clone(),
            text,
        });
        origins.push(TaskEvidenceOrigin {
            evidence_id: evidence_id.clone(),
            fact: task_fact(&request.fact),
            role: TaskRangeRole::Provenance,
            index: 0,
            origin,
        });
        if let Some(candidate) = &request.candidate {
            let mut candidate = candidate.clone();
            candidate.evidence_ids.push(evidence_id);
            candidates.push(candidate);
        } else {
            source_evidence_ids.push(evidence_id);
        }
        selection_report.supplied_evidence = evidence.len();
        selection_report.supplied_candidates = candidates.len();
    }
    if candidates.len() == MAX_CANDIDATES {
        selection_report.stop_reason = SelectionStopReason::CandidateLimit;
    } else if selection_report.acquisition_attempts == MAX_REQUESTS && plan.unplanned_candidates > 0
    {
        selection_report.stop_reason = SelectionStopReason::AttemptLimit;
    }
    if source_evidence_ids.is_empty() || candidates.is_empty() {
        selection_report.stop_reason = SelectionStopReason::RequiredMembershipMissing;
        return Err(failure(
            &selection_report,
            "The task has no readable source or candidate evidence.",
        ));
    }
    let snapshot = context_hub::ContextSnapshot::new(graph.0, graph.1)
        .map_err(|_| failure(&selection_report, INVALID))?;
    let task = AgentTask {
        action_id: plan.action_id,
        gap_id: plan.gap_id,
        source_id: plan.source_id,
        edge_label: plan.edge_label,
        existing_confidence: ConfidenceTier::Gap,
        source_evidence_ids,
        evidence,
        candidates,
    };
    let validation_report = selection_report.clone();
    let basis = TaskSourceBasisV2 {
        schema_version: 2,
        graph_snapshot_id: snapshot.id().into(),
        selected_facts: selected.into_values().collect(),
        evidence: origins,
        selection: selection_report,
    };
    // Leases are explicitly out of scope before a caller can invoke a provider.
    drop(guards);
    PreparedAgentTask::new(task, basis).map_err(|_| {
        let mut report = validation_report;
        report.stop_reason = SelectionStopReason::InvalidSelection;
        failure(
            &report,
            "Prepared task exceeds its bounded evidence or metadata contract.",
        )
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod completion_tests;
