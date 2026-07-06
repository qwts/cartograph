import type { GraphNode, SourceState } from '../store';
import { TierBadge } from './TierBadge';

export interface EvidencePanelProps {
  node: GraphNode;
  source: SourceState;
  onClose: () => void;
}

/**
 * Split `text` at an evidence byte span (provenance spans are byte offsets
 * into the original file, not UTF-16 indices — encode to compare apples to
 * apples). `windowStart` is the byte offset of `text` within the file when
 * the backend returned a window of a large file.
 */
function splitAtSpan(text: string, byteStart: number, byteEnd: number, windowStart: number) {
  const bytes = new TextEncoder().encode(text);
  const decoder = new TextDecoder();
  const start = Math.max(0, byteStart - windowStart);
  const end = Math.max(start, byteEnd - windowStart);
  return {
    before: decoder.decode(bytes.slice(0, start)),
    span: decoder.decode(bytes.slice(start, end)),
    after: decoder.decode(bytes.slice(end)),
  };
}

/**
 * Evidence for a selected fact: tier, extractor, file/span/commit, and a
 * read-only source view with the span highlighted (jump-to-source; NG1 —
 * navigation, never edit).
 */
export function EvidencePanel({ node, source, onClose }: EvidencePanelProps) {
  const prov = node.props.prov;
  const ev = prov?.evidence[0];

  return (
    <section className="card evidence-panel">
      <div className="evidence-head">
        <h2>Evidence — {node.id}</h2>
        <button type="button" onClick={onClose}>
          Close
        </button>
      </div>
      {!prov || !ev ? (
        <p className="muted">This node carries no evidence span.</p>
      ) : (
        <>
          <p className="evidence-meta">
            <TierBadge tier={prov.confidence_tier} />
            <span className="muted"> {prov.extractor_id} · </span>
            <code>
              {ev.repo}:{ev.path}
            </code>
            <span className="muted">
              {' '}
              bytes {ev.byte_start}–{ev.byte_end} @ {ev.commit_sha}
            </span>
          </p>
          {source === 'loading' ? (
            <p className="muted">Loading source…</p>
          ) : source === 'unavailable' ? (
            <p className="muted">Source unavailable (moved since ingest?) — span metadata above.</p>
          ) : (
            <pre className="evidence-source" data-testid="evidence-source">
              {(() => {
                const { before, span, after } = splitAtSpan(
                  source.text,
                  ev.byte_start,
                  ev.byte_end,
                  source.window_start,
                );
                return (
                  <>
                    {source.window_start > 0 ? '… ' : ''}
                    {before}
                    <mark>{span}</mark>
                    {after}
                    {source.truncated ? '\n… (windowed)' : ''}
                  </>
                );
              })()}
            </pre>
          )}
        </>
      )}
    </section>
  );
}
