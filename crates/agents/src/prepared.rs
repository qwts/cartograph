//! Validated, immutable task input copied by the host before any provider call.
use crate::source_basis::{domain_hash, invalid, text};
use crate::{
    AgentBroker, AgentError, AgentTask, CandidateBasis, EvidenceBasis, TaskBasisManifest,
    TaskSourceBasisV2,
};
use core_prov::content_hash;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedAgentTask {
    task: AgentTask,
    source_basis: TaskSourceBasisV2,
}
impl PreparedAgentTask {
    /// Establish bounded internal consistency. Only host preparation establishes
    /// source authority; this pure constructor performs no I/O or provider work.
    pub fn new(task: AgentTask, source_basis: TaskSourceBasisV2) -> Result<Self, AgentError> {
        // Bound caller-owned metadata before cloning it into a fingerprint
        // manifest; the constructor itself must not amplify oversized strings.
        if task.evidence.len() > 12
            || task.candidates.len() > 8
            || !identity_metadata(
                &task.action_id,
                &task.gap_id,
                &task.source_id,
                &task.edge_label,
            )
            || !membership_metadata(&task.source_evidence_ids)
            || task
                .evidence
                .iter()
                .any(|e| e.text.len() > 8192 || !evidence_metadata(&e.id, &e.source))
            || task
                .candidates
                .iter()
                .any(|c| !candidate_metadata(&c.node_id, &c.label, &c.evidence_ids))
        {
            return Err(invalid());
        }
        validate_summary_bound(&task)?;
        AgentBroker::bounded_default().validate_task(&task)?;
        let manifest = manifest(&task, 2);
        validate_manifest_metadata(&manifest)?;
        source_basis.validate(&manifest)?;
        Ok(Self { task, source_basis })
    }
    pub fn task(&self) -> &AgentTask {
        &self.task
    }
    pub fn source_basis(&self) -> &TaskSourceBasisV2 {
        &self.source_basis
    }
    pub fn basis_hash(&self) -> Result<String, AgentError> {
        prepared_basis_hash(&manifest(&self.task, 2), &self.source_basis)
    }
}

pub(crate) fn validate_summary_bound(task: &AgentTask) -> Result<(), AgentError> {
    let bytes = task
        .candidates
        .iter()
        .try_fold(0usize, |sum, c| sum.checked_add(c.summary.len()));
    if bytes.is_none_or(|n| n > crate::staging::MAX_STAGED_RECORD_BYTES) {
        return Err(invalid());
    }
    Ok(())
}
pub(crate) fn manifest(task: &AgentTask, version: u32) -> TaskBasisManifest {
    TaskBasisManifest {
        schema_version: version,
        action_id: task.action_id.clone(),
        gap_id: task.gap_id.clone(),
        source_id: task.source_id.clone(),
        edge_label: task.edge_label.clone(),
        existing_confidence: task.existing_confidence,
        source_evidence_ids: task.source_evidence_ids.clone(),
        evidence: task
            .evidence
            .iter()
            .map(|i| EvidenceBasis {
                id: i.id.clone(),
                source: i.source.clone(),
                text_hash: content_hash(i.text.as_bytes()),
                text_bytes: i.text.len(),
            })
            .collect(),
        candidates: task
            .candidates
            .iter()
            .map(|c| CandidateBasis {
                node_id: c.node_id.clone(),
                label: c.label.clone(),
                summary_hash: content_hash(c.summary.as_bytes()),
                summary_bytes: c.summary.len(),
                evidence_ids: c.evidence_ids.clone(),
            })
            .collect(),
    }
}
fn identity_metadata(action: &str, gap: &str, source: &str, edge: &str) -> bool {
    text(action, 8192) && text(gap, 8192) && text(source, 8192) && text(edge, 256)
}
fn membership_metadata(ids: &[String]) -> bool {
    ids.len() <= 12
        && ids.iter().all(|s| text(s, 8192))
        && crate::unique_nonempty(ids.iter().map(String::as_str)).is_ok()
}
fn evidence_metadata(id: &str, source: &core_prov::EvidenceRef) -> bool {
    text(id, 8192)
        && text(&source.repo, 256)
        && text(&source.path, 1024)
        && text(&source.commit_sha, 1024)
        && source.byte_start < source.byte_end
}
fn candidate_metadata(node: &str, label: &str, ids: &[String]) -> bool {
    text(node, 8192) && text(label, 256) && membership_metadata(ids)
}
pub(crate) fn validate_manifest_metadata(b: &TaskBasisManifest) -> Result<(), AgentError> {
    if b.schema_version != 2
        || !identity_metadata(&b.action_id, &b.gap_id, &b.source_id, &b.edge_label)
        || b.evidence.len() > 12
        || b.candidates.len() > 8
        || !membership_metadata(&b.source_evidence_ids)
        || b.evidence
            .iter()
            .any(|e| !evidence_metadata(&e.id, &e.source))
        || b.candidates
            .iter()
            .any(|c| !candidate_metadata(&c.node_id, &c.label, &c.evidence_ids))
        || b.candidates
            .iter()
            .try_fold(0usize, |sum, c| sum.checked_add(c.summary_bytes))
            .is_none_or(|n| n > crate::staging::MAX_STAGED_RECORD_BYTES)
    {
        return Err(invalid());
    }
    Ok(())
}
pub(crate) fn prepared_basis_hash(
    b: &TaskBasisManifest,
    source: &TaskSourceBasisV2,
) -> Result<String, AgentError> {
    validate_manifest_metadata(b)?;
    source.validate(b)?;
    let mut b = b.clone();
    // Action identity is consent identity, never part of semantic task identity.
    b.action_id.clear();
    b.source_evidence_ids.sort();
    b.evidence.sort_by(|a, b| a.id.cmp(&b.id));
    for c in &mut b.candidates {
        c.evidence_ids.sort();
    }
    b.candidates.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    let mut source = source.clone();
    source
        .evidence
        .sort_by(|a, b| a.evidence_id.cmp(&b.evidence_id));
    Ok(domain_hash(
        b"cartograph:prepared-agent-task:v2\0",
        &serde_json::to_vec(&(b, source))?,
    ))
}

#[cfg(test)]
pub(crate) mod tests;
