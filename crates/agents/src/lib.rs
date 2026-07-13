//! Bounded, propose-only T3 broker and durable curation decisions.
//!
//! The broker accepts only explicit Gap-resolution tasks, sends bounded cited
//! spans through the `llm` egress firewall, and returns data-only
//! [`AgentProposal`] values. This crate deliberately has no graph-store
//! dependency or mutation API: T3 cannot write confirmed facts.

use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier, content_hash, may_overwrite};
use llm::{
    AnalysisTier, CompletionAction, CompletionPayload, ConsentGrant, EgressFirewall, EgressPreview,
    LlmProvider, PayloadSpan,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Stable extractor identifier carried by every T3 proposal.
pub const AGENT_EXTRACTOR_ID: &str = "t3.agent-broker";

/// One evidence span made available to the bounded agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEvidence {
    /// Stable citation id used in model output.
    pub id: String,
    /// Provenance reference for the cited source.
    pub source: EvidenceRef,
    /// Exact source text; the LLM firewall redacts secrets before use.
    pub text: String,
}

/// One candidate target the model is permitted to choose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCandidate {
    /// Existing graph node id.
    pub node_id: String,
    /// Existing graph node label.
    pub label: String,
    /// Deterministic summary used in the prompt.
    pub summary: String,
    /// Evidence ids that support this target.
    pub evidence_ids: Vec<String>,
}

/// Explicit unresolved slot submitted to T3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTask {
    /// Stable per-run action id used by the egress consent grant.
    pub action_id: String,
    /// Explicit Gap node being resolved.
    pub gap_id: String,
    /// Existing source node for the proposed edge.
    pub source_id: String,
    /// Edge label the lower tiers left unresolved.
    pub edge_label: String,
    /// Current confidence of the unresolved slot.
    pub existing_confidence: ConfidenceTier,
    /// Evidence ids supporting the source/Gap side.
    pub source_evidence_ids: Vec<String>,
    /// Bounded evidence available to the model.
    pub evidence: Vec<AgentEvidence>,
    /// Closed candidate set; arbitrary model-created targets are rejected.
    pub candidates: Vec<AgentCandidate>,
}

impl AgentTask {
    /// Stable hash of the unresolved slot and its exact evidence/candidates.
    /// Decision-log entries reapply only while this basis remains unchanged.
    pub fn basis_hash(&self) -> Result<String, AgentError> {
        // Consent is per `action_id`, but curation survives a new run. Sort
        // set-shaped inputs so extractor ordering also cannot invalidate it.
        let mut source_evidence_ids = self.source_evidence_ids.clone();
        source_evidence_ids.sort();
        let mut evidence = self.evidence.clone();
        evidence.sort_by(|a, b| a.id.cmp(&b.id));
        let mut candidates = self.candidates.clone();
        for candidate in &mut candidates {
            candidate.evidence_ids.sort();
        }
        candidates.sort_by(|a, b| a.node_id.cmp(&b.node_id));
        Ok(content_hash(&serde_json::to_vec(&(
            &self.gap_id,
            &self.source_id,
            &self.edge_label,
            self.existing_confidence,
            source_evidence_ids,
            evidence,
            candidates,
        ))?))
    }
}

/// Hard bounds applied before any provider call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerLimits {
    /// Maximum source spans.
    pub max_evidence_spans: usize,
    /// Maximum UTF-8 bytes in any one evidence span.
    pub max_span_bytes: usize,
    /// Maximum combined UTF-8 evidence bytes.
    pub max_total_evidence_bytes: usize,
    /// Maximum candidate targets.
    pub max_candidates: usize,
    /// Edge labels this broker may propose.
    pub allowed_edge_labels: BTreeSet<String>,
}

impl Default for BrokerLimits {
    fn default() -> Self {
        Self {
            max_evidence_spans: 12,
            max_span_bytes: 8 * 1024,
            max_total_evidence_bytes: 48 * 1024,
            max_candidates: 20,
            allowed_edge_labels: [
                "CALLS",
                "PUBLISHES",
                "SUBSCRIBES",
                "FETCHES",
                "REFERENCES",
                "BACKS",
                "DECIDES",
                "GOVERNS",
                "MAPS_TO",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        }
    }
}

/// Data-only T3 proposal. Applying it is a later gated/human-curated step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProposal {
    /// Gap this proposal would fill.
    pub gap_id: String,
    /// Existing source node.
    pub source_id: String,
    /// Existing candidate target node.
    pub target_id: String,
    /// Proposed edge label.
    pub edge_label: String,
    /// Model explanation, never treated as a confirmed fact.
    pub annotation: String,
    /// Stable evidence/candidate basis used for re-ingest curation.
    pub basis_hash: String,
    /// Always Agentic/InferredWeak with cited evidence.
    pub provenance: Provenance,
}

