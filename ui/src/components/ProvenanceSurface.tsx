import type {
  EvalResult,
  ExtractorCoverage,
  FindingsSummary,
  IngestRecord,
  TierDistribution,
} from '../store';

export interface ProvenanceSurfaceProps {
  /** Register headline — the same summary Workspace and Gaps quote. */
  findings: FindingsSummary | null;
  /** Tier counts from the shared atlas selector. */
  distribution: TierDistribution;
  /** Per-extractor coverage for the latest ingest (#119). */
  coverage: ExtractorCoverage[];
  /** Paired-eval calibration records, newest first. */
  evals: EvalResult[];
  /** Ingest history, newest first (#119). */
  history: IngestRecord[];
}

const TIER_SEGMENTS = [
  { key: 'confirmed', label: 'Confirmed', className: 'seg-confirmed' },
  { key: 'inferredStrong', label: 'Inferred Strong', className: 'seg-inferred-strong' },
  { key: 'inferredWeak', label: 'Inferred Weak', className: 'seg-inferred-weak' },
  { key: 'gap', label: 'Gap', className: 'seg-gap' },
] as const;

function pct(part: number, total: number): number {
  return total === 0 ? 0 : (part / total) * 100;
}

/** Provenance & Eval (handoff screenshot 06): tier distribution, extractor
 *  coverage, paired-eval gates, and evidence health over re-ingests. Every
 *  count reads the shared selectors/commands — no chart invents numbers,
 *  and none of them encodes by color alone (each carries text + aria). */
