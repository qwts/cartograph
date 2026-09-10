import type { CapturedDescription, CapturedText } from '../primarySourceStore';

export interface CapturedSourceProps {
  description: CapturedDescription | null;
  text: CapturedText | null;
  loading: boolean;
  error: string | null;
  onRead: (index: number) => void;
}

/** Captured source has its own identity and never replaces a live source window. */
export function CapturedSource({ description, text, loading, error, onRead }: CapturedSourceProps) {
  return (
    <section className="captured-source" aria-label="Captured primary source">
      <h3>Captured primary source</h3>
      <p className="muted">Full input coverage and business interpretation are not established.</p>
      {error && <p role="status">{error}</p>}
      {!description && !loading && !error && <p className="muted">No retained primary-source binding is available for this fact.</p>}
      {description && <>
        <p><code>{description.repo_key}</code></p>
        <label>
          Cited range
          <select aria-label="Captured cited range" defaultValue="" key={description.receipt_id}
            disabled={loading} onChange={(event) => {
              if (event.target.value !== '') onRead(Number(event.target.value));
            }}>
            <option value="" disabled>Choose a captured range</option>
            {description.ranges.map((range) => <option key={range.index} value={range.index}>
              {range.index + 1}. {range.path} · bytes {range.byte_start}–{range.byte_end}
            </option>)}
          </select>
        </label>
        <details><summary>Capture reference</summary><code>{description.receipt_id}</code></details>
      </>}
      {loading && <p role="status">Loading captured source…</p>}
      {text && description && text.receipt_id === description.receipt_id && <>
        <p>{text.path} · bytes {text.byte_start}–{text.byte_end}</p>
        <pre data-testid="captured-source-text">{text.text}</pre>
      </>}
    </section>
  );
}
