import type { BasisAssessmentState, BasisComparison, BasisEvidenceStatus } from '../store';

export interface ProposalBasisAssessmentProps {
  state?: BasisAssessmentState;
  onAssess?: () => void;
}

const comparisonLabels: Record<BasisComparison, string> = {
  unchanged: 'Unchanged at this check',
  changed: 'Changed since preparation',
  legacy_unverified: 'Legacy basis — unverified',
  operational_failure: 'Comparison could not be completed',
};

const evidenceLabels: Record<BasisEvidenceStatus, string> = {
  available: 'Retained bytes available',
  unavailable: 'Retained bytes unavailable',
  invalid: 'Retained evidence invalid',
  operational_failure: 'Retained evidence could not be checked',
  validation_byte_limit: 'Retained-byte check reached its limit',
  working_tree_unverified: 'Working-tree input — unverified',
};

/** An explicit observation of the selected proposal; never a review eligibility gate. */
export function ProposalBasisAssessment({ state, onAssess }: ProposalBasisAssessmentProps) {
  const result = state?.result;
  return (
    <section className="egress-section" aria-label="Current basis assessment">
      <h3>Current basis</h3>
      <p className="muted">
        Check the graph, source associations and retained bytes separately.
        This does not certify current checkout contents, complete analysis inputs or business correctness.
      </p>
      {onAssess && (
        <button type="button" className="secondary-button" disabled={state?.loading} onClick={onAssess}>
          {state?.loading ? 'Checking current basis…' : state ? 'Check again' : 'Check current basis'}
        </button>
      )}
      {!state && <p className="muted">Not checked for this selection.</p>}
      {state?.loading && <p className="muted" role="status">Checking the saved basis…</p>}
      {state?.error && <p className="error-text" role="alert">{state.error}</p>}
      {result && (
        <>
          <dl>
            <div><dt>Graph</dt><dd>{comparisonLabels[result.graph_comparison]}</dd></div>
            <div><dt>Source associations</dt><dd>{comparisonLabels[result.association_comparison]}</dd></div>
            <div><dt>Unverified evidence</dt><dd>{result.unverified_evidence} item(s); no working-tree freshness check</dd></div>
          </dl>
          <ul className="consent-notes" aria-label="Retained evidence availability">
            {result.evidence.map((item) => (
              <li key={item.evidence_id}><code>{item.evidence_id}</code> · {evidenceLabels[item.status]}</li>
            ))}
          </ul>
          <details>
            <summary>Assessment details</summary>
            <p>Observed graph: <code>{result.current_graph_snapshot_id ?? 'Unavailable'}</code></p>
            <p>Captured file validation: {result.captured_validation_bytes} / {result.max_captured_validation_bytes} bytes.</p>
          </details>
          <p className="muted">This observation does not change the saved proposal, its review or its awaiting-reconciliation status.</p>
        </>
      )}
    </section>
  );
}
