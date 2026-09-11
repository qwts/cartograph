import type {
  InvestigationCatalog, InvestigationCitation, InvestigationConsent, InvestigationDetail,
  InvestigationEvent, InvestigationResult,
} from './investigationTypes';

/** Scripted UI data only. No real provider execution or H4 acceptance is implied. */
export const investigationCatalog: InvestigationCatalog = {
  schema_version: 1,
  specialists: [
    { id: 'domain-analyst@2', name: 'Domain analyst', version: 2, prompt_fingerprint: 'prompt:domain-v2',
      purpose: 'Explain business behavior and its unresolved dependencies.',
      operations: ['query_context', 'read_evidence', 'finish'], tier: 'Agentic', confidence_tier: 'InferredWeak' },
    { id: 'evidence-auditor@2', name: 'Evidence auditor', version: 2, prompt_fingerprint: 'prompt:audit-v2',
      purpose: 'Inspect the support and limits of recovered claims.',
      operations: ['query_context', 'read_evidence', 'finish'], tier: 'Agentic', confidence_tier: 'InferredWeak' },
  ],
  providers: [
    { mode: 'local', provider_id: 'ollama', model: 'fixture-local-model', endpoint: 'http://127.0.0.1:11434',
      deployment: null, available: true, unavailable_reason: null },
    { mode: 'cloud', provider_id: 'azure-openai', model: 'fixture-cloud-model', endpoint: 'https://example.openai.azure.com',
      deployment: 'investigation-fixture', available: true, unavailable_reason: null },
  ],
  limits: { profile: 'investigation-v1', model_invocations: 8, tool_actions: 8, selected_facts: 64,
    evidence_requests: 64, evidence_items: 12, evidence_bytes: 49152, captured_validation_bytes: 134217728,
    generated_tokens_per_invocation: 2048, generated_token_reservations: 16384,
    active_seconds: 600, consent_wait_seconds: 900, wall_seconds: 3600 },
};

export const investigationCitations: InvestigationCitation[] = [
  { citation_id: 'C1', fact: { kind: 'node', id: 'rule:stock' }, fact_digest: 'node-v1:original-rule',
    source: { repo: 'local/src_original', path: 'src/stock.ts', byte_start: 12, byte_end: 54, commit_sha: 'working-tree' },
    role: 'definition_expression', index: 2, text_hash: 'text:original',
    origin: { kind: 'captured_primary_source', registered_source_id: 'src_original', receipt_id: 'receipt:original',
      receipt_inventory_index: 8, captured: { file: { source_id: 'src_original', capture_id: 'capture:original',
        path: 'src/stock.ts', digest: 'file:original', byte_len: 180 }, byte_start: 12, byte_end: 54 },
      scope: 'primary_source_only', input_closure: 'input_closure_not_established' } },
  { citation_id: 'C2', fact: { kind: 'node', id: 'gap:stock' }, fact_digest: 'node-v1:original-gap',
    source: null, role: null, index: null, text_hash: null, origin: { kind: 'graph_metadata' } },
  { citation_id: 'C3', fact: { kind: 'node', id: 'legacy:stock' }, fact_digest: 'node-v1:legacy',
    source: { repo: 'legacy', path: 'src/old.ts', byte_start: 0, byte_end: 20, commit_sha: 'old' },
    role: 'provenance', index: 0, text_hash: 'text:legacy', origin: { kind: 'working_tree_unverified' } },
];

export function investigationDetail(id = 'inv-fixture', overrides: Partial<InvestigationDetail> = {}): InvestigationDetail {
  return { schema_version: 1, investigation_id: id, conversation_id: `conversation-${id}`, parent_id: null,
    job_id: 91, specialist_id: 'domain-analyst@2', question: 'What stock behavior is supported, and what remains unknown?',
    scope: { type: 'neighborhood', anchor: 'rule:stock', hops: 1 }, provider_mode: 'local',
    status: 'completed', revision: 9, cancel_requested: false, invocation_pending: false,
    actions: { can_cancel: false, can_follow_up: true }, graph_snapshot_id: 'snapshot:original',
    last_event_sequence: 9, has_result: true, created_at: '2026-09-10T10:00:00Z', updated_at: '2026-09-10T10:00:09Z',
    context_owner: 'system:registered', origin: 'app', specialist: investigationCatalog.specialists[0],
    provider: investigationCatalog.providers[0], scope_snapshot_id: 'scope:original', limits: investigationCatalog.limits,
    usage: { model_invocations: 2, tool_actions: 1, selected_facts: 2, evidence_requests: 1, evidence_items: 1,
      evidence_bytes: 42, captured_validation_bytes: 180, generated_token_reservations: 4096,
      reported_input_tokens: null, reported_output_tokens: null, active_milliseconds: 9000 },
    citations: investigationCitations, error: null, ...overrides };
}

