import { useCallback, useEffect, useRef, useState } from 'react';
import type { GraphNode, SourceState, Tier } from '../store';
import { TierBadge } from './TierBadge';

export interface EvidencePanelProps {
  node: GraphNode;
  source: SourceState;
  onClose: () => void;
  /** Load a different supporting evidence span for the same fact. */
  onShowEvidence?: (index: number) => void;
  /** Open the Resolution Strategy modal for a Gap; absent until the
   *  escalation runner exists (#120), where the CTA renders disabled. */
  onOpenResolution?: (node: GraphNode) => void;
  /** Which evidence ref is currently shown (default first). */
  evidenceIndex?: number;
}

export const DRAWER_MIN = 320;
export const DRAWER_MAX = 560;

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

/** 1-based line:col of a position given the text before it and the true
 *  file line number of the window's first line. */
function lineCol(before: string, windowStartLine: number): { line: number; col: number } {
  const lines = before.split('\n');
  return {
    line: windowStartLine + lines.length - 1,
    col: (lines[lines.length - 1]?.length ?? 0) + 1,
  };
}

const WHY_TIER: Record<Tier, string> = {
  Confirmed:
    'Established deterministically (T0/T1): parsed structure or observed execution, with an exact source span. No inference involved.',
  InferredStrong:
    'Proposed by the semantic tier (T2) above the calibrated precision floor, with cited evidence. It annotates the graph — it never overwrites or masquerades as T0/T1 (R-INT-1).',
  InferredWeak:
    'Proposed by the agentic tier (T3) as a reviewed suggestion with cited evidence. Weakest confidence; excluded from verified-only exports (R-INT-5).',
  Gap: 'Deterministic recovery attempted this and stopped: the evidence exists but is not statically resolvable. Recorded explicitly with a Resolution Strategy — never guessed (R-INT-4).',
};

/**
 * Evidence drawer (handoff §Evidence drawer): read-only, resizable
 * (320↔560px), full provenance incl. the complete content hash, true
 * file line numbers, and the whole span range. Never edits source (NG1).
 */