/// Bounded T3 broker. It exposes no graph mutation capability.
pub struct AgentBroker {
    limits: BrokerLimits,
}

impl AgentBroker {
    /// Construct a broker with explicit limits.
    pub fn new(limits: BrokerLimits) -> Self {
        Self { limits }
    }

    /// Construct the default bounded broker.
    pub fn bounded_default() -> Self {
        Self::new(BrokerLimits::default())
    }

    /// Return the exact egress preview for an action without invoking a model.
    pub fn preview(
        &self,
        provider: &dyn LlmProvider,
        firewall: &EgressFirewall,
        task: &AgentTask,
    ) -> Result<EgressPreview, AgentError> {
        let action = self.prepare(task)?;
        Ok(firewall.preview(provider, &action)?)
    }

    /// Run one bounded task and return a staged proposal. No graph is passed
    /// in or mutated; a cloud provider additionally requires matching consent.
    pub fn propose(
        &self,
        provider: &dyn LlmProvider,
        firewall: &EgressFirewall,
        task: &AgentTask,
        consent: Option<&ConsentGrant>,
    ) -> Result<AgentProposal, AgentError> {
        let action = self.prepare(task)?;
        let completion = firewall.complete(provider, &action, consent)?;
        let raw: RawProposal = serde_json::from_str(&completion.text)
            .map_err(|error| AgentError::InvalidResponse(error.to_string()))?;
        self.validate_response(task, raw)
    }

    fn prepare(&self, task: &AgentTask) -> Result<CompletionAction, AgentError> {
        self.validate_task(task)?;
        let candidates = task
            .candidates
            .iter()
            .map(|candidate| {
                serde_json::json!({
                    "node_id": candidate.node_id,
                    "label": candidate.label,
                    "summary": candidate.summary,
                    "evidence_ids": candidate.evidence_ids,
                })
            })
            .collect::<Vec<_>>();
        let prompt = serde_json::to_string_pretty(&serde_json::json!({
            "gap_id": task.gap_id,
            "source_id": task.source_id,
            "edge_label": task.edge_label,
            "source_evidence_ids": task.source_evidence_ids,
            "allowed_candidates": candidates,
            "response_schema": {
                "target_id": "one allowed candidate node_id",
                "annotation": "brief rationale",
                "citations": ["source evidence id", "target evidence id"]
            }
        }))?;
        Ok(CompletionAction {
            action_id: task.action_id.clone(),
            tier: AnalysisTier::Agentic,
            payload: CompletionPayload {
                system: concat!(
                    "You are Cartograph's bounded T3 resolver. Propose one link only. ",
                    "Choose an existing allowed target, cite evidence from both sides, ",
                    "never claim Confirmed or InferredStrong confidence, never invent a node, ",
                    "and return exactly one JSON object with target_id, annotation, citations."
                )
                .into(),
                prompt,
                spans: task
                    .evidence
                    .iter()
                    .map(|evidence| PayloadSpan {
                        id: evidence.id.clone(),
                        repo: evidence.source.repo.clone(),
                        path: evidence.source.path.clone(),
                        byte_start: evidence.source.byte_start,
                        byte_end: evidence.source.byte_end,
                        commit_sha: evidence.source.commit_sha.clone(),
                        text: evidence.text.clone(),
                    })
                    .collect(),
            },
        })
    }