export function ProvenanceSurface({
  findings,
  distribution,
  coverage,
  evals,
  history,
}: ProvenanceSurfaceProps) {
  const facts = distribution.total;
  const unsupported = findings?.unsupported ?? 0;
  // Oldest → newest so the history chart reads left to right in time.
  const timeline = [...history].slice(0, 8).reverse();
  const deterministic =
    timeline.length >= 2 &&
    timeline.some((record, index) =>
      timeline
        .slice(index + 1)
        .some(
          (later) =>
            later.commit_sha === record.commit_sha &&
            later.repo === record.repo &&
            later.content_hash === record.content_hash,
        ),
    );

  return (
    <section className="prov-surface" aria-label="Provenance and evaluation">
      <header className="ingest-hero">
        <h2>Provenance &amp; Evaluation</h2>
        <p className="muted">
          Tier distribution, extractor coverage, paired-eval quality gates, and evidence health
          over time.
        </p>
      </header>

      <h3 className="settings-section-title" role="heading" aria-level={3}>
        Tier distribution · {facts} graph facts · {unsupported} unsupported patterns
      </h3>
      <div
        className="tier-bar"
        role="img"
        aria-label={`Tier distribution of ${facts} graph facts: ${distribution.confirmed} Confirmed, ${distribution.inferredStrong} Inferred Strong, ${distribution.inferredWeak} Inferred Weak, ${distribution.gap} Gap; plus ${unsupported} unsupported patterns in the register`}
      >
        {TIER_SEGMENTS.map((segment) => (
          <div
            key={segment.key}
            className={`tier-bar-seg ${segment.className}`}
            style={{ width: `${pct(distribution[segment.key], facts)}%` }}
          />
        ))}
      </div>
      <ul className="tier-legend">
        {TIER_SEGMENTS.map((segment) => (
          <li key={segment.key}>
            <span className={`legend-dot ${segment.className}`} aria-hidden="true" />
            {segment.label} <strong>{distribution[segment.key]}</strong>
          </li>
        ))}
        <li>
          <span className="legend-dot seg-unsupported" aria-hidden="true" />
          Unsupported <strong>{unsupported}</strong>
          <span className="muted"> (register finding, not a graph fact)</span>
        </li>
      </ul>

      <div className="prov-columns">
        <div>
          <h3 className="settings-section-title" role="heading" aria-level={3}>
            Extractor coverage
          </h3>
          {coverage.length === 0 ? (
            <p className="muted">No ingest recorded yet — coverage lands with the first run.</p>
          ) : (
            <ul className="coverage-rows">
              {coverage.map((row) => (
                <li key={row.extractor} className="coverage-row">
                  <code className="coverage-name">{row.extractor}</code>
                  <div
                    className="coverage-bar"
                    role="img"
                    aria-label={
                      row.coverage_pct === null
                        ? `${row.extractor}: ${row.facts} facts, scope not applicable this ingest`
                        : `${row.extractor}: ${row.facts} facts from ${row.files_with_facts} of ${row.files_in_scope} files (${Math.round(row.coverage_pct)}% coverage)`
                    }
                  >
                    {row.coverage_pct !== null && (
                      <div
                        className="coverage-bar-fill"
                        style={{ width: `${Math.min(row.coverage_pct, 100)}%` }}
                      />
                    )}
                  </div>
                  <span className="coverage-tail">
                    {row.coverage_pct === null ? 'n/a' : `${Math.round(row.coverage_pct)}%`} ·{' '}
                    {row.facts} facts
                  </span>
                </li>
              ))}
            </ul>
          )}
        </div>

        <div>
          <h3 className="settings-section-title" role="heading" aria-level={3}>
            Paired-eval quality gate (T2/T3)
          </h3>
          {evals.length === 0 ? (
            <p className="muted">
              No calibration recorded — the gate runs when a semantic overlay is staged.
            </p>
          ) : (
            <ul className="eval-cards">
              {evals.slice(0, 4).map((result) => (
                <li
                  key={result.id}
                  className={`eval-card ${result.passed ? 'passed' : 'failed'}`}
                >
                  <span className="eval-provider">
                    <code>{result.provider}</code>
                    <span className={`eval-badge ${result.passed ? 'passed' : 'failed'}`}>
                      {result.passed ? 'GATE PASS' : 'BELOW FLOOR'}
                    </span>
                  </span>
                  <code className="eval-numbers">
                    P {result.precision.toFixed(2)} · R {result.recall.toFixed(2)} · floor{' '}
                    {result.precision_floor.toFixed(2)}
                  </code>
                  <span className="muted">
                    {result.approved} of {result.proposals} proposals admitted
                  </span>
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>

      <h3 className="settings-section-title" role="heading" aria-level={3}>
        Evidence health over re-ingests
      </h3>
      {timeline.length === 0 ? (
        <p className="muted">No ingest history yet.</p>
      ) : (
        <>
          <div className="history-chart">
            {timeline.map((record) => {
              const max = Math.max(...timeline.map((entry) => entry.graph_facts), 1);
              return (
                <div
                  key={record.id}
                  className="history-col"
                  role="img"
                  aria-label={`Ingest ${record.id} of ${record.repo} at ${record.commit_sha}: ${record.graph_facts} facts — ${record.confirmed} Confirmed, ${record.inferred_strong} Inferred Strong, ${record.inferred_weak} Inferred Weak, ${record.gap} Gap; hash ${record.content_hash.slice(0, 8)}`}
                >
                  <div
                    className="history-stack"
                    style={{ height: `${pct(record.graph_facts, max)}%` }}
                  >
                    <div
                      className="tier-bar-seg seg-gap"
                      style={{ height: `${pct(record.gap, record.graph_facts)}%` }}
                    />
                    <div
                      className="tier-bar-seg seg-inferred-weak"
                      style={{ height: `${pct(record.inferred_weak, record.graph_facts)}%` }}
                    />
                    <div
                      className="tier-bar-seg seg-inferred-strong"
                      style={{ height: `${pct(record.inferred_strong, record.graph_facts)}%` }}
                    />
                    <div
                      className="tier-bar-seg seg-confirmed"
                      style={{ height: `${pct(record.confirmed, record.graph_facts)}%` }}
                    />
                  </div>
                  <code className="history-label">
                    #{record.id} @{record.commit_sha.slice(0, 7)}
                  </code>
                  <code className="history-hash" title={record.content_hash}>
                    {record.content_hash.slice(0, 8)}
                  </code>
                </div>
              );
            })}
          </div>
          <p className="muted prov-note" data-testid="determinism-note">
            {deterministic
              ? 'Determinism verified in this history: repeated ingests of the same commit carry identical content hashes.'
              : 'Re-ingesting the same commit yields an identical graph — equal content hashes in this history are the proof.'}
          </p>
        </>
      )}
    </section>
  );
}