export function EvidencePanel({
  node,
  source,
  onClose,
  onShowEvidence,
  onOpenResolution,
  evidenceIndex = 0,
}: EvidencePanelProps) {
  const prov = node.props.prov;
  const ev = prov?.evidence[evidenceIndex] ?? prov?.evidence[0];
  const [width, setWidth] = useState(420);
  const [copied, setCopied] = useState(false);
  const dragging = useRef(false);

  // Esc closes from anywhere — the drawer is an overlay, not a route.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

  const onDragStart = useCallback((event: React.PointerEvent) => {
    dragging.current = true;
    event.currentTarget.setPointerCapture(event.pointerId);
  }, []);
  const onDragMove = useCallback((event: React.PointerEvent) => {
    if (!dragging.current) return;
    setWidth(Math.min(DRAWER_MAX, Math.max(DRAWER_MIN, window.innerWidth - event.clientX)));
  }, []);
  const onDragEnd = useCallback(() => {
    dragging.current = false;
  }, []);

  const name =
    typeof node.props.name === 'string' ? node.props.name : (node.props.path ?? node.id);

  return (
    <section
      className="evidence-panel"
      style={{ width }}
      data-testid="evidence-drawer"
      aria-label="Evidence, read-only"
    >
      <div
        className="evidence-resize"
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize evidence drawer"
        onPointerDown={onDragStart}
        onPointerMove={onDragMove}
        onPointerUp={onDragEnd}
      />
      <div className="evidence-head">
        <div>
          <p className="evidence-kicker">Evidence · read-only</p>
          <h2>{String(name)}</h2>
        </div>
        {prov && <TierBadge tier={prov.confidence_tier} />}
        <button type="button" aria-label="Close evidence" onClick={onClose}>
          ✕
        </button>
      </div>

      {!prov || !ev ? (
        <p className="muted">This fact carries no evidence span.</p>
      ) : (
        <>
          {prov.confidence_tier !== 'Confirmed' && (
            <p
              className={`evidence-why ${prov.confidence_tier === 'Gap' ? 'gap' : 'inferred'}`}
            >
              {prov.confidence_tier === 'Gap'
                ? 'Why this is a Gap: evidence exists but is not statically resolvable — listed explicitly, never guessed.'
                : 'Why this is Inferred: proposed by a higher tier with cited evidence; it never overwrites T0/T1.'}
            </p>
          )}

          <details className="evidence-why-tier">
            <summary>Why this tier?</summary>
            <p className="muted">{WHY_TIER[prov.confidence_tier]}</p>
          </details>

          {source === 'loading' ? (
            <p className="muted">Loading source…</p>
          ) : source === 'unavailable' ? (
            <p className="muted">Source unavailable (moved since ingest?) — span metadata below.</p>
          ) : (
            (() => {
              const { before, span, after } = splitAtSpan(
                source.text,
                ev.byte_start,
                ev.byte_end,
                source.window_start,
              );
              const startLine = source.window_start_line ?? 1;
              const from = lineCol(before, startLine);
              const to = lineCol(before + span, startLine);
              const totalLines = source.text.split('\n').length;
              return (
                <>
                  <p className="evidence-span-range">
                    <code data-testid="span-range">
                      bytes {ev.byte_start}–{ev.byte_end} · L{from.line}:{from.col} – L{to.line}:
                      {to.col}
                    </code>
                  </p>
                  <div className="evidence-code" data-testid="evidence-code">
                    <pre className="evidence-gutter" aria-hidden="true">
                      {Array.from({ length: totalLines }, (_, i) => startLine + i).join('\n')}
                    </pre>
                    <pre className="evidence-source" data-testid="evidence-source">
                      {before}
                      <mark>{span}</mark>
                      {after}
                      {source.truncated ? '\n… (windowed)' : ''}
                    </pre>
                  </div>
                </>
              );
            })()
          )}

          <h3 className="settings-section-title">Provenance</h3>
          <dl className="provenance-table">
            <div>
              <dt>Tier</dt>
              <dd>{prov.tier}</dd>
            </div>
            <div>
              <dt>Confidence</dt>
              <dd>{prov.confidence_tier}</dd>
            </div>
            <div>
              <dt>Extractor</dt>
              <dd>
                <code>{prov.extractor_id}</code>
              </dd>
            </div>
            <div>
              <dt>File</dt>
              <dd>
                <code>
                  {ev.repo}:{ev.path}
                </code>
              </dd>
            </div>
            <div>
              <dt>Span</dt>
              <dd>
                <code>
                  bytes {ev.byte_start}–{ev.byte_end}
                </code>
              </dd>
            </div>
            <div>
              <dt>Commit</dt>
              <dd>
                <code>{ev.commit_sha}</code>
              </dd>
            </div>
            <div>
              <dt>content_hash</dt>
              <dd className="evidence-hash-cell">
                <code data-testid="content-hash">{prov.content_hash}</code>
                <button
                  type="button"
                  className="secondary-button copy-hash"
                  data-hash={prov.content_hash}
                  onClick={() => {
                    void navigator.clipboard?.writeText(prov.content_hash);
                    setCopied(true);
                  }}
                >
                  {copied ? 'Copied' : 'Copy'}
                </button>
              </dd>
            </div>
          </dl>

          {prov.evidence.length > 1 && (
            <>
              <h3 className="settings-section-title">Supporting evidence</h3>
              <ul className="supporting-evidence">
                {prov.evidence.map((reference, index) => (
                  <li key={`${reference.path}:${reference.byte_start}`}>
                    <button
                      type="button"
                      className={`evidence-ref${index === evidenceIndex ? ' active' : ''}`}
                      onClick={() => onShowEvidence?.(index)}
                    >
                      <code>
                        {reference.repo}:{reference.path} · bytes {reference.byte_start}–
                        {reference.byte_end}
                      </code>
                    </button>
                  </li>
                ))}
              </ul>
            </>
          )}

          {prov.confidence_tier === 'Gap' && (
            <button
              type="button"
              className="resolution-cta"
              disabled={!onOpenResolution}
              title={
                onOpenResolution
                  ? undefined
                  : 'No runnable strategy for this fact — escalation needs a Gap node'
              }
              onClick={() => onOpenResolution?.(node)}
            >
              Open Resolution Strategy
            </button>
          )}
        </>
      )}
      <footer className="evidence-footer muted">
        Source navigation is read-only. T2/T3 never overwrite or masquerade as T0/T1.
      </footer>
    </section>
  );
}