    fn validate_task(&self, task: &AgentTask) -> Result<(), AgentError> {
        if !may_overwrite(Tier::Agentic, task.existing_confidence) {
            return Err(AgentError::Integrity(format!(
                "T3 cannot propose over {:?}",
                task.existing_confidence
            )));
        }
        if !self.limits.allowed_edge_labels.contains(&task.edge_label) {
            return Err(AgentError::InvalidTask(format!(
                "edge label {} is outside the broker allowlist",
                task.edge_label
            )));
        }
        if task.action_id.trim().is_empty()
            || task.gap_id.trim().is_empty()
            || task.source_id.trim().is_empty()
        {
            return Err(AgentError::InvalidTask(
                "action, Gap, and source ids must be non-empty".into(),
            ));
        }
        if task.evidence.is_empty() || task.evidence.len() > self.limits.max_evidence_spans {
            return Err(AgentError::InvalidTask(format!(
                "evidence span count must be 1..={}",
                self.limits.max_evidence_spans
            )));
        }
        if task.candidates.is_empty() || task.candidates.len() > self.limits.max_candidates {
            return Err(AgentError::InvalidTask(format!(
                "candidate count must be 1..={}",
                self.limits.max_candidates
            )));
        }
        let total_bytes: usize = task.evidence.iter().map(|item| item.text.len()).sum();
        if total_bytes > self.limits.max_total_evidence_bytes
            || task
                .evidence
                .iter()
                .any(|item| item.text.len() > self.limits.max_span_bytes)
        {
            return Err(AgentError::InvalidTask(
                "evidence exceeds configured byte bounds".into(),
            ));
        }
        let evidence_ids = unique_nonempty(task.evidence.iter().map(|item| item.id.as_str()))?;
        let candidate_ids =
            unique_nonempty(task.candidates.iter().map(|item| item.node_id.as_str()))?;
        if candidate_ids.len() != task.candidates.len() {
            return Err(AgentError::InvalidTask("duplicate candidate id".into()));
        }
        if task.source_evidence_ids.is_empty()
            || task
                .source_evidence_ids
                .iter()
                .any(|id| !evidence_ids.contains(id))
        {
            return Err(AgentError::InvalidTask(
                "source evidence ids must name available evidence".into(),
            ));
        }
        if task.candidates.iter().any(|candidate| {
            candidate.evidence_ids.is_empty()
                || candidate
                    .evidence_ids
                    .iter()
                    .any(|id| !evidence_ids.contains(id))
        }) {
            return Err(AgentError::InvalidTask(
                "each candidate must cite available target evidence".into(),
            ));
        }
        Ok(())
    }

    fn validate_response(
        &self,
        task: &AgentTask,
        raw: RawProposal,
    ) -> Result<AgentProposal, AgentError> {
        let candidates: BTreeMap<&str, &AgentCandidate> = task
            .candidates
            .iter()
            .map(|candidate| (candidate.node_id.as_str(), candidate))
            .collect();
        let candidate = candidates.get(raw.target_id.as_str()).ok_or_else(|| {
            AgentError::InvalidResponse(
                "model selected a target outside the closed candidate set".into(),
            )
        })?;
        let evidence: BTreeMap<&str, &AgentEvidence> = task
            .evidence
            .iter()
            .map(|item| (item.id.as_str(), item))
            .collect();
        let citations = unique_nonempty(raw.citations.iter().map(String::as_str))?;
        if citations.len() != raw.citations.len()
            || citations
                .iter()
                .any(|id| !evidence.contains_key(id.as_str()))
        {
            return Err(AgentError::InvalidResponse(
                "citations must be unique ids from the supplied evidence".into(),
            ));
        }
        if !task
            .source_evidence_ids
            .iter()
            .any(|id| citations.contains(id))
            || !candidate
                .evidence_ids
                .iter()
                .any(|id| citations.contains(id))
        {
            return Err(AgentError::InvalidResponse(
                "proposal must cite both the source/Gap and selected target".into(),
            ));
        }
        let basis_hash = task.basis_hash()?;
        let canonical_fact = serde_json::to_vec(&(
            &task.gap_id,
            &task.source_id,
            &raw.target_id,
            &task.edge_label,
            &basis_hash,
        ))?;
        let cited_refs = raw
            .citations
            .iter()
            .filter_map(|id| evidence.get(id.as_str()))
            .map(|item| item.source.clone())
            .collect();
        let provenance = Provenance::new(
            Tier::Agentic,
            ConfidenceTier::InferredWeak,
            cited_refs,
            AGENT_EXTRACTOR_ID,
            &canonical_fact,
        )
        .map_err(|error| AgentError::Integrity(error.to_string()))?;
        Ok(AgentProposal {
            gap_id: task.gap_id.clone(),
            source_id: task.source_id.clone(),
            target_id: raw.target_id,
            edge_label: task.edge_label.clone(),
            annotation: raw.annotation,
            basis_hash,
            provenance,
        })
    }
}

