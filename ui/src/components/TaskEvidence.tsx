import type { TaskSourceBasisV2 } from '../store';

export interface TaskEvidenceProps {
  basis?: TaskSourceBasisV2;
  compact?: boolean;
}

/** Persisted/prepared input coverage. Availability is a separate, requested observation. */
export function TaskEvidence({ basis, compact = false }: TaskEvidenceProps) {
  if (!basis) {
    return (
      <p className="muted">
        {compact ? 'Source binding unverified · legacy evidence' :
          'Source binding unverified: legacy working-tree spans have no parser-input receipt. Acceptance does not certify evidence freshness.'}
      </p>
    );
  }
  const captured = basis.evidence.filter((item) => item.origin.kind === 'captured_primary_source').length;
  const unverified = basis.evidence.length - captured;
  const selection = basis.selection;
  const stop = {
    candidate_limit: 'Candidate limit reached',
    neighborhood_exhausted: 'Candidate neighborhood exhausted',
    attempt_limit: 'Evidence request limit reached',
    validation_byte_limit: 'Captured file validation limit reached',
    participating_source_unavailable: 'Participating source unavailable',
    invalid_selection: 'Source selection could not be validated',
    required_membership_missing: 'Required evidence or candidate missing',
  }[selection.stop_reason];
  const limited = selection.stop_reason !== 'neighborhood_exhausted' || selection.omissions.length > 0;
  return (
    <div className="task-evidence">
      <p className="muted">Source input: {captured} retained parser span(s) · {unverified} working-tree span(s), unverified.</p>
      {compact ? limited && <p className="muted">Selection limits or omissions apply.</p> : (
        <>
          <p className="muted">
            Retained spans establish primary parser input only. Complete analysis inputs and business meaning remain unestablished.
            Acceptance does not certify evidence freshness.
          </p>
          <p className="muted">
            {stop}. Supplied {selection.supplied_evidence} evidence item(s) and {selection.supplied_candidates} candidate(s).
            {' '}{selection.omissions.length} omission(s); {selection.metadata_preselected_not_read} preselected item(s) not read;
            {' '}{selection.unread_tail ? 'Further candidates beyond the selection window were not inspected.' :
              'No further candidates beyond the selection window.'}
          </p>
          <details>
            <summary>Evidence origins ({basis.evidence.length})</summary>
            <ul className="consent-notes">
              {basis.evidence.map((item) => (
                <li key={item.evidence_id}>
                  <code>{item.evidence_id}</code> · {item.origin.kind === 'captured_primary_source' ? (
                    <>
                      Retained parser input · <code>{item.origin.captured.file.path}</code>
                      {' · bytes '}{item.origin.captured.byte_start}..{item.origin.captured.byte_end}
                      <br />Registered source: <code>{item.origin.registered_source_id}</code>
                      <br />Receipt: <code>{item.origin.receipt_id}</code>
                      {' · inventory '}{item.origin.receipt_inventory_index}
                    </>
                  ) : 'Working-tree input — unverified'}
                  {' · citation '}{item.role} / {item.index}
                </li>
              ))}
            </ul>
          </details>
          <details>
            <summary>Selection details</summary>
            <p>Metadata lookahead: {selection.metadata_lookahead} / {selection.limits.metadata_lookahead}.</p>
            <p>Evidence requests: {selection.acquisition_attempts} / {selection.limits.acquisition_attempts}.</p>
            <p>Captured file validation: {selection.captured_validation_bytes} / {selection.limits.captured_validation_bytes} bytes.</p>
            <p>Payload limits: {selection.limits.evidence} spans, {selection.limits.candidates} candidates,
              {' '}{selection.limits.span_bytes} bytes per span and {selection.limits.total_evidence_bytes} bytes combined.</p>
            <ul className="consent-notes">
              {selection.omissions.map((item) => (
                <li key={item.request_index}>Request {item.request_index}: {item.reason === 'missing_citation'
                  ? 'No citation supplied' : 'Legacy working-tree read unavailable'}.</li>
              ))}
            </ul>
            <p>Prepared graph: <code>{basis.graph_snapshot_id}</code></p>
          </details>
          <details>
            <summary>Saved basis metadata</summary>
            <pre>{JSON.stringify(basis, null, 2)}</pre>
          </details>
        </>
      )}
    </div>
  );
}
