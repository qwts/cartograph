//! Pure, bounded source-selection metadata (SPEC-09). These values establish
//! consistency of a host-supplied task, never authority to read source or a claim
//! of complete producer input closure.

use crate::{AgentError, TaskBasisManifest};
use core_prov::content_hash;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_SOURCE_BASIS_BYTES: usize = 64 * 1024;
pub const MAX_CAPTURED_VALIDATION_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskFactKey {
    Node {
        id: String,
    },
    Edge {
        source: String,
        label: String,
        destination: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSourceAssociation {
    pub repo_key: String,
    pub receipt_id: String,
    pub emitted_fact_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskFactSelection {
    pub fact: TaskFactKey,
    pub fact_digest: String,
    // Required on the wire: null records observed absence, not omitted metadata.
    #[serde(deserialize_with = "required_binding")]
    pub binding: Option<TaskSourceAssociation>,
}
fn required_binding<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<TaskSourceAssociation>, D::Error> {
    Option::deserialize(d)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskRangeRole {
    Provenance,
    RuleExit,
    ConditionBranch,
    ConditionExpression,
    ReturnValue,
    ThrowValue,
    Dependency,
    DependencyDeclaration,
    Redaction,
    DefinitionDeclaration,
    DefinitionUse,
    DefinitionInitializer,
    DefinitionExpression,
    DefinitionDependency,
    DefinitionDependencyDeclaration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCaptureFileRef {
    pub source_id: String,
    pub capture_id: String,
    pub path: String,
    pub digest: String,
    pub byte_len: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCaptureSpanRef {
    pub file: TaskCaptureFileRef,
    pub byte_start: u64,
    pub byte_end: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimarySourceScope {
    PrimarySourceOnly,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputClosureStatus {
    InputClosureNotEstablished,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskEvidenceKind {
    WorkingTreeUnverified,
    CapturedPrimarySource {
        registered_source_id: String,
        receipt_id: String,
        receipt_inventory_index: u32,
        captured: TaskCaptureSpanRef,
        scope: PrimarySourceScope,
        input_closure: InputClosureStatus,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskEvidenceOrigin {
    pub evidence_id: String,
    pub fact: TaskFactKey,
    pub role: TaskRangeRole,
    pub index: u32,
    pub origin: TaskEvidenceKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionLimits {
    pub metadata_lookahead: usize,
    pub acquisition_attempts: usize,
    pub selected_facts: usize,
    pub evidence: usize,
    pub candidates: usize,
    pub span_bytes: usize,
    pub total_evidence_bytes: usize,
    pub captured_validation_bytes: u64,
}
impl Default for SelectionLimits {
    fn default() -> Self {
        Self {
            metadata_lookahead: 64,
            acquisition_attempts: 64,
            selected_facts: 65,
            evidence: 12,
            candidates: 8,
            span_bytes: 8192,
            total_evidence_bytes: 49152,
            captured_validation_bytes: MAX_CAPTURED_VALIDATION_BYTES,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionOmissionReason {
    MissingCitation,
    LegacyReadUnavailable,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionOmission {
    pub request_index: usize,
    pub fact: TaskFactKey,
    pub reason: SelectionOmissionReason,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionStopReason {
    CandidateLimit,
    NeighborhoodExhausted,
    AttemptLimit,
    ValidationByteLimit,
    ParticipatingSourceUnavailable,
    InvalidSelection,
    RequiredMembershipMissing,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionReport {
    pub metadata_lookahead: usize,
    pub acquisition_attempts: usize,
    pub supplied_evidence: usize,
    pub supplied_candidates: usize,
    pub captured_validation_bytes: u64,
    pub limits: SelectionLimits,
    pub omissions: Vec<SelectionOmission>,
    pub metadata_preselected_not_read: usize,
    pub unread_tail: bool,
    pub stop_reason: SelectionStopReason,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSourceBasisV2 {
    pub schema_version: u32,
    pub graph_snapshot_id: String,
    pub selected_facts: Vec<TaskFactSelection>,
    pub evidence: Vec<TaskEvidenceOrigin>,
    pub selection: SelectionReport,
}

pub(crate) fn invalid() -> AgentError {
    AgentError::InvalidTask("invalid prepared source basis".into())
}
pub(crate) fn text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.contains('\0')
}
pub(crate) fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn prefixed(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(hash)
}
fn receipt(value: &str) -> bool {
    prefixed(value, "ts-primary-v1:") || prefixed(value, "ts-primary-v2:")
}
impl TaskFactKey {
    fn valid(&self) -> bool {
        match self {
            Self::Node { id } => text(id, 8192),
            Self::Edge {
                source,
                label,
                destination,
            } => text(source, 8192) && text(label, 256) && text(destination, 8192),
        }
    }
    fn digest_valid(&self, value: &str) -> bool {
        prefixed(
            value,
            match self {
                Self::Node { .. } => "node-v1:",
                Self::Edge { .. } => "edge-v1:",
            },
        )
    }
}
impl TaskSourceBasisV2 {
    /// Canonical metadata identity included verbatim as safe hex in the egress
    /// prompt; redaction cannot collapse two receipt selections to one consent.
    pub fn fingerprint(&self) -> Result<String, AgentError> {
        let bytes = bounded_json(self, MAX_SOURCE_BASIS_BYTES)?;
        Ok(domain_hash(b"cartograph:task-source-basis:v2\0", &bytes))
    }
    pub(crate) fn validate(&self, basis: &TaskBasisManifest) -> Result<(), AgentError> {
        let r = &self.selection;
        let l = &r.limits;
        if self.schema_version != 2
            || !prefixed(&self.graph_snapshot_id, "context-v1:")
            || l != &SelectionLimits::default()
            || r.metadata_lookahead > l.metadata_lookahead
            || r.acquisition_attempts > r.metadata_lookahead
            || r.acquisition_attempts > l.acquisition_attempts
            || r.metadata_preselected_not_read != r.metadata_lookahead - r.acquisition_attempts
            || r.supplied_evidence != basis.evidence.len()
            || r.supplied_candidates != basis.candidates.len()
            || r.supplied_candidates > l.candidates
            || r.supplied_evidence > l.evidence
            || r.captured_validation_bytes > l.captured_validation_bytes
            || r.omissions.len() > r.acquisition_attempts
            || r.acquisition_attempts != r.supplied_evidence + r.omissions.len()
            || self.selected_facts.len() > l.selected_facts
            || self.selected_facts.len() > r.acquisition_attempts + 1
            || self.evidence.len() != basis.evidence.len()
        {
            return Err(invalid());
        }
        match r.stop_reason {
            SelectionStopReason::CandidateLimit if r.supplied_candidates == l.candidates => {}
            SelectionStopReason::NeighborhoodExhausted
                if !r.unread_tail && r.metadata_preselected_not_read == 0 => {}
            SelectionStopReason::AttemptLimit
                if r.acquisition_attempts == l.acquisition_attempts && r.unread_tail => {}
            _ => return Err(invalid()),
        }
        let mut facts = BTreeMap::new();
        let mut previous = None;
        for selected in &self.selected_facts {
            if !selected.fact.valid()
                || !selected.fact.digest_valid(&selected.fact_digest)
                || previous.is_some_and(|p| p >= &selected.fact)
            {
                return Err(invalid());
            }
            if let Some(binding) = &selected.binding
                && (!text(&binding.repo_key, 256)
                    || !receipt(&binding.receipt_id)
                    || binding.emitted_fact_digest != selected.fact_digest)
            {
                return Err(invalid());
            }
            previous = Some(&selected.fact);
            facts.insert(&selected.fact, selected);
        }
        let node = |id: &str| TaskFactKey::Node { id: id.into() };
        if !facts.contains_key(&node(&basis.source_id))
            || !facts.contains_key(&node(&basis.gap_id))
            || basis
                .candidates
                .iter()
                .any(|c| !facts.contains_key(&node(&c.node_id)))
        {
            return Err(invalid());
        }
        let mut seen = BTreeSet::new();
        let mut justified = BTreeSet::new();
        let mut charged = 0u64;
        for origin in &self.evidence {
            let item = basis
                .evidence
                .iter()
                .find(|e| e.id == origin.evidence_id)
                .ok_or_else(invalid)?;
            if !seen.insert(&origin.evidence_id) || origin.index >= 1024 {
                return Err(invalid());
            }
            let selected = facts.get(&origin.fact).ok_or_else(invalid)?;
            justified.insert(&origin.fact);
            // Every membership is attached to its original supporting fact.
            if basis.source_evidence_ids.contains(&origin.evidence_id)
                && origin.fact != node(&basis.source_id)
                && origin.fact != node(&basis.gap_id)
            {
                return Err(invalid());
            }
            if basis.candidates.iter().any(|c| {
                c.evidence_ids.contains(&origin.evidence_id) && origin.fact != node(&c.node_id)
            }) {
                return Err(invalid());
            }
            match &origin.origin {
                TaskEvidenceKind::WorkingTreeUnverified => {
                    if selected.binding.is_some() {
                        return Err(invalid());
                    }
                }
                TaskEvidenceKind::CapturedPrimarySource {
                    registered_source_id,
                    receipt_id,
                    receipt_inventory_index,
                    captured,
                    ..
                } => {
                    let binding = selected.binding.as_ref().ok_or_else(invalid)?;
                    let file = &captured.file;
                    if !text(registered_source_id, 256)
                        || registered_source_id != &file.source_id
                        || receipt_id != &binding.receipt_id
                        || *receipt_inventory_index >= 1024
                        || binding.repo_key != item.source.repo
                        || !prefixed(&file.capture_id, "capture-v1:")
                        || !hash(&file.digest)
                        || !valid_path(&file.path)
                        || file.path != item.source.path
                        || file.byte_len > 16 * 1024 * 1024
                        || captured.byte_start >= captured.byte_end
                        || captured.byte_end > file.byte_len
                        || captured.byte_start != item.source.byte_start
                        || captured.byte_end != item.source.byte_end
                        || captured.byte_end - captured.byte_start != item.text_bytes as u64
                        || captured.byte_end - captured.byte_start > l.span_bytes as u64
                    {
                        return Err(invalid());
                    }
                    if receipt_id.starts_with("ts-primary-v1:")
                        && matches!(
                            origin.role,
                            TaskRangeRole::DefinitionDeclaration
                                | TaskRangeRole::DefinitionUse
                                | TaskRangeRole::DefinitionInitializer
                                | TaskRangeRole::DefinitionExpression
                                | TaskRangeRole::DefinitionDependency
                                | TaskRangeRole::DefinitionDependencyDeclaration
                        )
                    {
                        return Err(invalid());
                    }
                    charged = charged.checked_add(file.byte_len).ok_or_else(invalid)?;
                }
            }
        }
        let mut previous_request = None;
        for omission in &r.omissions {
            if omission.request_index >= r.acquisition_attempts
                || previous_request.is_some_and(|p| p >= omission.request_index)
                || !facts.contains_key(&omission.fact)
            {
                return Err(invalid());
            }
            if omission.reason == SelectionOmissionReason::LegacyReadUnavailable
                && facts[&omission.fact].binding.is_some()
            {
                return Err(invalid());
            }
            previous_request = Some(omission.request_index);
            justified.insert(&omission.fact);
        }
        if charged != r.captured_validation_bytes {
            return Err(invalid());
        }
        // There is at most one additional fixed slot; unread candidates cannot
        // be smuggled into the persisted selection as if their evidence was read.
        let extra: Vec<_> = facts.keys().filter(|f| !justified.contains(**f)).collect();
        if extra.len() > 1
            || extra.first().is_some_and(
                |f| !matches!(f,TaskFactKey::Edge{label,..} if label == &basis.edge_label),
            )
        {
            return Err(invalid());
        }
        self.fingerprint()?;
        Ok(())
    }
}
fn valid_path(path: &str) -> bool {
    text(path, 1024)
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains(':')
        && path
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}
pub(crate) fn domain_hash(domain: &[u8], bytes: &[u8]) -> String {
    let mut input = Vec::with_capacity(domain.len() + bytes.len());
    input.extend_from_slice(domain);
    input.extend_from_slice(bytes);
    content_hash(&input)
}

/// Refuse over-budget serialization before extending the destination buffer.
pub(crate) fn bounded_json(value: &impl Serialize, limit: usize) -> Result<Vec<u8>, AgentError> {
    struct Bounded {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for Bounded {
        fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
            if input.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::other("metadata byte bound"));
            }
            self.bytes.extend_from_slice(input);
            Ok(input.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| invalid())?;
    Ok(writer.bytes)
}