fn unique_nonempty<'a>(
    values: impl Iterator<Item = &'a str>,
) -> Result<BTreeSet<String>, AgentError> {
    let values: Vec<&str> = values.collect();
    if values.iter().any(|value| value.trim().is_empty()) {
        return Err(AgentError::InvalidTask("ids must be non-empty".into()));
    }
    let unique: BTreeSet<String> = values.iter().map(|value| (*value).to_string()).collect();
    if unique.len() != values.len() {
        return Err(AgentError::InvalidTask("ids must be unique".into()));
    }
    Ok(unique)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProposal {
    target_id: String,
    annotation: String,
    citations: Vec<String>,
}

/// Human decision applied to one stable proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalDecision {
    /// Admit to the curated/best-effort overlay as InferredWeak.
    Accepted,
    /// Keep the Gap and suppress this proposal.
    Rejected,
}

impl ProposalDecision {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
        }
    }

    fn parse(value: &str) -> Result<Self, AgentError> {
        match value {
            "accepted" => Ok(Self::Accepted),
            "rejected" => Ok(Self::Rejected),
            other => Err(AgentError::Storage(format!(
                "invalid persisted decision {other}"
            ))),
        }
    }
}

/// Durable decision-log row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionRecord {
    /// Stable staged proposal.
    pub proposal: AgentProposal,
    /// Accepted or rejected.
    pub decision: ProposalDecision,
    /// Optional human note.
    pub note: Option<String>,
    /// SQLite UTC update timestamp.
    pub updated_at: String,
}

/// Any inferred compiler assertion that a human may curate in the Workbench.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CuratableAssertion {
    /// Stable graph/export subject id.
    pub subject_id: String,
    /// Human-readable assertion summary.
    pub summary: String,
    /// T2/T3 provenance; its content hash is the durable re-ingest key.
    pub provenance: Provenance,
}

/// Human disposition for one inferred assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssertionDecision {
    /// Keep the inference in exports at its original confidence tier.
    Accepted,
    /// Suppress the inference from exports while the content hash matches.
    Rejected,
    /// Preserve the inference and attach a human note.
    Annotated,
}

impl AssertionDecision {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Annotated => "annotated",
        }
    }

    fn parse(value: &str) -> Result<Self, AgentError> {
        match value {
            "accepted" => Ok(Self::Accepted),
            "rejected" => Ok(Self::Rejected),
            "annotated" => Ok(Self::Annotated),
            other => Err(AgentError::Storage(format!(
                "invalid persisted assertion decision {other}"
            ))),
        }
    }
}

/// Durable Workbench decision keyed by the assertion content hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssertionDecisionRecord {
    /// Exact inferred assertion that was reviewed.
    pub assertion: CuratableAssertion,
    /// Accept, reject, or annotate without upgrading confidence.
    pub decision: AssertionDecision,
    /// Optional human note; required for `Annotated`.
    pub note: Option<String>,
    /// SQLite UTC update timestamp.
    pub updated_at: String,
}

/// SQLite/WAL decision log on the durable state spine.
pub struct DecisionLog {
    conn: Connection,
}

