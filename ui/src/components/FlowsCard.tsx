import { useRef, useState } from 'react';
import type { Flow, FlowHop, Tier } from '../store';

export type FlowExportMode = 'verified-only' | 'best-effort';

export interface FlowsCardProps {
  /** Traced flows as data — status and score surface per R-INT-2. */
  flows: Flow[];
  /** Flow-dossier Markdown from the spec compiler, or null with no backend. */
  dossier: string | null;
  /** Open the evidence drawer for a hop's own provenance. */
  onSelectHop?: (hop: FlowHop) => void;
  /** Open the Resolution Strategy modal for a gap hop's Gap node. */
  onOpenResolution?: (gapId: string) => void;
}

const STATUS_CLASS: Record<Flow['status'], string> = {
  Verified: 'tier-confirmed',
  Inferred: 'tier-inferredstrong',
  Partial: 'tier-gap',
};

const CONFIDENCE_CLASS: Record<Tier, string> = {
  Confirmed: 'tier-confirmed',
  InferredStrong: 'tier-inferredstrong',
  InferredWeak: 'tier-inferredweak',
  Gap: 'tier-gap',
};

/** Backend facts with absent/future confidence values fail closed as Gap. */
function hopConfidence(hop: FlowHop): Tier {
  return hop.confidence === 'Confirmed' ||
    hop.confidence === 'InferredStrong' ||
    hop.confidence === 'InferredWeak' ||
    hop.confidence === 'Gap'
    ? hop.confidence
    : 'Gap';
}

function isGapHop(hop: FlowHop): boolean {
  return hopConfidence(hop) === 'Gap' || hop.gap_reason !== null;
}

/** R-INT-5 projection: verified-only excludes agentic/weak hops, while
 * preserving confirmed, strong, and explicit Gap facts exactly as recorded. */
export function projectFlow(flow: Flow, mode: FlowExportMode): Flow {
  if (mode === 'best-effort') return flow;
  return {
    ...flow,
    hops: flow.hops.filter((hop) => hopConfidence(hop) !== 'InferredWeak'),
  };
}

function tierLabel(tier: string): string {
  switch (tier) {
    case 'Deterministic':
      return 'T0';
    case 'Dynamic':
      return 'T1';
    case 'Semantic':
      return 'T2';
    case 'Agentic':
      return 'T3';
    default:
      return tier || 'Unknown tier';
  }
}

function gapReason(hop: FlowHop): string {
  if (hop.gap_reason) return hop.gap_reason;
  if (/^GAP:\s*/i.test(hop.dst_name)) return hop.dst_name.replace(/^GAP:\s*/i, '') || 'unresolved';
  if (hop.confidence !== 'Gap') {
    return `confidence metadata missing or unrecognized (${hop.confidence || 'empty'})`;
  }
  return 'unresolved';
}

function attemptedEscalation(hop: FlowHop): string {
  return hop.attempted_tiers.length > 0 ? hop.attempted_tiers.join(' → ') : 'not recorded';
}

/** Kind kicker for a hop card, from the destination's recorded identity —
 * never guessed from array position. */
export function hopKind(hop: FlowHop): string {
  if (isGapHop(hop)) return 'GAP';
  const prefix = hop.dst.split(':', 1)[0];
  switch (prefix) {
    case 'ep':
      return 'ENDPOINT';
    case 'sym':
      return 'FUNCTION';
    case 'chan':
    case 'channel':
      return 'CHANNEL';
    case 'res':
      return 'RESOURCE';
    case 'component':
      return 'COMPONENT';
    default:
      return hop.label.toUpperCase();
  }
}

/** Header status chip text: gap counts are named, never implied by color. */
export function statusBadge(flow: Flow): string {
  if (flow.status !== 'Partial') return flow.status.toUpperCase();
  const gaps = flow.hops.filter(isGapHop).length;
  return `PARTIAL (${gaps} gap${gaps === 1 ? '' : 's'})`;
}

