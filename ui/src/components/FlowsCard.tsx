import { useState } from 'react';

export interface FlowsCardProps {
  /** Flow-dossier Markdown from the spec compiler, or null with no backend. */
  dossier: string | null;
}

/** True when the dossier has actual flows (not just the header line). */
function hasContent(dossier: string | null): dossier is string {
  return dossier !== null && dossier.includes('## ');
}

/**
 * Flow-dossier artifact (SPEC-00 §7, M3). Every T0-traceable flow with its
 * status, score, Mermaid sequence, and per-hop provenance table — rendered
 * read-only with a copy action. The interactive Flow Inspector arrives at M9.
 */
export function FlowsCard({ dossier }: FlowsCardProps) {
  const [copied, setCopied] = useState(false);

  return (
    <section className="card">
      <h2>Flows</h2>
      {!hasContent(dossier) ? (
        <p className="muted">
          No flows traced yet — ingest a repo with endpoints or event channels.
        </p>
      ) : (
        <>
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
