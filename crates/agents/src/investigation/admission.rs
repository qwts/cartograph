//! Validate complete immutable metadata and result identities independently of
//! the current graph. Raw source remains necessary only for first admission.
use super::*;
use crate::{TaskFactKey, TaskRangeRole};
use core_prov::{ConfidenceTier, Tier};
use std::collections::{BTreeMap, BTreeSet};

fn hash(value: &str) -> bool {
    crate::source_basis::hash(value)
}
fn digest(fact: &TaskFactKey, value: &str) -> bool {
    value
        .strip_prefix(match fact {
            TaskFactKey::Node { .. } => "node-v1:",
            TaskFactKey::Edge { .. } => "edge-v1:",
        })
        .is_some_and(hash)
}
fn path(value: &str) -> bool {
    text(value, 1024)
        && !value.starts_with('/')
        && !value.contains(['\\', ':'])
        && value
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
}

impl InvestigationInputLedger {
    /// Enforce bounded, complete metadata before publication or use as a saved
    /// historical basis. This does not certify source bytes or current freshness.
    pub fn validate(&self) -> Result<(), InvestigationError> {
        if self.schema_version != 1
            || !super::records::context_hash(&self.graph_snapshot_id)
            || !super::records::context_hash(&self.scope_snapshot_id)
            || self.selected_facts.len() > MAX_SELECTED_FACTS
            || self.receipt_references.len() > MAX_SELECTED_FACTS
            || self.queries.len() > MAX_TOOL_ACTIONS as usize
            || self.citations.len() > MAX_SELECTED_FACTS + 12
            || self.supplied_history_bytes > 16 * 1024
            || (self.supplied_history_bytes == 0) != self.supplied_history_hash.is_none()
            || self
                .supplied_history_hash
                .as_deref()
                .is_some_and(|v| !hash(v))
        {
            return Err(InvestigationError::InvalidInput);
        }
        let mut selections = BTreeMap::new();
        let mut previous = None;
        for fact in &self.selected_facts {
            if !super::protocol::valid_fact(&fact.fact)
                || !digest(&fact.fact, &fact.fact_digest)
                || previous.is_some_and(|p| p >= &fact.fact)
            {
                return Err(InvestigationError::InvalidInput);
            }
            if let Some(binding) = &fact.binding
                && (!text(&binding.repo_key, 256)
                    || !receipt_id(&binding.receipt_id)
                    || binding.emitted_fact_digest != fact.fact_digest)
            {
                return Err(InvestigationError::InvalidInput);
            }
            previous = Some(&fact.fact);
            selections.insert(&fact.fact, fact);
        }
        let mut citation_ids = BTreeSet::new();
        let expected_receipts = self
            .selected_facts
            .iter()
            .filter_map(|f| f.binding.as_ref())
            .map(|b| (&b.repo_key, &b.receipt_id))
            .collect::<BTreeSet<_>>();
        let mut referenced = BTreeSet::new();
        let mut previous_receipt = None;
        for receipt in &self.receipt_references {
            if !text(&receipt.source_id, 256)
                || !text(&receipt.repo_key, 256)
                || !receipt_id(&receipt.receipt_id)
                || previous_receipt.is_some_and(|previous| previous >= receipt)
                || !referenced.insert((&receipt.repo_key, &receipt.receipt_id))
            {
                return Err(InvestigationError::InvalidInput);
            }
            previous_receipt = Some(receipt);
        }
        if referenced != expected_receipts {
            return Err(InvestigationError::InvalidInput);
        }
        let mut graph_citations = BTreeSet::new();
        let mut evidence_items = 0usize;
        let mut source_bytes = 0u64;
        let mut validation_bytes = 0u64;
        for citation in &self.citations {
            let selected = selections
                .get(&citation.fact)
                .ok_or(InvestigationError::InvalidInput)?;
            if !super::records::opaque_id(&citation.citation_id)
                || !citation_ids.insert(&citation.citation_id)
                || selected.fact_digest != citation.fact_digest
            {
                return Err(InvestigationError::InvalidInput);
            }
            if matches!(citation.origin, InvestigationEvidenceOrigin::GraphMetadata) {
                if citation.source.is_some()
                    || citation.role.is_some()
                    || citation.index.is_some()
                    || citation.text_hash.is_some()
                    || !graph_citations.insert(&citation.fact)
                {
                    return Err(InvestigationError::InvalidInput);
                }
                continue;
            }
            evidence_items += 1;
            let source = citation
                .source
                .as_ref()
                .ok_or(InvestigationError::InvalidInput)?;
            if !text(&source.repo, 256)
                || !path(&source.path)
                || !text(&source.commit_sha, 256)
                || source.byte_start >= source.byte_end
                || source.byte_end - source.byte_start > 8192
                || citation.role.is_none()
                || citation.index.is_none_or(|i| i >= 1024)
                || !citation.text_hash.as_deref().is_some_and(hash)
            {
                return Err(InvestigationError::InvalidInput);
            }
            source_bytes = source_bytes
                .checked_add(source.byte_end - source.byte_start)
                .ok_or(InvestigationError::LimitExceeded)?;
            match &citation.origin {
                InvestigationEvidenceOrigin::WorkingTreeUnverified => {
                    if selected.binding.is_some() {
                        return Err(InvestigationError::InvalidInput);
                    }
                }
                InvestigationEvidenceOrigin::CapturedPrimarySource {
                    registered_source_id,
                    receipt_id,
                    receipt_inventory_index,
                    captured,
                    ..
                } => {
                    let binding = selected
                        .binding
                        .as_ref()
                        .ok_or(InvestigationError::InvalidInput)?;
                    if binding.repo_key != source.repo
                        || &binding.receipt_id != receipt_id
                        || !self.receipt_references.iter().any(|r| {
                            r.source_id == *registered_source_id
                                && r.repo_key == binding.repo_key
                                && r.receipt_id == *receipt_id
                        })
                        || !text(registered_source_id, 256)
                        || *receipt_inventory_index >= 1024
                        || &captured.file.source_id != registered_source_id
                        || !captured
                            .file
                            .capture_id
                            .strip_prefix("capture-v1:")
                            .is_some_and(hash)
                        || !hash(&captured.file.digest)
                        || captured.file.path != source.path
                        || captured.file.byte_len > 16 * 1024 * 1024
                        || captured.byte_start != source.byte_start
                        || captured.byte_end != source.byte_end
                        || captured.byte_end > captured.file.byte_len
                        || (receipt_id.starts_with("ts-primary-v1:")
                            && matches!(
                                citation.role,
                                Some(
                                    TaskRangeRole::DefinitionDeclaration
                                        | TaskRangeRole::DefinitionUse
                                        | TaskRangeRole::DefinitionInitializer
                                        | TaskRangeRole::DefinitionExpression
                                        | TaskRangeRole::DefinitionDependency
                                        | TaskRangeRole::DefinitionDependencyDeclaration
                                )
                            ))
                    {
                        return Err(InvestigationError::InvalidInput);
                    }
                    validation_bytes = validation_bytes
                        .checked_add(captured.file.byte_len)
                        .ok_or(InvestigationError::LimitExceeded)?;
                }
                InvestigationEvidenceOrigin::GraphMetadata => unreachable!(),
            }
        }
        if graph_citations.len() != selections.len()
            || evidence_items > 12
            || source_bytes > 48 * 1024
            || validation_bytes > 128 * 1024 * 1024
        {
            return Err(InvestigationError::InvalidInput);
        }
        let mut query_bytes = 0usize;
        let mut returned_facts = 0usize;
        for query in &self.queries {
            query.query.validate()?;
            if !hash(&query.response_hash)
                || query.response_bytes > query.query.max_bytes
                || query.response_bytes == 0
                || query.returned_facts > query.query.max_facts
                || query.total_selected > 50_000
                || query.returned_facts > query.total_selected
                || query
                    .query
                    .cursor
                    .as_ref()
                    .is_some_and(|c| c.snapshot_id != self.scope_snapshot_id)
            {
                return Err(InvestigationError::InvalidInput);
            }
            query_bytes = query_bytes
                .checked_add(query.response_bytes)
                .ok_or(InvestigationError::LimitExceeded)?;
            returned_facts += query.returned_facts;
        }
        if query_bytes > 96 * 1024 || returned_facts < selections.len() {
            return Err(InvestigationError::InvalidInput);
        }
        bounded_bytes(self, MAX_MANIFEST_BYTES)?;
        Ok(())
    }
}

