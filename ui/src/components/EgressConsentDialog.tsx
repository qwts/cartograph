export interface EgressPayloadSpan {
  id: string;
  repo: string;
  path: string;
  byte_start: number;
  byte_end: number;
  commit_sha: string;
  text: string;
}

export interface EgressPreview {
  provider_id: string;
  locality: 'Local' | 'Cloud';
  tier: 'Semantic' | 'Agentic';
  action_id: string;
  payload: {
    system: string;
    prompt: string;
    spans: EgressPayloadSpan[];
  };
  payload_hash: string;
  redaction_count: number;
}

export interface EgressConsentDialogProps {
  preview: EgressPreview;
  busy?: boolean;
  onCancel: () => void;
  onConsent: (preview: EgressPreview) => void;
}

/**
 * Explicit, one-action cloud egress consent. The component renders every
 * redacted field and source span contained in the firewall preview; it never
 * reconstructs or truncates the payload the user is approving.
 */
export function EgressConsentDialog({
  preview,
  busy = false,
  onCancel,
  onConsent,
}: EgressConsentDialogProps) {
  const tierBadge =
    preview.tier === 'Agentic'
      ? { label: 'T3 · InferredWeak', className: 'tier-inferredweak' }
      : { label: 'T2 · InferredStrong', className: 'tier-inferredstrong' };

  return (
    <div className="egress-backdrop" role="presentation">
      <section
        className="egress-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="egress-title"
        aria-describedby="egress-description"
      >
        <header className="egress-heading">
          <div>
            <p className="egress-kicker">Cloud egress · one action only</p>
            <h2 id="egress-title">Review exact model payload</h2>
          </div>
          <span className={`tier-badge ${tierBadge.className}`}>{tierBadge.label}</span>
        </header>

        <p id="egress-description" className="muted">
          Cartograph will send only the redacted instructions and spans shown below to{' '}
          <strong>{preview.provider_id}</strong>. This approval cannot be reused if the payload changes.
        </p>

        <dl className="egress-meta">
          <div>
            <dt>Tier</dt>
            <dd>{preview.tier}</dd>
          </div>
          <div>
            <dt>Action</dt>
            <dd>{preview.action_id}</dd>
          </div>
          <div>
            <dt>Redactions</dt>
            <dd>{preview.redaction_count}</dd>
          </div>
          <div>
            <dt>Payload hash</dt>
            <dd className="egress-hash">{preview.payload_hash}</dd>
          </div>
        </dl>

        <div className="egress-section">
          <h3>System instructions</h3>
          <pre>{preview.payload.system}</pre>
        </div>
        <div className="egress-section">
          <h3>Task prompt</h3>
          <pre>{preview.payload.prompt}</pre>
        </div>
        <div className="egress-section">
          <h3>Evidence spans ({preview.payload.spans.length})</h3>
          <ol className="egress-spans">
            {preview.payload.spans.map((span) => (
              <li key={span.id}>
                <div className="egress-span-meta">
                  <strong>{span.id}</strong>
                  <span>
                    {span.repo}/{span.path}:{span.byte_start}-{span.byte_end} @{span.commit_sha}
                  </span>
                </div>
                <pre>{span.text}</pre>
              </li>
            ))}
          </ol>
        </div>

        <footer className="egress-actions">
          <button type="button" className="secondary-button" disabled={busy} onClick={onCancel}>
            Keep local
          </button>
          <button type="button" disabled={busy} onClick={() => onConsent(preview)}>
            {busy ? 'Sending…' : 'Allow this action once'}
          </button>
        </footer>
      </section>
    </div>
  );
}
