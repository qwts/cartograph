//! Immutable host-owned proposal staging (SPEC-03, ADR-0021).
//!
//! Only the host's original bounded task and its validated result can create a
//! stage. Reviews address that stage by ID and revision; they cannot replace its
//! content. Source fingerprints describe supplied working-tree text, not verified
//! revision evidence. This module has no graph, source-file, or provider API.

use crate::{
    AgentBroker, AgentError, AgentProposal, AgentTask, BrokerLimits, ProposalDecision, RawProposal,
    unique_nonempty, validate_staged_proposal,
};
use core_prov::{ConfidenceTier, EvidenceRef, content_hash};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};
use std::path::Path;

/// Current immutable envelope and task-manifest version.
pub const STAGING_SCHEMA_VERSION: u32 = 1;
/// Maximum complete serialized staged record, including its current review.
pub const MAX_STAGED_RECORD_BYTES: usize = 128 * 1024;
/// Maximum number of records returned by one history page.
pub const MAX_STAGED_PAGE_ITEMS: usize = 50;

/// Fingerprint of one supplied span; raw `AgentEvidence.text` is never retained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceBasis {
    /// Citation identity in the host task.
    pub id: String,
    /// Original citation, whose binding to the supplied bytes remains unverified.
    pub source: EvidenceRef,
    /// Hash of the exact UTF-8 text supplied to the broker.
    pub text_hash: String,
    /// Supplied UTF-8 byte count, for durable bound validation.
    pub text_bytes: usize,
}

/// Closed candidate identity and membership without archiving its prompt summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateBasis {
    /// Existing candidate node identity.
    pub node_id: String,
    /// Supplied candidate label.
    pub label: String,
    /// Hash of the exact supplied candidate summary.
    pub summary_hash: String,
    /// Supplied summary UTF-8 byte count.
    pub summary_bytes: usize,
    /// Supplied evidence membership.
    pub evidence_ids: Vec<String>,
}

/// Bounded, versioned comparison basis derived only from the original host task.
/// Lists preserve the supplied order; the proposal also retains the broker's
/// original, set-normalized `basis_hash`. Neither representation proves freshness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskBasisManifest {
    /// Manifest interpretation version.
    pub schema_version: u32,
    /// Producing action identity.
    pub action_id: String,
    /// Gap submitted by the host.
    pub gap_id: String,
    /// Source submitted by the host; exact directed-slot proof is a later gate.
    pub source_id: String,
    /// Relation submitted by the host.
    pub edge_label: String,
    /// Existing unresolved-slot confidence.
    pub existing_confidence: ConfidenceTier,
    /// Source/Gap-side evidence membership.
    pub source_evidence_ids: Vec<String>,
    /// Fingerprints and references for every supplied span, including uncited ones.
    pub evidence: Vec<EvidenceBasis>,
    /// Complete bounded candidate set, including unselected candidates.
    pub candidates: Vec<CandidateBasis>,
}

/// What is known about the relationship between supplied bytes and citations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceBinding {
    /// Working-tree reads have no source-byte attestation from the original parse.
    WorkingTreeUnverified,
}

/// Eligibility for the later curated projection, independent of human review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextStatus {
    /// Staging and review cannot establish freshness or activate curated facts.
    AwaitingReconciliation,
}

/// Durable proposal plus its separate, revisioned human review. Proposal fields
/// remain flat on the wire for existing review surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedProposal {
    /// Immutable staging envelope version.
    pub schema_version: u32,
    /// Complete-content stage identity, distinct from the legacy edge fact hash.
    pub proposal_id: String,
    /// Exact broker-produced body and producing provenance.
    #[serde(flatten)]
    pub proposal: AgentProposal,
    /// Producing durable job identity; no foreign key ties lifetime to job cleanup.
    pub job_id: i64,
    /// Recovered snapshot copied before task assembly; not a freshness attestation.
    pub graph_snapshot_id: String,
    /// Original bounded task with source and summary text replaced by hashes.
    pub basis: TaskBasisManifest,
    /// Explicitly unverified source binding, unchanged by acceptance.
    pub evidence_binding: EvidenceBinding,
    /// Explicitly pending reconciliation, unchanged by acceptance.
    pub context_status: ContextStatus,
    /// Compare-and-set review revision; zero means never reviewed.
    pub review_revision: u64,
    /// Current human decision, if any.
    pub review_decision: Option<ProposalDecision>,
    /// Optional human review note.
    pub review_note: Option<String>,
    /// Initial SQLite UTC creation timestamp; excluded from content identity.
    pub created_at: String,
    /// Last review UTC timestamp; excluded from content identity.
    pub reviewed_at: Option<String>,
}

