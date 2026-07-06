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

/** Point the T0 TypeScript extractor at a local directory (M1 slice). */
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
          placeholder="/path/to/a/typescript/repo"
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
        <p className="muted">
          Job #{summary.job_id}: {summary.files} files → {summary.nodes} nodes,{' '}
          {summary.edges} edges.
        </p>
      )}
      {!summary && !error && (
        <p className="muted">T0 extraction only — every fact Confirmed with evidence, or absent.</p>
      )}
    </section>
  );
}
