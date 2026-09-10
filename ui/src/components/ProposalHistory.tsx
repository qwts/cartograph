import type { StagedProposal } from '../store';
import { TierBadge } from './TierBadge';

export interface ProposalHistoryProps {
  proposals: StagedProposal[];
  loading: boolean;
  error: string | null;
  hasMore: boolean;
  onRefresh: () => void;
  onLoadMore: () => void;
  onOpen: (proposal: StagedProposal) => void;
}

/** Durable review history; all data and actions come from the host-backed store. */
export function ProposalHistory({
  proposals, loading, error, hasMore, onRefresh, onLoadMore, onOpen,
}: ProposalHistoryProps) {
  return (
    <section className="proposal-history" aria-label="Proposal history">
      <header className="proposal-history-heading">
        <div>
          <h3>Pending and reviewed proposals</h3>
          <p className="muted">Saved proposals remain available after restart and job cleanup.</p>
        </div>
        <button type="button" className="secondary-button" disabled={loading} onClick={onRefresh}>
          Refresh proposal history
        </button>
      </header>
      <p className="muted">
        Accepted proposals await context reconciliation. Review preserves T3 / InferredWeak;
        it does not yet change context or exports. Source binding is unverified.
      </p>
      {error && <p className="error-text" role="alert">{error}</p>}
      {loading && <p className="muted" role="status">Loading saved proposals…</p>}
      {!loading && !error && proposals.length === 0 && (
        <p className="muted">No saved proposals yet.</p>
      )}
      {proposals.length > 0 && (
        <>
          <p className="muted">{proposals.length} loaded</p>
          <ul className="proposal-history-list" aria-label="Saved proposals">
            {proposals.map((proposal) => (
              <li key={proposal.proposal_id}>
                <div className="proposal-history-heading">
                  <strong>
                    {proposal.review_decision === 'accepted'
                      ? 'Accepted · awaiting context reconciliation'
                      : proposal.review_decision === 'rejected' ? 'Rejected' : 'Pending review'}
                  </strong>
                  <TierBadge tier={proposal.provenance.confidence_tier} />
                </div>
                <p><code>{proposal.source_id}</code> —{proposal.edge_label}→ <code>{proposal.target_id}</code></p>
                <p className="proposal-annotation">{proposal.annotation}</p>
                <p className="muted">Source binding unverified · review revision {proposal.review_revision}</p>
                <button type="button" onClick={() => onOpen(proposal)}>
                  {proposal.review_decision ? 'View reviewed proposal' : 'Review proposal'}
                </button>
              </li>
            ))}
          </ul>
        </>
      )}
      {hasMore && (
        <button type="button" className="register-show-more" disabled={loading} onClick={onLoadMore}>
          Load more proposals
        </button>
      )}
    </section>
  );
}
