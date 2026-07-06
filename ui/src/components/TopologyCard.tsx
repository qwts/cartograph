import { useState } from 'react';

export interface TopologyCardProps {
  /** Mermaid text from the spec compiler, or null with no backend. */
  mermaid: string | null;
}

/** True when the artifact has actual resources (not just the header line). */
function hasContent(mermaid: string | null): mermaid is string {
  return mermaid !== null && mermaid.trim() !== 'flowchart LR';
}

/**
 * Resource/topology map artifact (SPEC-00 §7, M2). Renders the Mermaid text
 * read-only with a copy action — paste into any Mermaid renderer, a PR, or
 * the docs. In-app rendering arrives with the Atlas (M9).
 */
export function TopologyCard({ mermaid }: TopologyCardProps) {
  const [copied, setCopied] = useState(false);

  return (
    <section className="card">
      <h2>Topology map</h2>
      {!hasContent(mermaid) ? (
        <p className="muted">No resources recovered yet — ingest a repo with Terraform.</p>
      ) : (
        <>
          <pre className="evidence-source topology-mermaid" data-testid="topology-mermaid">
            {mermaid}
          </pre>
          <p style={{ marginTop: '0.75rem' }}>
            <button
              type="button"
              onClick={() => {
                void navigator.clipboard?.writeText(mermaid).then(() => {
                  setCopied(true);
                  setTimeout(() => setCopied(false), 1500);
                });
              }}
            >
              {copied ? 'Copied' : 'Copy Mermaid'}
            </button>
          </p>
        </>
      )}
    </section>
  );
}
