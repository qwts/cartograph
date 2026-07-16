import { useState } from 'react';
import type { GraphStats, SystemRepo } from '../store';

export interface GraphStatsCardProps {
  stats: GraphStats | null;
  /** Repos in the current system (#162) — the clear confirmation names
   * exactly what is being thrown away. */
  systemContents?: SystemRepo[];
  canClear: boolean;
  clearing: boolean;
  error: string | null;
  onClear: () => void;
}

/** Node/edge counts plus a confirmed clear action, phrased in system terms
 * (#162): the user thinks in projects/systems, not storage layers. */
export function GraphStatsCard({
  stats,
  systemContents,
  canClear,
  clearing,
  error,
  onClear,
}: GraphStatsCardProps) {
  const [confirming, setConfirming] = useState(false);

  return (
    <section className="card">
      <h2>System graph</h2>
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
          {clearing ? 'Clearing…' : 'Clear system'}
        </button>
      ) : (
        <div className="clear-confirmation" role="alert">
          <p>
            Remove every recovered fact
            {systemContents && systemContents.length > 0
              ? ` for ${systemContents.map((entry) => entry.repo).join(', ')}`
              : ' in this system'}
            ? Job history and settings are kept.
          </p>
          <div className="clear-confirmation-actions">
            <button type="button" className="secondary-button" onClick={() => setConfirming(false)}>
              Keep system
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