impl DecisionLog {
    /// Open the durable decision log, creating its table if absent.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AgentError> {
        let conn = Connection::open(path).map_err(storage)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(storage)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS agent_decisions (
                 proposal_hash TEXT PRIMARY KEY,
                 basis_hash    TEXT NOT NULL,
                 gap_id        TEXT NOT NULL,
                 decision      TEXT NOT NULL CHECK (decision IN ('accepted', 'rejected')),
                 proposal_json TEXT NOT NULL,
                 note          TEXT,
                 updated_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
             ) STRICT;
             CREATE INDEX IF NOT EXISTS idx_agent_decisions_basis
                 ON agent_decisions(basis_hash);
             CREATE TABLE IF NOT EXISTS assertion_decisions (
                 content_hash  TEXT PRIMARY KEY,
                 assertion_json TEXT NOT NULL,
                 decision      TEXT NOT NULL CHECK (
                     decision IN ('accepted', 'rejected', 'annotated')
                 ),
                 note          TEXT,
                 updated_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
             ) STRICT;",
        )
        .map_err(storage)?;
        Ok(Self { conn })
    }

    /// Insert or replace the human decision for a stable proposal hash.
    pub fn record(
        &mut self,
        proposal: &AgentProposal,
        decision: ProposalDecision,
        note: Option<&str>,
    ) -> Result<DecisionRecord, AgentError> {
        validate_staged_proposal(proposal)?;
        let proposal_hash = &proposal.provenance.content_hash;
        self.conn
            .execute(
                "INSERT INTO agent_decisions (
                     proposal_hash, basis_hash, gap_id, decision, proposal_json, note
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(proposal_hash) DO UPDATE SET
                     decision = excluded.decision,
                     proposal_json = excluded.proposal_json,
                     note = excluded.note,
                     updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')",
                params![
                    proposal_hash,
                    proposal.basis_hash,
                    proposal.gap_id,
                    decision.as_str(),
                    serde_json::to_string(proposal)?,
                    note,
                ],
            )
            .map_err(storage)?;
        self.get(proposal_hash)?.ok_or_else(|| {
            AgentError::Storage("decision disappeared after successful write".into())
        })
    }

    /// Fetch one decision by proposal content hash.
    pub fn get(&self, proposal_hash: &str) -> Result<Option<DecisionRecord>, AgentError> {
        let row = self
            .conn
            .query_row(
                "SELECT proposal_json, decision, note, updated_at
                 FROM agent_decisions WHERE proposal_hash = ?1",
                params![proposal_hash],
                read_decision_row,
            )
            .optional()
            .map_err(storage)?;
        row.map(parse_decision_row).transpose()
    }

    /// Reapply all prior decisions whose exact task basis still exists after
    /// re-ingest. Changed evidence produces a new basis and stays undecided.
    pub fn reapply(&self, basis_hash: &str) -> Result<Vec<DecisionRecord>, AgentError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT proposal_json, decision, note, updated_at
                 FROM agent_decisions WHERE basis_hash = ?1 ORDER BY proposal_hash",
            )
            .map_err(storage)?;
        let rows = stmt
            .query_map(params![basis_hash], read_decision_row)
            .map_err(storage)?;
        rows.map(|row| row.map_err(storage).and_then(parse_decision_row))
            .collect()
    }

    /// List all curation decisions, newest first.
    pub fn list(&self) -> Result<Vec<DecisionRecord>, AgentError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT proposal_json, decision, note, updated_at
                 FROM agent_decisions ORDER BY updated_at DESC, proposal_hash",
            )
            .map_err(storage)?;
        let rows = stmt.query_map([], read_decision_row).map_err(storage)?;
        rows.map(|row| row.map_err(storage).and_then(parse_decision_row))
            .collect()
    }

    /// Insert or replace curation for any cited T2/T3 assertion.
    pub fn record_assertion(
        &mut self,
        assertion: &CuratableAssertion,
        decision: AssertionDecision,
        note: Option<&str>,
    ) -> Result<AssertionDecisionRecord, AgentError> {
        validate_curatable_assertion(assertion)?;
        let normalized_note = note.map(str::trim).filter(|note| !note.is_empty());
        if decision == AssertionDecision::Annotated && normalized_note.is_none() {
            return Err(AgentError::InvalidTask(
                "annotated decisions require a non-empty note".into(),
            ));
        }
        self.conn
            .execute(
                "INSERT INTO assertion_decisions (
                     content_hash, assertion_json, decision, note
                 ) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(content_hash) DO UPDATE SET
                     assertion_json = excluded.assertion_json,
                     decision = excluded.decision,
                     note = excluded.note,
                     updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')",
                params![
                    assertion.provenance.content_hash,
                    serde_json::to_string(assertion)?,
                    decision.as_str(),
                    normalized_note,
                ],
            )
            .map_err(storage)?;
        self.get_assertion(&assertion.provenance.content_hash)?
            .ok_or_else(|| {
                AgentError::Storage("assertion decision disappeared after successful write".into())
            })
    }

    /// Fetch curation by the exact assertion content hash after re-ingest.
    pub fn get_assertion(
        &self,
        content_hash: &str,
    ) -> Result<Option<AssertionDecisionRecord>, AgentError> {
        let row = self
            .conn
            .query_row(
                "SELECT assertion_json, decision, note, updated_at
                 FROM assertion_decisions WHERE content_hash = ?1",
                params![content_hash],
                read_decision_row,
            )
            .optional()
            .map_err(storage)?;
        row.map(parse_assertion_decision_row).transpose()
    }

    /// List all Workbench curation decisions, newest first.
    pub fn list_assertions(&self) -> Result<Vec<AssertionDecisionRecord>, AgentError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT assertion_json, decision, note, updated_at
                 FROM assertion_decisions ORDER BY updated_at DESC, content_hash",
            )
            .map_err(storage)?;
        let rows = stmt.query_map([], read_decision_row).map_err(storage)?;
        rows.map(|row| row.map_err(storage).and_then(parse_assertion_decision_row))
            .collect()
    }
}

