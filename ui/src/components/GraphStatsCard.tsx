import { useState } from 'react';
import type { GraphStats } from '../store';

export interface GraphStatsCardProps {
  stats: GraphStats | null;
  canClear: boolean;
  clearing: boolean;
  error: string | null;
  onClear: () => void;
}

/** Node/edge counts plus a confirmed clear action for disposable graph facts. */
export function GraphStatsCard({
  stats,
  canClear,
  clearing,
  error,
  onClear,
}: GraphStatsCardProps) {
  const [confirming, setConfirming] = useState(false);

  return (
    <section className="card">
      <h2>Unified graph</h2>
      <div className="stat" data-testid="graph-node-count">
        {stats ? stats.nodes : '—'}
      </div>
      <div className="stat-label">nodes</div>
      <div className="stat" data-testid="graph-edge-count">
        {stats ? stats.edges : '—'}
      </div>
      <div className="stat-label">edges</div>
      {!confirming ? (
        <button
          className="clear-graph-button"
          type="button"
          disabled={!canClear || clearing}
          onClick={() => setConfirming(true)}
        >
          {clearing ? 'Clearing…' : 'Clear graph'}
        </button>
      ) : (
        <div className="clear-confirmation" role="alert">
          <p>Clear all graph facts? Job history will be kept.</p>
          <div className="clear-confirmation-actions">
            <button type="button" className="secondary-button" onClick={() => setConfirming(false)}>
              Keep graph
            </button>
            <button
              type="button"
              className="danger-button"
              disabled={clearing}
              onClick={() => {
                setConfirming(false);
                onClear();
              }}
            >
              Confirm clear
            </button>
          </div>
        </div>
      )}
      {error && <p className="error-text">{error}</p>}
    </section>
  );
}
