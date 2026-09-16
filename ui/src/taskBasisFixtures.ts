import type { TaskSourceBasisV2 } from './store';

/** Shared story fixtures for mixed v1/v2 evidence presentation (AC-0179). */
export const mixedTaskBasis: TaskSourceBasisV2 = {
  schema_version: 2,
  graph_snapshot_id: 'snapshot:prepared',
  selected_facts: [
    { fact: { kind: 'node', id: 'sym:capture' }, fact_digest: 'node-v1:captured',
      binding: { repo_key: 'local/src_registered', receipt_id: 'receipt:original', emitted_fact_digest: 'node-v1:captured' } },
    { fact: { kind: 'node', id: 'sym:send' }, fact_digest: 'node-v1:target', binding: null },
    ...Array.from({ length: 62 }, (_, index) => ({
      fact: { kind: 'node' as const, id: index === 0 ? 'gap:sync' : `unreadable:${index}` },
      fact_digest: `node-v1:omitted-${index}`, binding: null,
    })),
  ],
  evidence: [
    { evidence_id: 'E1', fact: { kind: 'node', id: 'sym:capture' }, role: 'provenance', index: 0,
      origin: { kind: 'captured_primary_source', registered_source_id: 'src_registered',
        receipt_id: 'receipt:original', receipt_inventory_index: 0,
        captured: { file: { source_id: 'src_registered', capture_id: 'capture:original', path: 'src/capture.ts',
          digest: 'retained-file-digest', byte_len: 256 }, byte_start: 10, byte_end: 60 },
        scope: 'primary_source_only', input_closure: 'input_closure_not_established' } },
    { evidence_id: 'E2', fact: { kind: 'node', id: 'sym:send' }, role: 'provenance', index: 0,
      origin: { kind: 'working_tree_unverified' } },
  ],
  selection: {
    metadata_lookahead: 64, acquisition_attempts: 64, supplied_evidence: 2, supplied_candidates: 1,
    captured_validation_bytes: 256,
    limits: { metadata_lookahead: 64, acquisition_attempts: 64, selected_facts: 65, evidence: 12, candidates: 8,
      span_bytes: 8192, total_evidence_bytes: 49152, captured_validation_bytes: 134217728 },
    omissions: Array.from({ length: 62 }, (_, index) => ({
      request_index: index + 1,
      fact: { kind: 'node' as const, id: index === 0 ? 'gap:sync' : `unreadable:${index}` },
      reason: index === 0 ? 'missing_citation' as const : 'legacy_read_unavailable' as const,
    })),
    metadata_preselected_not_read: 0, unread_tail: true, stop_reason: 'attempt_limit',
  },
};
