import { useState } from 'react';
import type { Flow } from '../store';

export interface FlowsCardProps {
  /** Traced flows as data — status and score surface per R-INT-2. */
  flows: Flow[];
  /** Flow-dossier Markdown from the spec compiler, or null with no backend. */
  dossier: string | null;
}

/** Status chip color classes reuse the tier palette (R-INT-2): a Verified
 *  flow is confirmed-green, an Inferred one inferred-blue, a Partial one
 *  gap-red — they must never look alike. */
const STATUS_CLASS: Record<Flow['status'], string> = {
  Verified: 'tier-confirmed',
  Inferred: 'tier-inferredstrong',
  Partial: 'tier-gap',
};

/** True when the artifact has actual flows (not just the header line). */
function hasContent(dossier: string | null): dossier is string {
  return dossier !== null && dossier.includes('## ');
}

/**
 * Flow-dossier artifact (SPEC-00 §7, M3). One row per traced flow with its
 * status and score, then the full dossier read-only with a copy action.
 * The interactive Flow Inspector arrives at M9.
 */
export function FlowsCard({ flows, dossier }: FlowsCardProps) {
  const [copied, setCopied] = useState(false);

  return (
    <section className="card">
      <h2>Flows</h2>
      {flows.length === 0 || !hasContent(dossier) ? (
        <p className="muted">
          No flows traced yet — ingest a repo with endpoints or event channels.
        </p>
      ) : (
        <>
          <ul className="flow-list">
            {flows.map((flow) => (
              <li key={flow.trigger} className="flow-row">
                <span className="flow-trigger">{flow.trigger_name}</span>
                <span className="flow-score" title="mean hop weight (SPEC-00 §5.3)">
                  {flow.score.toFixed(2)}
                </span>
                <span className={`tier-badge ${STATUS_CLASS[flow.status]}`}>{flow.status}</span>
              </li>
            ))}
          </ul>
          <pre className="evidence-source flows-dossier" data-testid="flows-dossier">
            {dossier}
          </pre>
          <p style={{ marginTop: '0.75rem' }}>
            <button
              type="button"
              onClick={() => {
                void navigator.clipboard?.writeText(dossier).then(() => {
                  setCopied(true);
                  setTimeout(() => setCopied(false), 1500);
                });
              }}
            >
              {copied ? 'Copied' : 'Copy dossier'}
            </button>
          </p>
        </>
      )}
    </section>
  );
}
