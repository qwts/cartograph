import { useMemo, useState } from 'react';
import type {
  AssertionDecision,
  AssertionDecisionRecord,
  SpecArtifact,
  SpecAssertion,
  SpecBundle,
  SpecExportMode,
  Tier,
} from '../store';
import { TierBadge } from './TierBadge';

export interface SpecWorkbenchProps {
  bundle: SpecBundle | null;
  mode: SpecExportMode;
  decisions: AssertionDecisionRecord[];
  busy: boolean;
  error: string | null;
  canCurate: boolean;
  onModeChange: (mode: SpecExportMode) => void;
  onCurate: (assertion: SpecAssertion, decision: AssertionDecision, note?: string) => void;
  onCopyArtifact: (artifact: SpecArtifact) => void;
  onExportBundle: (bundle: SpecBundle) => void;
}

function isInferred(tier: Tier): boolean {
  return tier === 'InferredStrong' || tier === 'InferredWeak';
}

function decisionLabel(decision: AssertionDecision): string {
  switch (decision) {
    case 'accepted':
      return 'Accepted';
    case 'rejected':
      return 'Rejected';
    case 'annotated':
      return 'Annotated';
  }
}

/** Official spec artifacts, provenance, curation, and export controls (US-0012). */
export function SpecWorkbench({
  bundle,
  mode,
  decisions,
  busy,
  error,
  canCurate,
  onModeChange,
  onCurate,
  onCopyArtifact,
  onExportBundle,
}: SpecWorkbenchProps) {
  const [selectedId, setSelectedId] = useState('');
  const [notes, setNotes] = useState<Record<string, string>>({});
  const decisionByHash = useMemo(
    () => new Map(decisions.map((record) => [record.assertion.provenance.content_hash, record])),
    [decisions],
  );
  const selected =
    bundle?.artifacts.find((artifact) => artifact.id === selectedId) ?? bundle?.artifacts[0] ?? null;

  return (
    <section className="card spec-workbench-card" aria-labelledby="spec-workbench-title">
      <header className="spec-workbench-header">
        <div>
          <h2 id="spec-workbench-title">Spec Workbench</h2>
          <p className="muted">
            Review official artifacts, curate inferred assertions, and export without obscuring provenance.
          </p>
        </div>
        <div className="spec-workbench-controls">
          <div className="flow-mode-toggle" aria-label="Official spec export mode">
            {(['verified-only', 'best-effort'] as const).map((item) => (
              <button
                key={item}
                type="button"
                className={mode === item ? 'active' : ''}
                aria-pressed={mode === item}
                disabled={busy}
                onClick={() => onModeChange(item)}
              >
                {item}
              </button>
            ))}
          </div>
          <button
            type="button"
            disabled={!bundle || busy}
            onClick={() => bundle && onExportBundle(bundle)}
          >
            Export bundle
          </button>
        </div>
      </header>

      {error && <p className="error spec-workbench-error">{error}</p>}
      {!bundle ? (
        <p className="muted spec-workbench-empty">
          No compiled spec is available — connect to the core and ingest a repository set.
        </p>
      ) : (
        <>
          <div className="spec-workbench-summary" role="status">
            <span>{bundle.artifacts.length} artifacts</span>
            <span>{bundle.assertion_count} visible assertions</span>
            <span className="tier-gap">{bundle.gap_count} gaps</span>
            <span>{bundle.drift_count} drift findings</span>
            {busy && <span>Refreshing…</span>}
          </div>

          <div className="spec-workbench-layout">
            <nav className="spec-artifact-nav" aria-label="Official spec artifacts">
              {bundle.artifacts.map((artifact) => (
                <button
                  key={artifact.id}
                  type="button"
                  className={selected?.id === artifact.id ? 'active' : ''}
                  aria-current={selected?.id === artifact.id ? 'page' : undefined}
                  onClick={() => setSelectedId(artifact.id)}
                >
                  <span>{artifact.title}</span>
                  <small>{artifact.file_name}</small>
                  <strong>{artifact.assertions.length}</strong>
                </button>
              ))}
              <div className="spec-curation-log">
                <h3>Curation log</h3>
                {decisions.length === 0 ? (
                  <p className="muted">No decisions recorded.</p>
                ) : (
                  <ul>
                    {decisions.map((record) => (
                      <li key={record.assertion.provenance.content_hash}>
                        <strong>{decisionLabel(record.decision)}</strong>
                        <span>{record.assertion.summary}</span>
                        {record.note && <small>{record.note}</small>}
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            </nav>

            {selected && (
              <article className="spec-artifact-detail" aria-labelledby="selected-artifact-title">
                <header>
                  <div>
                    <p className="eyebrow">{selected.format} artifact</p>
                    <h3 id="selected-artifact-title">{selected.title}</h3>
                    <code>{selected.file_name}</code>
                  </div>
                  <button type="button" onClick={() => onCopyArtifact(selected)}>
                    Copy artifact
                  </button>
                </header>

                <pre className="spec-artifact-source" data-testid="spec-artifact-source">
                  {selected.content}
                </pre>

                <section className="spec-assertions" aria-labelledby="spec-assertions-title">
                  <header>
                    <h3 id="spec-assertions-title">Assertions and inline provenance</h3>
                    <span className="muted">{selected.assertions.length} in this artifact</span>
                  </header>
                  {selected.assertions.length === 0 ? (
                    <p className="muted">No graph-backed assertions were recovered for this artifact.</p>
                  ) : (
                    <ol>
                      {selected.assertions.map((assertion) => {
                        const provenance = assertion.provenance;
                        const record = decisionByHash.get(provenance.content_hash);
                        const curatable = isInferred(provenance.confidence_tier);
                        const note = notes[provenance.content_hash] ?? record?.note ?? '';
                        return (
                          <li key={assertion.id} className={`spec-assertion tier-border-${provenance.confidence_tier.toLowerCase()}`}>
                            <div className="spec-assertion-heading">
                              <div>
                                <p className="eyebrow">{assertion.subject_kind}</p>
                                <strong>{assertion.summary}</strong>
                              </div>
                              <TierBadge tier={provenance.confidence_tier} />
                            </div>
                            <dl className="spec-provenance">
                              <div><dt>Producer</dt><dd>{provenance.tier}</dd></div>
                              <div><dt>Extractor</dt><dd><code>{provenance.extractor_id}</code></dd></div>
                              <div className="wide"><dt>Content hash</dt><dd><code>{provenance.content_hash}</code></dd></div>
                              <div className="wide">
                                <dt>Evidence</dt>
                                <dd>
                                  {provenance.evidence.length === 0 ? (
                                    <span>None recorded — treated as unresolved.</span>
                                  ) : (
                                    <ul>
                                      {provenance.evidence.map((evidence) => (
                                        <li key={`${evidence.repo}:${evidence.path}:${evidence.byte_start}`}>
                                          <code>{evidence.repo}/{evidence.path}</code>
                                          <span>bytes {evidence.byte_start}..{evidence.byte_end}</span>
                                          <span>@ {evidence.commit_sha}</span>
                                        </li>
                                      ))}
                                    </ul>
                                  )}
                                </dd>
                              </div>
                            </dl>

                            {curatable && (
                              <div className="spec-curation-controls">
                                <label>
                                  Annotation
                                  <textarea
                                    value={note}
                                    placeholder="Add evidence-backed context…"
                                    disabled={!canCurate || busy}
                                    onChange={(event) => setNotes((current) => ({
                                      ...current,
                                      [provenance.content_hash]: event.target.value,
                                    }))}
                                  />
                                </label>
                                <div>
                                  <button
                                    type="button"
                                    disabled={!canCurate || busy || provenance.evidence.length === 0}
                                    onClick={() => onCurate(assertion, 'accepted', note || undefined)}
                                  >
                                    Accept
                                  </button>
                                  <button
                                    type="button"
                                    className="danger"
                                    disabled={!canCurate || busy || provenance.evidence.length === 0}
                                    onClick={() => onCurate(assertion, 'rejected', note || undefined)}
                                  >
                                    Reject
                                  </button>
                                  <button
                                    type="button"
                                    disabled={!canCurate || busy || provenance.evidence.length === 0 || !note.trim()}
                                    onClick={() => onCurate(assertion, 'annotated', note)}
                                  >
                                    Annotate
                                  </button>
                                  {record && (
                                    <span className={`curation-decision decision-${record.decision}`}>
                                      {decisionLabel(record.decision)}
                                    </span>
                                  )}
                                </div>
                              </div>
                            )}
                          </li>
                        );
                      })}
                    </ol>
                  )}
                </section>
              </article>
            )}
          </div>
        </>
      )}
    </section>
  );
}
