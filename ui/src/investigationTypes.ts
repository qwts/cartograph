/** SPEC-10 transport DTOs. Findings/history are separate from edge proposals.
 * Snake-case fields mirror the host; command argument names remain Tauri camelCase.
 * No execution token, live source path, raw transcript or model reasoning is exposed.
 */
import type { EgressPreview } from './components/EgressConsentDialog';
import type { FactKey } from './primarySourceStore';
import type { EvidenceRef, TaskSourceBasisV2 } from './store';

export type SpecialistId = 'domain-analyst@1' | 'evidence-auditor@1' | 'domain-analyst@2' | 'evidence-auditor@2';
export type InvestigationProviderMode = 'local' | 'cloud';
/** The existing context-hub QueryScope wire shape, not a filesystem scope. */
export type InvestigationScope = { type: 'all' } |
  { type: 'neighborhood'; anchor: string; hops: number };
export type InvestigationTool = 'query_context' | 'read_evidence';

export interface InvestigationSpecialist {
  id: SpecialistId;
  name: string;
  version: number;
  prompt_fingerprint: string;
  purpose: string;
  operations: (InvestigationTool | 'finish')[];
  tier: 'Agentic';
  confidence_tier: 'InferredWeak';
}

export interface InvestigationProvider {
  mode: InvestigationProviderMode;
  provider_id: string;
  model: string;
  endpoint: string;
  deployment: string | null;
  /** Absent in older records; never infer the current protocol for that history. */
  protocol_version?: string;
  available: boolean;
  unavailable_reason: string | null;
}

/** The host may narrow these ceilings; the UI never promises exact token/cost use. */
export interface InvestigationLimits {
  profile: string;
  model_invocations: number;
  tool_actions: number;
  selected_facts: number;
  evidence_requests: number;
  evidence_items: number;
  evidence_bytes: number;
  captured_validation_bytes: number;
  generated_tokens_per_invocation: number;
  generated_token_reservations: number;
  active_seconds: number;
  consent_wait_seconds: number;
  wall_seconds: number;
}

export interface InvestigationCatalog {
  schema_version: 1;
  specialists: InvestigationSpecialist[];
  providers: InvestigationProvider[];
  limits: InvestigationLimits;
}

export interface StartInvestigationRequest {
  schema_version: 1;
  request_nonce: string;
  specialist_id: SpecialistId;
  question: string;
  scope: InvestigationScope;
  provider_mode: InvestigationProviderMode;
  limit_profile: string;
  expected_graph_revision?: string;
  conversation_id?: string;
  parent_id?: string;
}

export type InvestigationStatus = 'queued' | 'preparing' | 'running' |
  'awaiting_consent' | 'completed' | 'failed' | 'cancelled' | 'interrupted' |
  'outcome_unknown';

/** Bounded history summary. Question is the host's redacted saved representation. */
export interface InvestigationSummary {
  schema_version: 1;
  investigation_id: string;
  conversation_id: string;
  parent_id: string | null;
  job_id: number;
  specialist_id: SpecialistId;
  question: string;
  scope: InvestigationScope;
  provider_mode: InvestigationProviderMode;
  status: InvestigationStatus;
  revision: number;
  cancel_requested: boolean;
  invocation_pending: boolean;
  actions: { can_cancel: boolean; can_follow_up: boolean };
  graph_snapshot_id: string | null;
  last_event_sequence: number;
  has_result: boolean;
  created_at: string;
  updated_at: string;
}

export interface InvestigationUsage {
  model_invocations: number;
  tool_actions: number;
  selected_facts: number;
  evidence_requests: number;
  evidence_items: number;
  evidence_bytes: number;
  captured_validation_bytes: number;
  generated_token_reservations: number;
  reported_input_tokens: number | null;
  reported_output_tokens: number | null;
  active_milliseconds: number;
}

/** Definition/provider are the saved identities, not the current catalog entries. */
export interface InvestigationDetail extends InvestigationSummary {
  context_owner: string;
  origin: 'app';
  specialist: InvestigationSpecialist;
  provider: InvestigationProvider;
  scope_snapshot_id: string | null;
  limits: InvestigationLimits;
  usage: InvestigationUsage;
  citations: InvestigationCitation[];
  error: string | null;
}

export interface InvestigationPage {
  items: InvestigationSummary[];
  next_cursor: string | null;
}

export type InvestigationEventKind = 'created' | 'preparation_started' |
  'context_prepared' | 'model_started' | 'model_completed' | 'action_admitted' |
  'tool_started' | 'tool_completed' | 'consent_required' | 'consent_approved' |
  'evidence_validation_reserved' |
  'consent_declined' | 'cancel_requested' | 'result_persisted' | 'completed' |
  'failed' | 'cancelled' | 'interrupted' | 'outcome_unknown';

/** Ordered durable activity; summary is fixed/sanitized host metadata, never thinking. */
export interface InvestigationEvent {
  investigation_id: string;
  sequence: number;
  revision: number;
  kind: InvestigationEventKind;
  step_id: string | null;
  tool: InvestigationTool | null;
  summary: string;
  created_at: string;
}

export interface InvestigationEventPage {
  investigation_id: string;
  items: InvestigationEvent[];
  /** Last sequence in this page, or the requested cursor for an empty page. */
  next_sequence: number;
  has_more: boolean;
}

/** Push messages invalidate observations; durable event pages remain authoritative. */
export interface InvestigationChanged {
  investigation_id: string;
  last_event_sequence: number;
  revision: number;
}

export type InvestigationClaimKind = 'implemented_behavior' | 'documented_intent' |
  'inferred_interpretation' | 'proposed_design';

export interface InvestigationFinding {
  finding_id: string;
  claim_kind: InvestigationClaimKind;
  title: string;
  statement: string;
  citation_ids: string[];
  limitations: string[];
  tier: 'Agentic';
  confidence_tier: 'InferredWeak';
}

export interface InvestigationResult {
  schema_version: 1;
  investigation_id: string;
  result_id: string;
  input_ledger_hash: string;
  graph_snapshot_id: string;
  findings: InvestigationFinding[];
  knowledge_completeness: 'partial' | 'insufficient_evidence';
  limitations: string[];
  observed_response_model: string | null;
  created_at: string;
}

export interface InvestigationCitation {
  citation_id: string;
  fact: FactKey;
  fact_digest: string;
  source: EvidenceRef | null;
  role: string | null;
  index: number | null;
  text_hash: string | null;
  origin: { kind: 'graph_metadata' } | TaskSourceBasisV2['evidence'][number]['origin'];
}

/** Exact payload exists only while the owning worker can authorize this step. */
export interface InvestigationConsent {
  investigation_id: string;
  step_id: string;
  revision: number;
  preview: EgressPreview;
  provider: InvestigationProvider;
  expires_at: string;
  provider_profile: {
    protocol_version: string;
    provider_id: string;
    locality: 'Local' | 'Cloud';
    endpoint_id: string;
    requested_model: string;
  };
  completion_limits: {
    input_bytes: number;
    request_bytes: number;
    response_bytes: number;
    output_text_bytes: number;
    max_output_tokens: number;
    request_timeout_ms: number;
  };
}

/** Historical lookup uses task/citation identity only; no current-fact fallback. */
export interface InvestigationCitationRead {
  investigation_id: string;
  citation_id: string;
  status: 'available' | 'metadata_only' | 'working_tree_unverified' |
    'unavailable' | 'invalid' | 'operational_failure';
  citation: InvestigationCitation;
  text: string | null;
}
