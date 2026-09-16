import type { RetainedSource, RetentionPreview } from '../primarySourceStore';

export interface SourceRetentionProps {
  sources: RetainedSource[];
  preview: RetentionPreview | null;
  busy: boolean;
  error: string | null;
  message: string | null;
  onPreview: (sourceId: string) => void;
  onDismiss: () => void;
  onForget: () => void;
}

/** Source-specific removal requires a current host preview before confirmation. */
export function SourceRetention({ sources, preview, busy, error, message, onPreview, onDismiss, onForget }: SourceRetentionProps) {
  return <section className="card source-retention" aria-label="Retained source storage">
    <h2>Retained source storage</h2>
    <p>Captured primary source is stored locally for evidence inspection. Forgetting it preserves receipt and review history, and makes those source reads unavailable.</p>
    <p className="muted">New recovery can retain source again. Clearing the graph or jobs does not forget source.</p>
    {sources.length === 0 && <p className="muted">No sources have been registered.</p>}
    <ul>{sources.map((source) => <li key={source.source_id}>
      <span>{source.display_name} <code>{source.repo_key}</code></span>{' '}
      <button type="button" disabled={busy} onClick={() => onPreview(source.source_id)} aria-label={`Preview retained source for ${source.repo_key}`}>Preview storage</button>
    </li>)}</ul>
    {busy && <p role="status">Checking retained source…</p>}
    {error && <p role="alert">{error}</p>}
    {message && <p role="status">{message}</p>}
    {preview && <div role="group" aria-label="Confirm source forgetting">
      <h3>Forget retained source for {preview.display_name}?</h3>
      <p><code>{preview.repo_key}</code></p>
      <p>{preview.captures} captures · {preview.files} file entries · {preview.bytes.toLocaleString()} captured bytes</p>
      <p>{preview.current_references} current and {preview.historical_references} historical receipt references; {preview.receipts} receipt records will be preserved.</p>
      {preview.staged_references !== undefined && <p>{preview.staged_references} staged evidence references use retained receipts. Their proposal history remains, but forgotten source bytes become unavailable.</p>}
      {preview.investigation_references !== undefined && <p>{preview.investigation_references} investigation references use retained receipts. Findings and their citation history remain, but forgotten source bytes become unavailable.</p>}
      <p>This removes only this source’s retained content. Other sources and their shared bytes remain available.</p>
      <button type="button" disabled={busy} onClick={onDismiss}>Keep source</button>{' '}
      <button type="button" disabled={busy || preview.captures === 0} onClick={onForget}>Forget retained source</button>
    </div>}
  </section>;
}