fn validate_curatable_assertion(assertion: &CuratableAssertion) -> Result<(), AgentError> {
    if assertion.subject_id.trim().is_empty()
        || assertion.summary.trim().is_empty()
        || assertion.provenance.validate().is_err()
        || assertion.provenance.content_hash.trim().is_empty()
        || assertion.provenance.evidence.is_empty()
        || !matches!(
            assertion.provenance.confidence_tier,
            ConfidenceTier::InferredStrong | ConfidenceTier::InferredWeak
        )
        || !matches!(assertion.provenance.tier, Tier::Semantic | Tier::Agentic)
    {
        return Err(AgentError::Integrity(
            "Workbench curation accepts only cited T2/T3 inferred assertions".into(),
        ));
    }
    Ok(())
}

fn validate_staged_proposal(proposal: &AgentProposal) -> Result<(), AgentError> {
    if proposal.provenance.tier != Tier::Agentic
        || proposal.provenance.confidence_tier != ConfidenceTier::InferredWeak
        || proposal.provenance.extractor_id != AGENT_EXTRACTOR_ID
        || proposal.provenance.evidence.is_empty()
    {
        return Err(AgentError::Integrity(
            "decision log accepts only cited Agentic/InferredWeak broker proposals".into(),
        ));
    }
    let canonical_fact = serde_json::to_vec(&(
        &proposal.gap_id,
        &proposal.source_id,
        &proposal.target_id,
        &proposal.edge_label,
        &proposal.basis_hash,
    ))?;
    if proposal.provenance.content_hash != content_hash(&canonical_fact) {
        return Err(AgentError::Integrity(
            "proposal content hash does not match its staged fact".into(),
        ));
    }
    Ok(())
}

type StoredDecision = (String, String, Option<String>, String);

fn read_decision_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredDecision> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn parse_decision_row(row: StoredDecision) -> Result<DecisionRecord, AgentError> {
    let proposal = serde_json::from_str(&row.0)?;
    validate_staged_proposal(&proposal)?;
    Ok(DecisionRecord {
        proposal,
        decision: ProposalDecision::parse(&row.1)?,
        note: row.2,
        updated_at: row.3,
    })
}

fn parse_assertion_decision_row(
    row: StoredDecision,
) -> Result<AssertionDecisionRecord, AgentError> {
    let assertion = serde_json::from_str(&row.0)?;
    validate_curatable_assertion(&assertion)?;
    Ok(AssertionDecisionRecord {
        assertion,
        decision: AssertionDecision::parse(&row.1)?,
        note: row.2,
        updated_at: row.3,
    })
}

fn storage(error: rusqlite::Error) -> AgentError {
    AgentError::Storage(error.to_string())
}