function mermaidSafe(value: string): string {
  return value.replace(/[\r\n;]+/g, ' ').replaceAll('"', "'");
}

function tableSafe(value: string): string {
  return value.replaceAll('|', '\\|').replace(/[\r\n]+/g, ' ');
}

/** Deterministic client projection used only when the verified-only UI mode
 * must omit InferredWeak hops from the copyable dossier (AC-0031). */
export function projectedDossier(flows: Flow[], mode: FlowExportMode): string {
  const lines = [`# Flow dossier — ${mode}`, ''];
  for (const original of flows) {
    const flow = projectFlow(original, mode);
    lines.push(`## ${flow.trigger_name} — ${flow.status} (score ${flow.score.toFixed(2)})`, '');
    lines.push(`Trigger: ${flow.trigger_kind} \`${flow.trigger}\``, '', '```mermaid', 'sequenceDiagram');
    const participants: Array<{ id: string; name: string }> = [];
    const alias = (id: string, name: string): string => {
      const existing = participants.findIndex((participant) => participant.id === id);
      if (existing >= 0) return `p${existing}`;
      participants.push({ id, name });
      return `p${participants.length - 1}`;
    };
    const arrows = flow.hops.map((hop) => {
      const source = alias(hop.src, hop.src_name);
      const target = alias(hop.dst, hop.dst_name);
      const confidence = hopConfidence(hop);
      const arrow = confidence === 'Gap' ? '--x' : '->>';
      return `    ${source}${arrow}${target}: ${hop.label} [${confidence}]`;
    });
    participants.forEach((participant, index) => lines.push(`    participant p${index} as ${mermaidSafe(participant.name)}`));
    lines.push(...arrows);
    lines.push('```', '', '| # | Hop | Tier | Confidence | Evidence |', '|---|-----|------|------------|----------|');
    flow.hops.forEach((hop, index) => {
      lines.push(
        `| ${index + 1} | ${tableSafe(hop.label)} \`${tableSafe(hop.src)}\` → \`${tableSafe(hop.dst)}\` | ${tableSafe(hop.tier)} | ${hopConfidence(hop)} | ${tableSafe(hop.evidence ?? '—')} |`,
      );
    });
    if (flow.hops.length === 0) lines.push('| — | No hops in this projection | — | — | — |');
    lines.push('');
  }
  return lines.join('\n');
}

/** Card zoom bounds: real layout width, never a CSS transform, so the
 * wrapping row can shrink without leaving intrinsic scroll width behind. */
const ZOOM_MIN = 0.7;
const ZOOM_MAX = 1.6;
const ZOOM_STEP = 0.15;
/** Base hop-card width at zoom 1 (design: ~168px minimum). */
const CARD_BASE_PX = 168;
/** Arrow + gap allowance per card when fitting. */
const CARD_CHROME_PX = 34;

function clampZoom(value: number): number {
  return Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, value));
}

interface HopCardProps {
  hop: FlowHop;
  index: number;
  excluded: boolean;
  onSelectHop?: (hop: FlowHop) => void;
  onOpenResolution?: (gapId: string) => void;
}

