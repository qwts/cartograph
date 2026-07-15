import type { IngestSummary } from '../store';

export interface IngestCardProps {
  busy: boolean;
  summary: IngestSummary | null;
  error: string | null;
  /** Disabled when there is no live backend. */
  canIngest: boolean;
  /** Enter the Connect → Preflight → Recover flow (#104). */
  onConnect: () => void;
}

/** Workspace entry into the ingest flow, plus the last recovery outcome
 *  (US-0001: a cloned repo is listed with its commit SHA). */
export function IngestCard({ busy, summary, error, canIngest, onConnect }: IngestCardProps) {
  return (
    <section className="card">
      <h2>Ingest</h2>
      <div className="ingest-form">
        <button type="button" onClick={onConnect} disabled={!canIngest || busy}>
          {busy ? 'Recovering…' : 'Connect a target'}
        </button>
      </div>
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
              <dt>Go</dt>
              <dd data-testid="go-layer-summary">
                {summary.layers.go.files} files · {summary.layers.go.nodes} nodes ·{' '}
                {summary.layers.go.edges} edges
              </dd>
            </div>
            <div>
              <dt>Java</dt>
              <dd data-testid="java-layer-summary">
                {summary.layers.java.files} files · {summary.layers.java.nodes} nodes ·{' '}
                {summary.layers.java.edges} edges
              </dd>
            </div>
            <div>
              <dt>Terraform</dt>
              <dd data-testid="tf-layer-summary">
                {summary.layers.tf.files} files · {summary.layers.tf.nodes} nodes ·{' '}
                {summary.layers.tf.edges} edges
              </dd>
            </div>
            <div>
              <dt>WebExtension</dt>
              <dd data-testid="webext-layer-summary">
                {summary.layers.webext.files} manifests · {summary.layers.webext.nodes} nodes ·{' '}
                {summary.layers.webext.edges} edges
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