fn receipt_id(value: &str) -> bool {
    value
        .strip_prefix("ts-primary-v1:")
        .or_else(|| value.strip_prefix("ts-primary-v2:"))
        .is_some_and(hash)
}

impl InvestigationResult {
    /// Only this initial admission checks raw input replay; subsequent historical
    /// reads check immutable identity and provenance against the saved ledger.
    pub fn admit<'a>(
        investigation_id: &str,
        ledger: &InvestigationInputLedger,
        action: &InvestigationAction,
        supplied_strings: impl IntoIterator<Item = &'a str>,
        observed_response_model: Option<String>,
        created_at: String,
    ) -> Result<Self, InvestigationError> {
        ledger.validate()?;
        let citations = ledger
            .citations
            .iter()
            .map(|c| c.citation_id.clone())
            .collect();
        let supplied = supplied_strings.into_iter().collect::<Vec<_>>();
        let findings = action.admit_finish(&citations, supplied.iter().copied())?;
        let InvestigationAction::Finish {
            findings: proposed,
            limitations: overall,
            ..
        } = action
        else {
            return Err(InvestigationError::InvalidAction);
        };
        let all_prose = proposed
            .iter()
            .flat_map(|f| {
                std::iter::once(f.title.as_str())
                    .chain(std::iter::once(f.statement.as_str()))
                    .chain(f.limitations.iter().map(String::as_str))
            })
            .chain(overall.iter().map(String::as_str))
            .chain(observed_response_model.as_deref());
        reject_prose_replay(all_prose, supplied.iter().copied())?;
        let InvestigationAction::Finish {
            knowledge_completeness,
            limitations,
            ..
        } = action
        else {
            return Err(InvestigationError::InvalidAction);
        };
        let mut result = Self {
            schema_version: 1,
            investigation_id: investigation_id.into(),
            result_id: String::new(),
            input_ledger_hash: ledger.fingerprint()?,
            graph_snapshot_id: ledger.graph_snapshot_id.clone(),
            findings,
            knowledge_completeness: *knowledge_completeness,
            limitations: limitations.clone(),
            observed_response_model,
            created_at,
        };
        result.result_id = result.content_id()?;
        result.validate(ledger)?;
        Ok(result)
    }

    fn content_id(&self) -> Result<String, InvestigationError> {
        let mut content = self.clone();
        content.result_id.clear();
        Ok(format!(
            "investigation-result-v1:{}",
            crate::source_basis::domain_hash(
                b"cartograph:investigation-result:v1\0",
                &bounded_bytes(&content, MAX_RESULT_BYTES)?,
            )
        ))
    }

    pub fn validate(&self, ledger: &InvestigationInputLedger) -> Result<(), InvestigationError> {
        ledger.validate()?;
        if self.schema_version != 1
            || !super::records::opaque_id(&self.investigation_id)
            || !text(&self.created_at, 64)
            || self.graph_snapshot_id != ledger.graph_snapshot_id
            || self.input_ledger_hash != ledger.fingerprint()?
            || self.result_id != self.content_id()?
            || self
                .observed_response_model
                .as_deref()
                .is_some_and(|v| !text(v, 256) || core_redact::redact_text(v).1 != 0)
        {
            return Err(InvestigationError::InvalidInput);
        }
        let mut ids = BTreeSet::new();
        for (index, finding) in self.findings.iter().enumerate() {
            if finding.finding_id != format!("finding-{}", index + 1)
                || !ids.insert(&finding.finding_id)
                || finding.tier != Tier::Agentic
                || finding.confidence_tier != ConfidenceTier::InferredWeak
            {
                return Err(InvestigationError::InvalidInput);
            }
        }
        let action = InvestigationAction::Finish {
            findings: self
                .findings
                .iter()
                .map(|f| ProposedInvestigationFinding {
                    claim_kind: f.claim_kind,
                    title: f.title.clone(),
                    statement: f.statement.clone(),
                    citation_ids: f.citation_ids.clone(),
                    limitations: f.limitations.clone(),
                })
                .collect(),
            knowledge_completeness: self.knowledge_completeness,
            limitations: self.limitations.clone(),
        };
        let citations = ledger
            .citations
            .iter()
            .map(|c| c.citation_id.clone())
            .collect();
        action.admit_finish(&citations, std::iter::empty())?;
        bounded_bytes(self, MAX_RESULT_BYTES)?;
        Ok(())
    }
}