function HopCard({ hop, index, excluded, onSelectHop, onOpenResolution }: HopCardProps) {
  const confidence = hopConfidence(hop);
  const gap = isGapHop(hop);
  const title = gap ? hop.dst_name.replace(/^GAP:\s*/i, '') || hop.label : hop.dst_name;
  const body = (
    <>
      <div className="flow-hop-head">
        <span className="flow-hop-kind">{hopKind(hop)}</span>
        <span className={`tier-badge ${CONFIDENCE_CLASS[confidence]}`}>
          {gap ? 'GAP' : tierLabel(hop.tier)}
        </span>
      </div>
      <strong className="flow-hop-title">{title}</strong>
      <span className="flow-hop-route">
        {hop.src_name} → {hop.dst_name}
      </span>
      {hop.evidence && <code className="flow-hop-evidence">{hop.evidence}</code>}
      {gap && (
        <p className="flow-hop-gap-note">
          Unresolved — {gapReason(hop)}. Attempted {attemptedEscalation(hop)}.
        </p>
      )}
      {excluded && (
        <p className="flow-hop-excluded-note">Excluded in verified-only — InferredWeak</p>
      )}
    </>
  );
  const className = `flow-hop-card${gap ? ' unresolved' : ''}${excluded ? ' excluded' : ''}`;
  return (
    <li className="flow-hop-step">
      {index > 0 && (
        <span className="flow-hop-arrow" aria-hidden="true">
          →
        </span>
      )}
      {excluded ? (
        // Excluded means excluded: the card stays visible so the projection
        // difference is explicit, but it is not an interactive fact here.
        <div className={className} aria-disabled="true">
          {body}
        </div>
      ) : (
        <button
          type="button"
          className={className}
          aria-label={`${gap ? 'Unresolved hop' : hop.label}: ${hop.src_name} to ${hop.dst_name}`}
          onClick={() => {
            // Only a real Gap node can escalate; a fail-closed hop (e.g.
            // unrecognized confidence) has no strategy — show its evidence.
            if (gap && hop.dst.startsWith('gap:')) onOpenResolution?.(hop.dst);
            else onSelectHop?.(hop);
          }}
        >
          {body}
        </button>
      )}
    </li>
  );
}

/** Flow Inspector surface (#107, handoff 03): header with id/status/score,
 * R-INT-5 projection toggle with an explicit hidden count, and a wrapping
 * hop-card sequence that never scrolls horizontally. Gap hops open the
 * Resolution Strategy; every other hop opens the evidence drawer. */
