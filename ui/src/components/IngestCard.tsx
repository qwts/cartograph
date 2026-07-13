import { useState } from 'react';
import type { IngestSummary } from '../store';

export interface IngestCardProps {
  busy: boolean;
  summary: IngestSummary | null;
  error: string | null;
  /** Disabled when there is no live backend. */
  canIngest: boolean;
  onIngest: (path: string) => void;
}

/** Point the T0 extractors at a local directory or a GitHub repo
 *  (US-0001: a cloned repo is listed with its commit SHA). */
export function IngestCard({ busy, summary, error, canIngest, onIngest }: IngestCardProps) {
  const [path, setPath] = useState('');

  return (
    <section className="card">
      <h2>Ingest</h2>
      <form
        className="ingest-form"
        onSubmit={(e) => {
          e.preventDefault();
          if (path.trim()) onIngest(path.trim());
        }}
      >
        <input
          type="text"
          value={path}
          placeholder="/path, github URL, or cartograph.system.toml"
          aria-label="Directory to ingest"
          onChange={(e) => setPath(e.target.value)}
          disabled={!canIngest || busy}
        />
        <button type="submit" disabled={!canIngest || busy || path.trim() === ''}>
          {busy ? 'Ingesting…' : 'Ingest'}
        </button>
      </form>
      {error && <p className="error-text">{error}</p>}
      {summary && !busy && (
        <div className="ingest-summary">
          <p className="muted">
            {summary.repos && summary.repos.length > 0 && (
              <>
                <code data-testid="system-repos">{summary.repos.join(', ')}</code> —{' '}
              </>
            )}
            {summary.repo && summary.commit_sha && (
              <>
                <code data-testid="cloned-repo">
                  {summary.repo} @ {summary.commit_sha.slice(0, 12)}
                </code>{' '}
                —{' '}
              </>
            )}
            Job #{summary.job_id}: {summary.files} files → {summary.nodes} nodes,{' '}
            {summary.edges} edges.
          </p>
          <dl className="layer-summary" aria-label="Ingest breakdown by source layer">
            <div>
              <dt>TypeScript</dt>
              <dd data-testid="ts-layer-summary">
                {summary.layers.ts.files} files · {summary.layers.ts.nodes} nodes ·{' '}
                {summary.layers.ts.edges} edges
              </dd>
            </div>
            <div>
              <dt>Python</dt>
              <dd data-testid="python-layer-summary">
                {summary.layers.python.files} files · {summary.layers.python.nodes} nodes ·{' '}
                {summary.layers.python.edges} edges
              </dd>
            </div>
            <div>
              <dt>Terraform</dt>
              <dd data-testid="tf-layer-summary">
                {summary.layers.tf.files} files · {summary.layers.tf.nodes} nodes ·{' '}
                {summary.layers.tf.edges} edges
              </dd>
            </div>
          </dl>
          {summary.delta && (
            <p className="muted" data-testid="delta-summary">
              Delta: {summary.delta.recomputed_files} recomputed · {summary.delta.reused_files}{' '}
              reused · {summary.delta.deleted_files} removed
            </p>
          )}
        </div>
      )}
      {!summary && !error && (
        <p className="muted">T0 extraction only — every fact Confirmed with evidence, or absent.</p>
      )}
    </section>
  );
}