/// Broker, provider, validation, and persistence failures.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// Task exceeds a hard bound or lacks required structure.
    #[error("invalid bounded task: {0}")]
    InvalidTask(String),
    /// T3 attempted to act outside its integrity ceiling.
    #[error("agent integrity violation: {0}")]
    Integrity(String),
    /// Model output violated the closed proposal schema.
    #[error("invalid agent response: {0}")]
    InvalidResponse(String),
    /// Model provider or egress firewall failed.
    #[error(transparent)]
    Provider(#[from] llm::ProviderError),
    /// JSON serialization failed.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Durable decision-log operation failed.
    #[error("decision log: {0}")]
    Storage(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm::{
        Completion, EgressPolicy, Embedding, Locality, ProviderCaps, ProviderCompletionRequest,
        ProviderError,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FixedProvider {
        calls: AtomicUsize,
        response: &'static str,
    }

    impl LlmProvider for FixedProvider {
        fn id(&self) -> &str {
            "local:test-agent"
        }

        fn locality(&self) -> Locality {
            Locality::Local
        }

        fn capabilities(&self) -> ProviderCaps {
            ProviderCaps {
                embeddings: false,
                chat: true,
                tool_use: false,
            }
        }

        fn embed(&self, _batch: &[String]) -> Result<Vec<Embedding>, ProviderError> {
            Err(ProviderError::Unsupported("embeddings"))
        }

        fn complete(
            &self,
            request: &ProviderCompletionRequest,
        ) -> Result<Completion, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.spans().len(), 2);
            Ok(Completion {
                text: self.response.into(),
            })
        }
    }

    fn evidence(id: &str, path: &str, text: &str) -> AgentEvidence {
        AgentEvidence {
            id: id.into(),
            source: EvidenceRef {
                repo: "acme/shop".into(),
                path: path.into(),
                byte_start: 1,
                byte_end: 20,
                commit_sha: "abc123".into(),
            },
            text: text.into(),
        }
    }

    fn task(confidence: ConfidenceTier) -> AgentTask {
        AgentTask {
            action_id: "agent:gap:orders".into(),
            gap_id: "gap:orders".into(),
            source_id: "sym:api#publish".into(),
            edge_label: "PUBLISHES".into(),
            existing_confidence: confidence,
            source_evidence_ids: vec!["source".into()],
            evidence: vec![
                evidence("source", "src/api.ts", "publish(queueName)"),
                evidence("target", "infra/orders.tf", "resource orders_queue"),
            ],
            candidates: vec![AgentCandidate {
                node_id: "res:orders_queue".into(),
                label: "Resource".into(),
                summary: "orders SQS queue".into(),
                evidence_ids: vec!["target".into()],
            }],
        }
    }

    fn proposal(provider: &FixedProvider, task: &AgentTask) -> AgentProposal {
        AgentBroker::bounded_default()
            .propose(
                provider,
                &EgressFirewall::new(EgressPolicy::local_only()),
                task,
                None,
            )
            .unwrap()
    }

    #[test]
    fn broker_returns_propose_only_inferred_weak_with_citations() {
        // AC-0020: T3 returns a proposal value only, with cited evidence and
        // an Agentic/InferredWeak ceiling; it has no GraphStore write handle.
        let provider = FixedProvider {
            calls: AtomicUsize::new(0),
            response: r#"{"target_id":"res:orders_queue","annotation":"names and config align","citations":["source","target"]}"#,
        };
        let proposal = proposal(&provider, &task(ConfidenceTier::Gap));
        assert_eq!(proposal.provenance.tier, Tier::Agentic);
        assert_eq!(
            proposal.provenance.confidence_tier,
            ConfidenceTier::InferredWeak
        );
        assert_eq!(proposal.provenance.evidence.len(), 2);
        assert_eq!(proposal.target_id, "res:orders_queue");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn broker_rejects_confirmed_slots_before_model_invocation() {
        // AC-0020/R-INT-1: no model output can overwrite T0/T1.
        let provider = FixedProvider {
            calls: AtomicUsize::new(0),
            response: "{}",
        };
        let error = AgentBroker::bounded_default()
            .propose(
                &provider,
                &EgressFirewall::new(EgressPolicy::local_only()),
                &task(ConfidenceTier::Confirmed),
                None,
            )
            .unwrap_err();
        assert!(matches!(error, AgentError::Integrity(_)));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn broker_rejects_uncited_or_invented_targets() {
        let invented = FixedProvider {
            calls: AtomicUsize::new(0),
            response: r#"{"target_id":"res:invented","annotation":"guess","citations":["source"]}"#,
        };
        let error = AgentBroker::bounded_default()
            .propose(
                &invented,
                &EgressFirewall::new(EgressPolicy::local_only()),
                &task(ConfidenceTier::Gap),
                None,
            )
            .unwrap_err();
        assert!(matches!(error, AgentError::InvalidResponse(_)));
    }

    #[test]
    fn accepted_and_rejected_decisions_persist_and_reapply_by_basis() {
        // AC-0025: decisions survive reopen and match only the unchanged
        // evidence/candidate basis after re-ingest.
        let provider = FixedProvider {
            calls: AtomicUsize::new(0),
            response: r#"{"target_id":"res:orders_queue","annotation":"match","citations":["source","target"]}"#,
        };
        let original_task = task(ConfidenceTier::Gap);
        let proposal = proposal(&provider, &original_task);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            let mut log = DecisionLog::open(&path).unwrap();
            log.record(
                &proposal,
                ProposalDecision::Accepted,
                Some("verified configuration"),
            )
            .unwrap();
            log.record(&proposal, ProposalDecision::Rejected, Some("superseded"))
                .unwrap();
        }
        let log = DecisionLog::open(&path).unwrap();
        let mut rerun_task = original_task.clone();
        rerun_task.action_id = "agent:gap:orders:second-run".into();
        assert_eq!(
            original_task.basis_hash().unwrap(),
            rerun_task.basis_hash().unwrap(),
            "per-action consent ids are not part of the re-ingest basis"
        );
        let reapplied = log.reapply(&rerun_task.basis_hash().unwrap()).unwrap();
        assert_eq!(reapplied.len(), 1);
        assert_eq!(reapplied[0].decision, ProposalDecision::Rejected);
        assert_eq!(reapplied[0].note.as_deref(), Some("superseded"));

        let mut changed_task = original_task;
        changed_task.evidence[0].text.push_str(" changed");
        assert!(
            log.reapply(&changed_task.basis_hash().unwrap())
                .unwrap()
                .is_empty()
        );

        let mut tampered = proposal;
        tampered.provenance.confidence_tier = ConfidenceTier::Confirmed;
        let mut log = DecisionLog::open(&path).unwrap();
        assert!(matches!(
            log.record(&tampered, ProposalDecision::Accepted, None),
            Err(AgentError::Integrity(_))
        ));
    }

    #[test]
    fn inferred_assertion_curation_persists_by_content_hash() {
        // AC-0033 (T-0033): accept/reject/annotate survives reopen and
        // re-applies only to the exact content-addressed inferred assertion.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let inferred = CuratableAssertion {
            subject_id: "edge:sym:a CALLS sym:b".into(),
            summary: "CALLS: a → b".into(),
            provenance: Provenance::new(
                Tier::Semantic,
                ConfidenceTier::InferredStrong,
                vec![EvidenceRef {
                    repo: "local/shop".into(),
                    path: "src/app.ts".into(),
                    byte_start: 10,
                    byte_end: 20,
                    commit_sha: "abc123".into(),
                }],
                "t2.semantic",
                b"sym:a CALLS sym:b",
            )
            .unwrap(),
        };
        {
            let mut log = DecisionLog::open(&path).unwrap();
            let record = log
                .record_assertion(
                    &inferred,
                    AssertionDecision::Annotated,
                    Some("verified naming convention"),
                )
                .unwrap();
            assert_eq!(record.decision, AssertionDecision::Annotated);
        }
        let log = DecisionLog::open(&path).unwrap();
        let reapplied = log
            .get_assertion(&inferred.provenance.content_hash)
            .unwrap()
            .unwrap();
        assert_eq!(reapplied.assertion, inferred);
        assert_eq!(
            reapplied.note.as_deref(),
            Some("verified naming convention")
        );
        assert_eq!(log.list_assertions().unwrap().len(), 1);

        let mut changed = reapplied.assertion;
        changed.provenance = Provenance::new(
            Tier::Semantic,
            ConfidenceTier::InferredStrong,
            changed.provenance.evidence.clone(),
            "t2.semantic",
            b"sym:a CALLS sym:c",
        )
        .unwrap();
        assert!(
            log.get_assertion(&changed.provenance.content_hash)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn assertion_curation_rejects_confirmed_and_empty_annotations() {
        let mut log = DecisionLog::open(":memory:").unwrap();
        let confirmed = CuratableAssertion {
            subject_id: "edge:a CALLS b".into(),
            summary: "CALLS: a → b".into(),
            provenance: Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
                vec![EvidenceRef {
                    repo: "local/shop".into(),
                    path: "src/app.ts".into(),
                    byte_start: 1,
                    byte_end: 2,
                    commit_sha: "abc123".into(),
                }],
                "t0.test",
                b"confirmed",
            )
            .unwrap(),
        };
        assert!(matches!(
            log.record_assertion(&confirmed, AssertionDecision::Rejected, None),
            Err(AgentError::Integrity(_))
        ));

        let mut inferred = confirmed;
        inferred.provenance = Provenance::new(
            Tier::Agentic,
            ConfidenceTier::InferredWeak,
            inferred.provenance.evidence.clone(),
            AGENT_EXTRACTOR_ID,
            b"inferred",
        )
        .unwrap();
        assert!(matches!(
            log.record_assertion(&inferred, AssertionDecision::Annotated, Some("  ")),
            Err(AgentError::InvalidTask(_))
        ));
    }

    #[test]
    #[ignore = "requires local Ollama with qwen3:8b already installed"]
    fn real_ollama_returns_bounded_cited_agent_proposal() {
        let provider = llm::OllamaProvider::local_default().unwrap();
        let proposal = AgentBroker::bounded_default()
            .propose(
                &provider,
                &EgressFirewall::new(EgressPolicy::local_only()),
                &task(ConfidenceTier::Gap),
                None,
            )
            .unwrap();
        assert_eq!(proposal.target_id, "res:orders_queue");
        assert_eq!(proposal.provenance.tier, Tier::Agentic);
        assert_eq!(
            proposal.provenance.confidence_tier,
            ConfidenceTier::InferredWeak
        );
        assert_eq!(proposal.provenance.evidence.len(), 2);
    }
}