export function FlowsCard({ flows, dossier, onSelectHop, onOpenResolution }: FlowsCardProps) {
  const [selectedTrigger, setSelectedTrigger] = useState(flows[0]?.trigger ?? '');
  const [mode, setMode] = useState<FlowExportMode>('best-effort');
  const [zoom, setZoom] = useState(1);
  const [copied, setCopied] = useState(false);
  const rowRef = useRef<HTMLOListElement>(null);
  const selectedIndex = Math.max(
    0,
    flows.findIndex((flow) => flow.trigger === selectedTrigger),
  );
  const selected: Flow | undefined = flows[selectedIndex];
  const projected = selected ? projectFlow(selected, mode) : null;
  const dossierText =
    mode === 'best-effort' && dossier?.includes('## ') ? dossier : projectedDossier(flows, mode);

  const fit = () => {
    const row = rowRef.current;
    if (!row || !selected) return;
    const cards = selected.hops.length + 1; // + trigger card
    setZoom(clampZoom((row.clientWidth / cards - CARD_CHROME_PX) / CARD_BASE_PX));
  };

  const hiddenCount = selected
    ? selected.hops.length - (projected?.hops.length ?? 0)
    : 0;
  const confirmedCount =
    projected?.hops.filter((hop) => hopConfidence(hop) === 'Confirmed').length ?? 0;
  const weakCount = selected
    ? selected.hops.filter((hop) => hopConfidence(hop) === 'InferredWeak').length
    : 0;

  return (
    <section className="card flow-inspector-card" aria-labelledby="flow-inspector-title">
      <div className="flow-inspector-heading">
        <div>
          <h2 id="flow-inspector-title">
            {selected ? (
              <>
                F-{String(selectedIndex + 1).padStart(4, '0')} · {selected.trigger_name}{' '}
                <span className={`tier-badge ${STATUS_CLASS[selected.status]}`}>
                  {statusBadge(selected)}
                </span>
              </>
            ) : (
              'Flow Inspector'
            )}
          </h2>
          <p className="muted">
            {selected
              ? `Trigger: ${selected.trigger_kind} ${selected.trigger_name}` +
                (selected.hops.length > 0
                  ? ` → ${selected.hops.map((hop) => hop.dst_name).join(' → ')}`
                  : '') +
                `. Flow score ${selected.score.toFixed(2)}.`
              : 'Trace each business flow hop with its tier, evidence, and explicit unresolved boundary.'}
          </p>
        </div>
        <div className="flow-inspector-controls">
          <div className="flow-mode-toggle" aria-label="Flow export mode">
            {(['verified-only', 'best-effort'] as const).map((item) => (
              <button
                key={item}
                type="button"
                className={mode === item ? 'active' : ''}
                aria-pressed={mode === item}
                onClick={() => setMode(item)}
              >
                {item}
              </button>
            ))}
          </div>
          <div className="flow-zoom" aria-label="Hop card zoom">
            <button
              type="button"
              aria-label="Zoom out"
              onClick={() => setZoom((value) => clampZoom(value - ZOOM_STEP))}
            >
              −
            </button>
            <button type="button" aria-label="Fit hops to view" onClick={fit}>
              Fit
            </button>
            <button
              type="button"
              aria-label="Zoom in"
              onClick={() => setZoom((value) => clampZoom(value + ZOOM_STEP))}
            >
              +
            </button>
          </div>
        </div>
      </div>

      {flows.length === 0 || !selected || !projected ? (
        <p className="muted flow-inspector-empty">
          No flows traced yet — ingest a repo with endpoints or event channels.
        </p>
      ) : (
        <>
          <div className="flow-inspector-toolbar">
            <label htmlFor="flow-trigger">Trigger source</label>
            <select
              id="flow-trigger"
              value={selected.trigger}
              onChange={(event) => setSelectedTrigger(event.target.value)}
            >
              {flows.map((flow) => (
                <option key={flow.trigger} value={flow.trigger}>
                  {flow.trigger_name}
                </option>
              ))}
            </select>
            <span className="muted" role="status">
              {projected.hops.length} of {selected.hops.length} hops shown
            </span>
          </div>

          <p className={`flow-projection-note ${mode}`} role="note">
            {mode === 'verified-only'
              ? `Verified-only: InferredWeak hops are excluded (${hiddenCount} hidden), but the ` +
                `Gap node is retained (R-INT-4). Projected coverage ${confirmedCount}/${projected.hops.length} confirmed hops.`
              : weakCount > 0
                ? `Best-effort: includes ${weakCount} InferredWeak hop${weakCount === 1 ? '' : 's'}, ` +
                  'annotated — excluded from verified-only exports (R-INT-5).'
                : 'Best-effort: no InferredWeak hops in this flow — both projections are identical.'}
          </p>

          <ol
            className="flow-hop-row"
            aria-label="Flow sequence"
            ref={rowRef}
            style={{ ['--hop-scale' as string]: zoom }}
          >
            <li className="flow-hop-step">
              <div className="flow-hop-card trigger">
                <div className="flow-hop-head">
                  <span className="flow-hop-kind">TRIGGER · {selected.trigger_kind}</span>
                </div>
                <strong className="flow-hop-title">{selected.trigger_name}</strong>
                <code className="flow-hop-evidence">{selected.trigger}</code>
              </div>
            </li>
            {selected.hops.map((hop, index) => {
              const excluded = mode === 'verified-only' && hopConfidence(hop) === 'InferredWeak';
              return (
                <HopCard
                  key={`${hop.src}-${hop.label}-${hop.dst}-${index}`}
                  hop={hop}
                  index={index + 1}
                  excluded={excluded}
                  onSelectHop={onSelectHop}
                  onOpenResolution={onOpenResolution}
                />
              );
            })}
          </ol>

          <details className="flow-dossier-details">
            <summary>Mermaid + provenance dossier ({mode})</summary>
            <pre className="evidence-source flows-dossier" data-testid="flows-dossier">
              {dossierText}
            </pre>
          </details>
          <p className="flow-copy-action">
            <button
              type="button"
              onClick={() => {
                void Promise.resolve(navigator.clipboard?.writeText(dossierText)).then(() => {
                  setCopied(true);
                  setTimeout(() => setCopied(false), 1500);
                });
              }}
            >
              {copied ? 'Copied' : `Copy ${mode} dossier`}
            </button>
          </p>
        </>
      )}
    </section>
  );
}
