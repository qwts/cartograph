import type { IngestSource, PatternFinding, PreflightReport } from '../store';

export interface PreflightSurfaceProps {
  source: IngestSource;
  target: string;
  /** Local detection output; null while busy or for remote targets. */
  report: PreflightReport | null;
  busy: boolean;
  error: string | null;
  /** Disabled when there is no live backend to recover with. */
  canRecover: boolean;
  onBack: () => void;
  onRunRecovery: () => void;
}

function FindingList({ items, marker }: { items: PatternFinding[]; marker: string }) {
  return (
    <ul className="preflight-items">
      {items.map((finding) => (
        <li key={`${finding.path}:${finding.line}:${finding.kind}`}>
          <span aria-hidden="true">{marker}</span> {finding.message}{' '}
          <code>
            {finding.path}:{finding.line}
          </code>
        </li>
      ))}
    </ul>
  );
}

/** Step 2 of the ingest flow (handoff §Preflight): local detection with the
 *  three-way classification from first contact. Potential system gaps and
 *  unsupported patterns are separate cards with separate meanings — an
 *  unsupported item is a tool limitation and never "becomes a Gap". */
export function PreflightSurface({
  source,
  target,
  report,
  busy,
  error,
  canRecover,
  onBack,
  onRunRecovery,
}: PreflightSurfaceProps) {
  return (
    <section className="ingest-flow" aria-label="Preflight checks">
      <header className="ingest-hero">
        <h2>Preflight checks</h2>
        <p className="muted">
          Detect languages, frameworks, and adapters. Runs locally —{' '}
          <strong className="local-safe">0 bytes egress</strong>.
        </p>
        <p className="preflight-target">
          <code>{target}</code>
        </p>
      </header>

      {busy && (
        <p className="muted" role="status">
          <span className="material-symbols-outlined spinning" aria-hidden="true">
            progress_activity
          </span>{' '}
          Detecting…
        </p>
      )}
      {error && <p className="error-text">{error}</p>}

      {!busy && !error && report === null && (
        <div className="preflight-card">
          <h3>
            <span className="material-symbols-outlined" aria-hidden="true">
              cloud_off
            </span>
            Detection deferred
          </h3>
          <p className="muted">
            {source === 'github'
              ? 'A remote repository is detected against its local clone at the start of recovery — before any parsing, still fully on-device.'
              : 'A system manifest is detected per declared repo at the start of recovery — before any parsing, still fully on-device.'}
          </p>
        </div>
      )}

      {report && (
        <div className="preflight-cards">
          <div className="preflight-card">
            <h3>
              <span className="material-symbols-outlined ok" aria-hidden="true">
                terminal
              </span>
              Languages
            </h3>
            {report.languages.length === 0 ? (
              <p className="muted">No source files detected.</p>
            ) : (
              <ul className="preflight-items">
                {report.languages.map((lang) => (
                  <li key={lang.language}>
                    <span aria-hidden="true">{lang.adapter ? '✓' : '~'}</span> {lang.language} —{' '}
                    {lang.adapter ? (
                      <code>{lang.adapter}</code>
                    ) : (
                      <em>no adapter — surfaces under Unsupported patterns</em>
                    )}{' '}
                    <span className="muted">({lang.files} files)</span>
                  </li>
                ))}
              </ul>
            )}
          </div>

          <div className="preflight-card">
            <h3>
              <span className="material-symbols-outlined ok" aria-hidden="true">
                deployed_code
              </span>
              Frameworks &amp; adapters
            </h3>
            {report.frameworks.length === 0 ? (
              <p className="muted">No framework markers detected.</p>
            ) : (
              <ul className="preflight-items">
                {report.frameworks.map((framework) => (
                  <li key={framework}>
                    <span aria-hidden="true">✓</span> {framework}
                  </li>
                ))}
              </ul>
            )}
          </div>

          <div className="preflight-card gap">
            <h3>
              <span className="material-symbols-outlined" aria-hidden="true">
                link_off
              </span>
              Potential system gaps
            </h3>
            {report.potential_gaps.length === 0 ? (
              <p className="muted">None detected.</p>
            ) : (
              <>
                <FindingList items={report.potential_gaps} marker="~" />
                <p className="preflight-note gap-note">
                  Evidence exists but is not statically resolvable. These are predicted System
                  Gaps — each gets an explicit Gap with a Resolution Strategy after recovery,
                  never a guess.
                </p>
              </>
            )}
          </div>

          <div className="preflight-card unsupported">
            <h3>
              <span className="material-symbols-outlined" aria-hidden="true">
                block
              </span>
              Unsupported patterns
            </h3>
            {report.unsupported.length === 0 ? (
              <p className="muted">None detected.</p>
            ) : (
              <>
                <FindingList items={report.unsupported} marker="~" />
                <p className="preflight-note unsupported-note">
                  No adapter covers these constructs — a tool limitation, not a System Gap.
                  Recovery proceeds around them and lists them explicitly.
                </p>
              </>
            )}
          </div>
        </div>
      )}

      <footer className="flow-actions">
        <button type="button" className="secondary-button" onClick={onBack}>
          Back
        </button>
        <span className="flow-actions-right">
          <button
            type="button"
            className="secondary-button"
            disabled
            title="Tier-selective recovery lands with escalation orchestration (#120)"
          >
            Structure only
          </button>
          <button type="button" onClick={onRunRecovery} disabled={!canRecover || busy}>
            <span className="material-symbols-outlined" aria-hidden="true">
              play_arrow
            </span>
            Run full recovery
          </button>
        </span>
      </footer>
    </section>
  );
}