/// Bounded newest-first history; pending and reviewed records share one queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedProposalPage {
    /// Records in immutable insertion-sequence order, newest first.
    pub items: Vec<StagedProposal>,
    /// Opaque keyset continuation; `None` means this traversal is complete.
    pub next_cursor: Option<String>,
}

/// Staging, validation, review-concurrency and persistence failures.
#[derive(Debug, thiserror::Error)]
pub enum StagingError {
    /// The original bounded task or result failed the broker contract.
    #[error(transparent)]
    InvalidTask(#[from] AgentError),
    /// A provider annotation repeats supplied task material under SPEC-03's
    /// bounded replay policy. Never carry the detected source in this error.
    #[error("proposal annotation replays supplied task text")]
    AnnotationReplaysTaskText,
    /// Candidate summaries must be bounded before task hashing or replay checks.
    #[error("staging task summaries exceed the byte bound")]
    TaskSummariesTooLarge,
    /// Missing or invalid host metadata.
    #[error("invalid staging metadata: {0}")]
    InvalidMetadata(&'static str),
    /// Stored immutable content failed version, integrity or structure checks.
    #[error("invalid immutable staged record")]
    InvalidRecord,
    /// An ID does not name a host-staged proposal.
    #[error("unknown staged proposal: {0}")]
    UnknownProposal(String),
    /// Another review has already changed this record.
    #[error("stale review revision: expected {expected}, current {actual}")]
    StaleReviewRevision {
        /// Revision supplied by the reviewer.
        expected: u64,
        /// Current persisted revision.
        actual: u64,
    },
    /// Complete serialized payload exceeds the per-record hard bound.
    #[error("staged record exceeds {MAX_STAGED_RECORD_BYTES} bytes ({bytes} bytes)")]
    RecordTooLarge {
        /// Measured serialized bytes.
        bytes: usize,
    },
    /// Page budgets must be bounded and nonzero.
    #[error("staged history limit must be between 1 and {MAX_STAGED_PAGE_ITEMS}")]
    InvalidLimit,
    /// A continuation is malformed or uses an unsupported version.
    #[error("invalid staged history cursor")]
    InvalidCursor,
    /// SQLite operation failed; no successful stage/review may be reported.
    #[error("proposal store: {0}")]
    Storage(#[from] rusqlite::Error),
    /// Serialization failed before a durable operation completed.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StageContent {
    schema_version: u32,
    proposal: AgentProposal,
    job_id: i64,
    graph_snapshot_id: String,
    basis: TaskBasisManifest,
    evidence_binding: EvidenceBinding,
    context_status: ContextStatus,
}

impl StageContent {
    fn build(
        task: &AgentTask,
        proposal: &AgentProposal,
        job_id: i64,
        graph_snapshot_id: &str,
    ) -> Result<Self, StagingError> {
        validate_metadata(job_id, graph_snapshot_id)?;
        check_size(proposal.annotation.len())?;
        let summary_bytes = task.candidates.iter().try_fold(0usize, |total, candidate| {
            total.checked_add(candidate.summary.len())
        });
        if summary_bytes.is_none_or(|bytes| bytes > MAX_STAGED_RECORD_BYTES) {
            return Err(StagingError::TaskSummariesTooLarge);
        }
        let broker = AgentBroker::bounded_default();
        broker.validate_task(task)?;
        reject_annotation_replay(task, &proposal.annotation)?;
        let citations = match_citations(task, proposal).ok_or_else(|| {
            AgentError::Integrity("proposal citations do not match the host task".into())
        })?;
        let validated = broker.validate_response(
            task,
            RawProposal {
                target_id: proposal.target_id.clone(),
                annotation: proposal.annotation.clone(),
                citations,
            },
        )?;
        if &validated != proposal {
            return Err(AgentError::Integrity(
                "proposal does not match the original host task and broker contract".into(),
            )
            .into());
        }
        let content = Self {
            schema_version: STAGING_SCHEMA_VERSION,
            proposal: proposal.clone(),
            job_id,
            graph_snapshot_id: graph_snapshot_id.into(),
            basis: TaskBasisManifest {
                schema_version: STAGING_SCHEMA_VERSION,
                action_id: task.action_id.clone(),
                gap_id: task.gap_id.clone(),
                source_id: task.source_id.clone(),
                edge_label: task.edge_label.clone(),
                existing_confidence: task.existing_confidence,
                source_evidence_ids: task.source_evidence_ids.clone(),
                evidence: task
                    .evidence
                    .iter()
                    .map(|item| EvidenceBasis {
                        id: item.id.clone(),
                        source: item.source.clone(),
                        text_hash: content_hash(item.text.as_bytes()),
                        text_bytes: item.text.len(),
                    })
                    .collect(),
                candidates: task
                    .candidates
                    .iter()
                    .map(|item| CandidateBasis {
                        node_id: item.node_id.clone(),
                        label: item.label.clone(),
                        summary_hash: content_hash(item.summary.as_bytes()),
                        summary_bytes: item.summary.len(),
                        evidence_ids: item.evidence_ids.clone(),
                    })
                    .collect(),
            },
            evidence_binding: EvidenceBinding::WorkingTreeUnverified,
            context_status: ContextStatus::AwaitingReconciliation,
        };
        content.validate()?;
        Ok(content)
    }

    fn validate(&self) -> Result<(), StagingError> {
        let limits = BrokerLimits::default();
        let basis = &self.basis;
        if self.schema_version != STAGING_SCHEMA_VERSION
            || basis.schema_version != STAGING_SCHEMA_VERSION
            || validate_metadata(self.job_id, &self.graph_snapshot_id).is_err()
            || validate_staged_proposal(&self.proposal).is_err()
            || basis.gap_id != self.proposal.gap_id
            || basis.source_id != self.proposal.source_id
            || basis.edge_label != self.proposal.edge_label
            || basis.action_id.trim().is_empty()
            || basis.gap_id.trim().is_empty()
            || basis.source_id.trim().is_empty()
            || !matches!(
                basis.existing_confidence,
                ConfidenceTier::Gap | ConfidenceTier::InferredWeak
            )
            || !limits.allowed_edge_labels.contains(&basis.edge_label)
            || basis.evidence.is_empty()
            || basis.evidence.len() > limits.max_evidence_spans
            || basis.candidates.is_empty()
            || basis.candidates.len() > limits.max_candidates
            || !is_hash(&self.proposal.basis_hash)
        {
            return Err(StagingError::InvalidRecord);
        }
        let evidence_ids = unique_nonempty(basis.evidence.iter().map(|item| item.id.as_str()))
            .map_err(|_| StagingError::InvalidRecord)?;
        unique_nonempty(basis.candidates.iter().map(|item| item.node_id.as_str()))
            .map_err(|_| StagingError::InvalidRecord)?;
        validate_membership(&basis.source_evidence_ids, &evidence_ids)?;
        let mut total_bytes = 0usize;
        for item in &basis.evidence {
            if item.source.repo.trim().is_empty()
                || item.source.path.trim().is_empty()
                || item.source.commit_sha.trim().is_empty()
                || item.source.byte_start >= item.source.byte_end
                || !is_hash(&item.text_hash)
                || item.text_bytes > limits.max_span_bytes
            {
                return Err(StagingError::InvalidRecord);
            }
            total_bytes += item.text_bytes;
        }
        if total_bytes > limits.max_total_evidence_bytes {
            return Err(StagingError::InvalidRecord);
        }
        for item in &basis.candidates {
            if item.label.trim().is_empty() || !is_hash(&item.summary_hash) {
                return Err(StagingError::InvalidRecord);
            }
            validate_membership(&item.evidence_ids, &evidence_ids)?;
        }
        // Validate stored citation membership without reintroducing raw source.
        if !basis
            .candidates
            .iter()
            .any(|item| item.node_id == self.proposal.target_id)
            || citation_assignment(
                &basis
                    .evidence
                    .iter()
                    .map(|item| (&item.id, &item.source))
                    .collect::<Vec<_>>(),
                &basis.source_evidence_ids,
                &basis
                    .candidates
                    .iter()
                    .find(|item| item.node_id == self.proposal.target_id)
                    .ok_or(StagingError::InvalidRecord)?
                    .evidence_ids,
                &self.proposal.provenance.evidence,
            )
            .is_none()
        {
            return Err(StagingError::InvalidRecord);
        }
        Ok(())
    }

    fn identity(&self) -> Result<String, StagingError> {
        Ok(format!(
            "proposal-stage-v1:{}",
            content_hash(&serde_json::to_vec(self)?)
        ))
    }
}

/// Compare transient normalized characters without retaining source or changing
/// the accepted proposal. Short complete items deliberately fail closed too.
/// This is an exact replay bound, not semantic/encoded-text declassification.
fn reject_annotation_replay(task: &AgentTask, annotation: &str) -> Result<(), StagingError> {
    const EXCERPT_SCALARS: usize = 48;
    fn normalized(text: &str) -> Vec<char> {
        text.chars()
            .filter(|character| !character.is_whitespace())
            .collect()
    }
    let annotation = normalized(annotation);
    let excerpts: HashSet<&[char]> = annotation.windows(EXCERPT_SCALARS).collect();
    for supplied in task
        .evidence
        .iter()
        .map(|item| item.text.as_str())
        .chain(task.candidates.iter().map(|item| item.summary.as_str()))
    {
        let supplied = normalized(supplied);
        let repeats = if supplied.is_empty() {
            false
        } else if supplied.len() < EXCERPT_SCALARS {
            annotation
                .windows(supplied.len())
                .any(|part| part == supplied)
        } else {
            supplied
                .windows(EXCERPT_SCALARS)
                .any(|part| excerpts.contains(part))
        };
        if repeats {
            return Err(StagingError::AnnotationReplaysTaskText);
        }
    }
    Ok(())
}

fn is_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_metadata(job_id: i64, graph_snapshot_id: &str) -> Result<(), StagingError> {
    if job_id <= 0 {
        return Err(StagingError::InvalidMetadata("job_id must be positive"));
    }
    if graph_snapshot_id.trim().is_empty() || graph_snapshot_id.len() > 1024 {
        return Err(StagingError::InvalidMetadata(
            "snapshot identity must contain 1..=1024 bytes",
        ));
    }
    Ok(())
}

fn validate_membership(ids: &[String], available: &BTreeSet<String>) -> Result<(), StagingError> {
    if ids.is_empty()
        || unique_nonempty(ids.iter().map(String::as_str)).is_err()
        || ids.iter().any(|id| !available.contains(id))
    {
        return Err(StagingError::InvalidRecord);
    }
    Ok(())
}

fn match_citations(task: &AgentTask, proposal: &AgentProposal) -> Option<Vec<String>> {
    let target = task
        .candidates
        .iter()
        .find(|item| item.node_id == proposal.target_id)?;
    citation_assignment(
        &task
            .evidence
            .iter()
            .map(|item| (&item.id, &item.source))
            .collect::<Vec<_>>(),
        &task.source_evidence_ids,
        &target.evidence_ids,
        &proposal.provenance.evidence,
    )
}

/// Broker output retains references rather than citation IDs. Find a valid
/// injective assignment back to supplied IDs, including both required sides.
/// Distinct IDs may legitimately share a source reference; one citation must not
/// accidentally satisfy two disjoint memberships merely because its span matches.
fn citation_assignment(
    evidence: &[(&String, &EvidenceRef)],
    source_ids: &[String],
    target_ids: &[String],
    citations: &[EvidenceRef],
) -> Option<Vec<String>> {
    if evidence.len() > 12 || citations.is_empty() || citations.len() > evidence.len() {
        return None;
    }
    let membership = |ids: &[String]| {
        evidence
            .iter()
            .enumerate()
            .fold(0u16, |mask, (index, (id, _))| {
                mask | if ids.contains(id) { 1 << index } else { 0 }
            })
    };
    let source_mask = membership(source_ids);
    let target_mask = membership(target_ids);
    let masks = citations
        .iter()
        .map(|reference| {
            evidence
                .iter()
                .enumerate()
                .fold(0u16, |mask, (index, (_, supplied))| {
                    mask | if *supplied == reference {
                        1 << index
                    } else {
                        0
                    }
                })
        })
        .collect::<Vec<_>>();
    fn assign(
        masks: &[u16],
        used: u16,
        source: u16,
        target: u16,
        seen: &mut BTreeSet<u16>,
        selected: &mut Vec<usize>,
    ) -> bool {
        if selected.len() == masks.len() {
            return used & source != 0 && used & target != 0;
        }
        if !seen.insert(used) {
            return false;
        }
        let mut remaining = masks[selected.len()] & !used;
        while remaining != 0 {
            let index = remaining.trailing_zeros() as usize;
            let bit = 1u16 << index;
            remaining &= !bit;
            selected.push(index);
            if assign(masks, used | bit, source, target, seen, selected) {
                return true;
            }
            selected.pop();
        }
        false
    }
    let mut selected = Vec::new();
    assign(
        &masks,
        0,
        source_mask,
        target_mask,
        &mut BTreeSet::new(),
        &mut selected,
    )
    .then(|| {
        selected
            .into_iter()
            .map(|index| evidence[index].0.clone())
            .collect()
    })
}

/// SQLite/WAL proposal store independent of legacy decision rows and job cleanup.
pub struct ProposalStore {
    conn: Connection,
}

impl ProposalStore {
    /// Open persistent staging storage. Existing legacy decisions are not imported.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StagingError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS staged_agent_proposals (
                 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                 proposal_id TEXT NOT NULL UNIQUE,
                 immutable_json TEXT NOT NULL CHECK (length(CAST(immutable_json AS BLOB)) <= 131072),
                 review_revision INTEGER NOT NULL DEFAULT 0 CHECK (review_revision >= 0),
                 review_decision TEXT CHECK (review_decision IN ('accepted', 'rejected')),
                 review_note TEXT,
                 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
                 reviewed_at TEXT,
                 CHECK ((review_revision = 0 AND review_decision IS NULL AND review_note IS NULL AND reviewed_at IS NULL)
                     OR (review_revision > 0 AND review_decision IS NOT NULL AND reviewed_at IS NOT NULL))
             ) STRICT;
             CREATE TRIGGER IF NOT EXISTS staged_agent_proposals_immutable
             BEFORE UPDATE OF sequence, proposal_id, immutable_json, created_at ON staged_agent_proposals
             BEGIN SELECT RAISE(ABORT, 'staged proposal content is immutable'); END;",
        )?;
        Ok(Self { conn })
    }