export function investigationEvents(id = 'inv-fixture'): InvestigationEvent[] {
  return [
    ['created', 'Investigation saved before preparation.', null],
    ['preparation_started', 'Preparing the authorized graph scope.', null],
    ['context_prepared', 'Prepared two selected facts from the frozen graph.', null],
    ['model_started', 'Started invocation 1.', null],
    ['tool_started', 'Reading admitted citation C1.', 'read_evidence'],
    ['evidence_validation_reserved', 'Reserved 180 captured-file validation bytes.', 'read_evidence'],
    ['tool_completed', 'Admitted the retained source occurrence.', 'read_evidence'],
    ['result_persisted', 'Saved cited findings against the original ledger.', null],
    ['completed', 'Execution completed; knowledge remains partial.', null],
  ].map(([kind, summary, tool], index) => ({ investigation_id: id, sequence: index + 1, revision: index + 1,
    kind: kind as InvestigationEvent['kind'], tool: tool as InvestigationEvent['tool'],
    summary: summary!, step_id: index >= 3 ? 'step-1' : null, created_at: `2026-09-10T10:00:0${index}Z` }));
}

export function investigationResult(id = 'inv-fixture'): InvestigationResult {
  return { schema_version: 1, investigation_id: id, result_id: `result:${id}`, input_ledger_hash: 'ledger:original',
    graph_snapshot_id: 'snapshot:original', knowledge_completeness: 'partial',
    findings: [
      { finding_id: 'F1', claim_kind: 'implemented_behavior', title: 'A local stock guard is present',
        statement: 'The inspected callable has a guarded exit. Its broader inventory policy remains unresolved.',
        citation_ids: ['C1', 'C2'], limitations: ['Runtime member values were not established.'],
        tier: 'Agentic', confidence_tier: 'InferredWeak' },
      { finding_id: 'F2', claim_kind: 'proposed_design', title: 'Clarify the policy boundary',
        statement: 'Document the conditions under which the surrounding stock policy applies.',
        citation_ids: ['C3'], limitations: ['This is a proposal, not observed project intent.'],
        tier: 'Agentic', confidence_tier: 'InferredWeak' },
    ], limitations: ['This bounded investigation does not establish complete feature coverage.'],
    observed_response_model: 'fixture-local-model', created_at: '2026-09-10T10:00:08Z' };
}

export function investigationConsent(id = 'inv-fixture', revision = 4, step = 'step-1'): InvestigationConsent {
  return { investigation_id: id, step_id: step, revision, provider: investigationCatalog.providers[1],
    expires_at: '2026-09-10T10:05:00Z',
    preview: { provider_id: 'azure-openai', locality: 'Cloud', tier: 'Agentic', action_id: `${id}/${step}`,
      payload: { system: 'Use admitted references only.', prompt: 'Inspect the selected stock guard.',
        spans: [{ id: 'C1', repo: 'local/src_original', path: 'src/stock.ts', byte_start: 12, byte_end: 54,
          commit_sha: 'working-tree', text: 'Retained fixture input approved for this step.' }] },
      payload_hash: `payload:${id}:${step}`, redaction_count: 0 },
    provider_profile: { protocol_version: 'bounded-completion-v1', provider_id: 'azure-openai',
      locality: 'Cloud', endpoint_id: 'https://example.openai.azure.com/investigation-fixture', requested_model: 'fixture-cloud-model' },
    completion_limits: { input_bytes: 98304, request_bytes: 131072, response_bytes: 65536,
      output_text_bytes: 16384, max_output_tokens: 2048, request_timeout_ms: 30000 } };
}
