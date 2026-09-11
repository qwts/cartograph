//! Versioned transport and durable metadata. None of these DTOs grants access to
//! a filesystem path, execution lease, current graph or provider.
use super::*;
use crate::{
    InputClosureStatus, PrimarySourceScope, TaskCaptureSpanRef, TaskFactKey, TaskFactSelection,
    TaskRangeRole,
};
use core_prov::EvidenceRef;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationProviderMode {
    Local,
    Cloud,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationProvider {
    pub mode: InvestigationProviderMode,
    pub provider_id: String,
    pub model: String,
    pub endpoint: String,
    pub deployment: Option<String>,
    /// Fixed bounded wire behavior used by this task. Older history has no
    /// recorded value; do not reinterpret it as the current protocol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<String>,
    pub available: bool,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationLimits {
    pub profile: String,
    pub model_invocations: u32,
    pub tool_actions: u32,
    pub selected_facts: usize,
    pub evidence_requests: u32,
    pub evidence_items: usize,
    pub evidence_bytes: usize,
    pub captured_validation_bytes: u64,
    pub generated_tokens_per_invocation: u32,
    pub generated_token_reservations: u32,
    pub active_seconds: u64,
    pub consent_wait_seconds: u64,
    pub wall_seconds: u64,
}

impl Default for InvestigationLimits {
    fn default() -> Self {
        Self {
            profile: "investigation-v1".into(),
            model_invocations: 8,
            tool_actions: 8,
            selected_facts: 64,
            evidence_requests: 64,
            evidence_items: 12,
            evidence_bytes: 48 * 1024,
            captured_validation_bytes: 128 * 1024 * 1024,
            generated_tokens_per_invocation: 2048,
            generated_token_reservations: 16384,
            active_seconds: 600,
            consent_wait_seconds: 900,
            wall_seconds: 3600,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationCatalog {
    pub schema_version: u32,
    pub specialists: Vec<InvestigationSpecialist>,
    pub providers: Vec<InvestigationProvider>,
    pub limits: InvestigationLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartInvestigationRequest {
    pub schema_version: u32,
    pub request_nonce: String,
    pub specialist_id: SpecialistId,
    pub question: String,
    pub scope: InvestigationScope,
    pub provider_mode: InvestigationProviderMode,
    pub limit_profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_graph_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
}

impl StartInvestigationRequest {
    /// Cheap immutable validation; mutable availability and capacity checks must
    /// run after the durable deduplication lookup, not inside this method.
    pub fn validate(&self) -> Result<(), InvestigationError> {
        self.scope.validate()?;
        if self.schema_version != 1
            || !text(&self.question, MAX_QUESTION_BYTES)
            || !opaque_id(&self.request_nonce)
            || self.limit_profile != "investigation-v1"
            || self
                .expected_graph_revision
                .as_ref()
                .is_some_and(|v| !context_hash(v))
            || self.conversation_id.as_ref().is_some_and(|v| !opaque_id(v))
            || self.parent_id.as_ref().is_some_and(|v| !opaque_id(v))
            || (self.parent_id.is_some() && self.conversation_id.is_none())
        {
            return Err(InvestigationError::InvalidRequest);
        }
        bounded_bytes(self, 8 * 1024)?;
        Ok(())
    }

    /// Hash binds original question bytes without storing them. Changing host
    /// provider/role availability must not change an identical nonce's identity.
    pub fn intent_hash(&self) -> Result<String, InvestigationError> {
        self.validate()?;
        Ok(crate::source_basis::domain_hash(
            b"cartograph:investigation-client-intent:v1\0",
            &bounded_bytes(self, 8 * 1024)?,
        ))
    }
}

pub(crate) fn opaque_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_:".contains(&b))
}
pub(crate) fn context_hash(value: &str) -> bool {
    value
        .strip_prefix("context-v1:")
        .is_some_and(crate::source_basis::hash)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationStatus {
    Queued,
    Preparing,
    Running,
    AwaitingConsent,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    OutcomeUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationActions {
    pub can_cancel: bool,
    pub can_follow_up: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationSummary {
    pub schema_version: u32,
    pub investigation_id: String,
    pub conversation_id: String,
    pub parent_id: Option<String>,
    pub job_id: i64,
    pub specialist_id: SpecialistId,
    pub question: String,
    pub scope: InvestigationScope,
    pub provider_mode: InvestigationProviderMode,
    pub status: InvestigationStatus,
    pub revision: u64,
    pub cancel_requested: bool,
    pub invocation_pending: bool,
    pub actions: InvestigationActions,
    pub graph_snapshot_id: Option<String>,
    pub last_event_sequence: u64,
    pub has_result: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationUsage {
    pub model_invocations: u32,
    pub tool_actions: u32,
    pub selected_facts: usize,
    pub evidence_requests: u32,
    pub evidence_items: usize,
    pub evidence_bytes: usize,
    pub captured_validation_bytes: u64,
    pub generated_token_reservations: u32,
    pub reported_input_tokens: Option<u64>,
    pub reported_output_tokens: Option<u64>,
    pub active_milliseconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvestigationDetail {
    #[serde(flatten)]
    pub summary: InvestigationSummary,
    pub context_owner: String,
    pub origin: InvestigationOrigin,
    pub specialist: InvestigationSpecialist,
    pub provider: InvestigationProvider,
    pub scope_snapshot_id: Option<String>,
    pub limits: InvestigationLimits,
    pub usage: InvestigationUsage,
    pub citations: Vec<InvestigationCitation>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationOrigin {
    App,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationPage {
    pub items: Vec<InvestigationSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationEventKind {
    Created,
    PreparationStarted,
    ContextPrepared,
    ModelStarted,
    ModelCompleted,
    ActionAdmitted,
    ToolStarted,
    ToolCompleted,
    ConsentRequired,
    ConsentApproved,
    ConsentDeclined,
    CancelRequested,
    ResultPersisted,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    OutcomeUnknown,
    EvidenceValidationReserved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationEvent {
    pub investigation_id: String,
    pub sequence: u64,
    pub revision: u64,
    pub kind: InvestigationEventKind,
    pub step_id: Option<String>,
    pub tool: Option<InvestigationTool>,
    pub summary: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationEventPage {
    pub investigation_id: String,
    pub items: Vec<InvestigationEvent>,
    pub next_sequence: u64,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationResult {
    pub schema_version: u32,
    pub investigation_id: String,
    pub result_id: String,
    pub input_ledger_hash: String,
    pub graph_snapshot_id: String,
    pub findings: Vec<InvestigationFinding>,
    pub knowledge_completeness: KnowledgeCompleteness,
    pub limitations: Vec<String>,
    pub observed_response_model: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InvestigationEvidenceOrigin {
    GraphMetadata,
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
pub struct InvestigationCitation {
    pub citation_id: String,
    pub fact: TaskFactKey,
    pub fact_digest: String,
    pub source: Option<EvidenceRef>,
    pub role: Option<TaskRangeRole>,
    pub index: Option<u32>,
    pub text_hash: Option<String>,
    pub origin: InvestigationEvidenceOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationConsent {
    pub investigation_id: String,
    pub step_id: String,
    pub revision: u64,
    pub preview: llm::EgressPreview,
    pub provider_profile: llm::bounded::ProviderProfile,
    pub completion_limits: llm::bounded::CompletionLimits,
    pub provider: InvestigationProvider,
    pub expires_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationCitationStatus {
    Available,
    MetadataOnly,
    WorkingTreeUnverified,
    Unavailable,
    Invalid,
    OperationalFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationCitationRead {
    pub investigation_id: String,
    pub citation_id: String,
    pub status: InvestigationCitationStatus,
    pub citation: InvestigationCitation,
    pub text: Option<String>,
}

/// Raw query results and source bytes are not stored here. Their content hashes
/// bind the exact supplied payloads; they cannot reconstruct a lost transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationQueryManifest {
    pub query: InvestigationQuery,
    pub response_hash: String,
    pub response_bytes: usize,
    pub returned_facts: usize,
    pub total_selected: usize,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationInputLedger {
    pub schema_version: u32,
    pub graph_snapshot_id: String,
    pub scope_snapshot_id: String,
    pub revision: u64,
    pub selected_facts: Vec<TaskFactSelection>,
    pub receipt_references: Vec<InvestigationReceiptReference>,
    pub citations: Vec<InvestigationCitation>,
    pub queries: Vec<InvestigationQueryManifest>,
    pub supplied_history_hash: Option<String>,
    pub supplied_history_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationReceiptReference {
    pub source_id: String,
    pub repo_key: String,
    pub receipt_id: String,
}

impl InvestigationInputLedger {
    pub fn fingerprint(&self) -> Result<String, InvestigationError> {
        self.validate()?;
        Ok(crate::source_basis::domain_hash(
            b"cartograph:investigation-input-ledger:v1\0",
            &bounded_bytes(self, MAX_MANIFEST_BYTES)?,
        ))
    }
}