    /// Persist a validated result from its original bounded host task before
    /// reporting success. Identical content is idempotent, including after review.
    pub fn stage(
        &mut self,
        task: &AgentTask,
        proposal: &AgentProposal,
        job_id: i64,
        graph_snapshot_id: &str,
    ) -> Result<StagedProposal, StagingError> {
        let content = StageContent::build(task, proposal, job_id, graph_snapshot_id)?;
        let immutable_json = serde_json::to_string(&content)?;
        check_size(immutable_json.len())?;
        let proposal_id = content.identity()?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO staged_agent_proposals (proposal_id, immutable_json) VALUES (?1, ?2)
             ON CONFLICT(proposal_id) DO NOTHING",
            params![proposal_id, immutable_json],
        )?;
        let row = get_on(&tx, &proposal_id)?.ok_or(StagingError::InvalidRecord)?;
        // Hash collisions or independently corrupted content cannot replace a stage.
        if immutable_content(&row) != content {
            return Err(StagingError::InvalidRecord);
        }
        check_record_size(&row)?;
        check_review_headroom(&row)?;
        tx.commit()?;
        Ok(row)
    }

    /// Retrieve one stage by its immutable ID, including its current review.
    pub fn get(&self, proposal_id: &str) -> Result<Option<StagedProposal>, StagingError> {
        get_on(&self.conn, proposal_id)
    }

    /// Atomically review only an existing stage and the expected revision.
    /// Acceptance leaves evidence binding, context status and provenance intact.
    pub fn review(
        &mut self,
        proposal_id: &str,
        expected_revision: u64,
        decision: ProposalDecision,
        note: Option<&str>,
    ) -> Result<StagedProposal, StagingError> {
        if let Some(note) = note {
            check_size(note.len())?;
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = get_on(&tx, proposal_id)?
            .ok_or_else(|| StagingError::UnknownProposal(proposal_id.into()))?;
        if current.review_revision != expected_revision {
            return Err(StagingError::StaleReviewRevision {
                expected: expected_revision,
                actual: current.review_revision,
            });
        }
        let next_revision = i64::try_from(expected_revision)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(StagingError::InvalidMetadata("review revision exhausted"))?;
        let changed = tx.execute(
            "UPDATE staged_agent_proposals SET review_revision = ?1, review_decision = ?2,
                 review_note = ?3, reviewed_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
             WHERE proposal_id = ?4 AND review_revision = ?5",
            params![
                next_revision,
                decision.as_str(),
                note,
                proposal_id,
                expected_revision as i64
            ],
        )?;
        if changed != 1 {
            return Err(StagingError::InvalidRecord);
        }
        let reviewed = get_on(&tx, proposal_id)?.ok_or(StagingError::InvalidRecord)?;
        check_record_size(&reviewed)?;
        tx.commit()?;
        Ok(reviewed)
    }

    /// List pending and reviewed history in stable newest-first insertion order.
    /// A cursor fences out later insertions; review values are current at each
    /// page read. The cursor is a continuation, not an authorization credential.
    pub fn list(
        &self,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<StagedProposalPage, StagingError> {
        if limit == 0 || limit > MAX_STAGED_PAGE_ITEMS {
            return Err(StagingError::InvalidLimit);
        }
        let cursor = cursor.map(parse_cursor).transpose()?;
        let ceiling: i64 = match &cursor {
            Some(cursor) => cursor.ceiling,
            None => self.conn.query_row(
                "SELECT COALESCE(MAX(sequence), 0) FROM staged_agent_proposals",
                [],
                |row| row.get(0),
            )?,
        };
        let before = cursor.as_ref().map(|cursor| cursor.before);
        let mut stmt = self.conn.prepare(
            "SELECT sequence, proposal_id, immutable_json, review_revision, review_decision,
                 review_note, created_at, reviewed_at FROM staged_agent_proposals
             WHERE sequence <= ?1 AND (?2 IS NULL OR sequence < ?2)
             ORDER BY sequence DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![ceiling, before, (limit + 1) as i64],
            read_stored_row,
        )?;
        let mut items = Vec::with_capacity(limit);
        let mut last_sequence = None;
        let mut more = false;
        for row in rows {
            let row = row?;
            if items.len() == limit {
                more = true;
                break;
            }
            last_sequence = Some(row.sequence);
            items.push(decode_row(row)?);
        }
        let next_cursor = if more {
            Some(serde_json::to_string(&HistoryCursor {
                version: STAGING_SCHEMA_VERSION,
                ceiling,
                before: last_sequence.ok_or(StagingError::InvalidRecord)?,
            })?)
        } else {
            None
        };
        Ok(StagedProposalPage { items, next_cursor })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryCursor {
    version: u32,
    ceiling: i64,
    before: i64,
}

fn parse_cursor(value: &str) -> Result<HistoryCursor, StagingError> {
    if value.len() > 256 {
        return Err(StagingError::InvalidCursor);
    }
    let cursor: HistoryCursor =
        serde_json::from_str(value).map_err(|_| StagingError::InvalidCursor)?;
    if cursor.version != STAGING_SCHEMA_VERSION
        || cursor.ceiling <= 0
        || cursor.before <= 0
        || cursor.before > cursor.ceiling
    {
        return Err(StagingError::InvalidCursor);
    }
    Ok(cursor)
}

struct StoredRow {
    sequence: i64,
    proposal_id: String,
    immutable_json: String,
    review_revision: i64,
    review_decision: Option<String>,
    review_note: Option<String>,
    created_at: String,
    reviewed_at: Option<String>,
}

fn read_stored_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredRow> {
    Ok(StoredRow {
        sequence: row.get(0)?,
        proposal_id: row.get(1)?,
        immutable_json: row.get(2)?,
        review_revision: row.get(3)?,
        review_decision: row.get(4)?,
        review_note: row.get(5)?,
        created_at: row.get(6)?,
        reviewed_at: row.get(7)?,
    })
}

fn get_on(conn: &Connection, proposal_id: &str) -> Result<Option<StagedProposal>, StagingError> {
    conn.query_row(
        "SELECT sequence, proposal_id, immutable_json, review_revision, review_decision,
             review_note, created_at, reviewed_at FROM staged_agent_proposals WHERE proposal_id = ?1",
        params![proposal_id], read_stored_row,
    ).optional()?.map(decode_row).transpose()
}

fn decode_row(row: StoredRow) -> Result<StagedProposal, StagingError> {
    check_size(row.immutable_json.len())?;
    let content: StageContent =
        serde_json::from_str(&row.immutable_json).map_err(|_| StagingError::InvalidRecord)?;
    content.validate()?;
    if content.identity()? != row.proposal_id || row.review_revision < 0 {
        return Err(StagingError::InvalidRecord);
    }
    let review_decision = row
        .review_decision
        .as_deref()
        .map(ProposalDecision::parse)
        .transpose()
        .map_err(|_| StagingError::InvalidRecord)?;
    if (row.review_revision == 0
        && (review_decision.is_some() || row.review_note.is_some() || row.reviewed_at.is_some()))
        || (row.review_revision > 0 && (review_decision.is_none() || row.reviewed_at.is_none()))
    {
        return Err(StagingError::InvalidRecord);
    }
    let staged = StagedProposal {
        schema_version: content.schema_version,
        proposal_id: row.proposal_id,
        proposal: content.proposal,
        job_id: content.job_id,
        graph_snapshot_id: content.graph_snapshot_id,
        basis: content.basis,
        evidence_binding: content.evidence_binding,
        context_status: content.context_status,
        review_revision: row.review_revision as u64,
        review_decision,
        review_note: row.review_note,
        created_at: row.created_at,
        reviewed_at: row.reviewed_at,
    };
    check_record_size(&staged)?;
    Ok(staged)
}

fn immutable_content(staged: &StagedProposal) -> StageContent {
    StageContent {
        schema_version: staged.schema_version,
        proposal: staged.proposal.clone(),
        job_id: staged.job_id,
        graph_snapshot_id: staged.graph_snapshot_id.clone(),
        basis: staged.basis.clone(),
        evidence_binding: staged.evidence_binding,
        context_status: staged.context_status,
    }
}

fn check_size(bytes: usize) -> Result<(), StagingError> {
    if bytes > MAX_STAGED_RECORD_BYTES {
        Err(StagingError::RecordTooLarge { bytes })
    } else {
        Ok(())
    }
}

fn check_record_size(staged: &StagedProposal) -> Result<(), StagingError> {
    check_size(serde_json::to_vec(staged)?.len())
}

/// A maximal pending payload must still permit an ordinary no-note review.
/// Reserve the largest supported revision and a complete UTC timestamp before
/// committing the stage; review notes retain their own transactional size check.
fn check_review_headroom(staged: &StagedProposal) -> Result<(), StagingError> {
    let mut reviewed = staged.clone();
    reviewed.review_revision = i64::MAX as u64;
    reviewed.review_decision = Some(ProposalDecision::Rejected);
    reviewed.review_note = None;
    reviewed.reviewed_at = Some("9999-12-31T23:59:59Z".into());
    check_record_size(&reviewed)
}

#[cfg(test)]
mod tests;
