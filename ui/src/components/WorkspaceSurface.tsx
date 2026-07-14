import type {
  FindingsSummary,
  IngestSummary,
  SpecBundle,
  TierDistribution,
} from '../store';

export interface WorkspaceSurfaceProps {
  summary: IngestSummary | null;
  findings: FindingsSummary | null;
  distribution: TierDistribution;
  bundle: SpecBundle | null;
  onReingest: () => void;
  onTriageGaps: () => void;
  onProvenance: () => void;
  /** Open an artifact in the Spec Workbench. */
  onOpenArtifact: () => void;
}

/** The landing's artifact cards (handoff §Workspace): matched to the spec
 *  bundle by file name so a missing artifact is visibly not generated. */
const ARTIFACT_CARDS: { file: string; title: string; icon: string }[] = [
  { file: 'user_stories.md', title: 'user_stories.md', icon: 'description' },
  { file: 'flow_dossiers.md', title: 'Flow dossiers', icon: 'account_tree' },
  { file: 'US-TM.md', title: 'US-TM.md', icon: 'table_chart' },
  { file: 'topology.md', title: 'Topology / resource map', icon: 'map' },
  { file: 'gap_register.md', title: 'Gap register', icon: 'report' },
  { file: 'adrs.md', title: 'ADR set + drift', icon: 'gavel' },
];

function percent(part: number, total: number): string {
  return total === 0 ? '0%' : `${Math.round((part / total) * 100)}%`;
}

/** Authority on the recovery axis: gaps make it partial; inferred facts make
 *  it inferred; a fully Confirmed graph is authoritative. Never conflated
 *  with the generation axis. */
function recoveryAuthority(findings: FindingsSummary, distribution: TierDistribution): string {
  if (findings.gaps > 0) return 'Recovery: partial';
  if (distribution.inferredStrong + distribution.inferredWeak > 0) return 'Recovery: inferred';
  return 'Recovery: authoritative';
}

/** Post-recovery landing (handoff screenshot 01): the honest outcome — tier
 *  overall, explicit findings tally, provenance health, artifact grid with
 *  independent generation/authority badges. Every count comes from the same
 *  register summary and atlas projection the other surfaces read. */
export function WorkspaceSurface({
  summary,
  findings,
  distribution,
  bundle,
  onReingest,
  onTriageGaps,
  onProvenance,
  onOpenArtifact,
}: WorkspaceSurfaceProps) {
  const recovered = findings !== null && findings.graph_facts > 0;

  const systemName =
    summary?.repo ??
    (summary?.repos?.length ? `${summary.repos.length} repos as one system` : null) ??
    (recovered ? 'Ingested system' : 'No system yet');
  const commit = summary?.commit_sha?.slice(0, 7) ?? (recovered ? 'workdir' : null);

  const outcomeTitle = !recovered
    ? 'No recovery yet'
    : findings.gaps > 0
      ? 'Partial recovery'
      : findings.open_findings > 0
        ? 'Recovery with open findings'
        : 'Full recovery';
  const overallTier = !recovered
    ? null
    : distribution.inferredStrong + distribution.inferredWeak > 0 || findings.gaps > 0
      ? { label: 'InferredStrong overall', className: 'tier-inferredstrong' }
      : { label: 'Confirmed overall', className: 'tier-confirmed' };

  return (
    <section className="workspace-landing" aria-label="Workspace">
      <header className="workspace-title-row">
        <div className="workspace-title">
          <h2>{systemName}</h2>
          {commit && <code className="commit-chip">@ {commit}</code>}
        </div>
        <button type="button" onClick={onReingest}>
          <span className="material-symbols-outlined" aria-hidden="true">
            add
          </span>
          {recovered ? 'Re-ingest' : 'Connect a target'}
        </button>
      </header>

      <div className="outcome-card" data-testid="outcome-card">
        <span className="material-symbols-outlined outcome-icon" aria-hidden="true">
          insights
        </span>
        <div className="outcome-main">
          <h3>
            {outcomeTitle}
            {overallTier && (
              <span className={`tier-badge ${overallTier.className}`}>{overallTier.label}</span>
            )}
          </h3>
          {recovered ? (
            <p>
              A third party could re-specify the Confirmed core from the artifacts.{' '}
              <strong>{findings.open_findings} open findings</strong> — {findings.gaps} gaps and{' '}
              {findings.unsupported} unsupported patterns (plus {findings.no_evidence}{' '}
              no-evidence) — are listed explicitly rather than guessed. Triage them to raise
              recovery authority.
            </p>
          ) : (
            <p className="muted">
              Connect a target to recover structure, flows, and a provenance-tagged spec —
              everything runs on-device.
            </p>
          )}
          {recovered && (
            <div className="outcome-actions">
              <button type="button" onClick={onTriageGaps}>
                Triage {findings.gaps} gaps
              </button>
              <button type="button" className="secondary-button" onClick={onProvenance}>
                Provenance &amp; eval
              </button>
            </div>
          )}
        </div>
      </div>

      {recovered && (
        <>
          <h3 className="settings-section-title">Provenance health</h3>
          <ul className="prov-health" data-testid="prov-health">
            <li className="prov-card confirmed">
              <span className="prov-name">Confirmed</span>
              <strong data-testid="count-confirmed">{distribution.confirmed}</strong>
              <span className="muted">
                {percent(distribution.confirmed, distribution.total)} of {distribution.total}{' '}
                facts
              </span>
            </li>
            <li className="prov-card inferred-strong">
              <span className="prov-name">Inferred Strong</span>
              <strong data-testid="count-inferred-strong">{distribution.inferredStrong}</strong>
              <span className="muted">
                {percent(distribution.inferredStrong, distribution.total)} of facts
              </span>
            </li>
            <li className="prov-card inferred-weak">
              <span className="prov-name">Inferred Weak</span>
              <strong data-testid="count-inferred-weak">{distribution.inferredWeak}</strong>
              <span className="muted">
                {percent(distribution.inferredWeak, distribution.total)} of facts
              </span>
            </li>
            <li className="prov-card gap">
              <span className="prov-name">Gap</span>
              <strong data-testid="count-gap">{findings.gaps}</strong>
              <span className="muted">{findings.gaps} in register</span>
            </li>
            <li className="prov-card unsupported">
              <span className="prov-name">Unsupported</span>
              <strong data-testid="count-unsupported">{findings.unsupported}</strong>
              <span className="muted">register finding</span>
            </li>
          </ul>
          <p className="muted prov-note">
            Coverage cards are a summary — the full tier distribution, extractor coverage, and
            eval health live in Provenance &amp; Eval.
          </p>

          <h3 className="settings-section-title">Artifacts</h3>
          <ul className="artifact-grid">
            {ARTIFACT_CARDS.map((card) => {
              const generated = bundle?.artifacts.some(
                (artifact) => artifact.file_name === card.file,
              );
              const isRegister = card.file === 'gap_register.md';
              return (
                <li key={card.file}>
                  <button type="button" className="artifact-card" onClick={onOpenArtifact}>
                    <span className="material-symbols-outlined" aria-hidden="true">
                      {card.icon}
                    </span>
                    <span className="artifact-title">{card.title}</span>
                    <span className="artifact-badges">
                      {isRegister ? (
                        // The register is a live worklist: exactly one
                        // completion-style badge, never two axes.
                        <span className="artifact-badge findings">
                          {findings.open_findings} open findings
                        </span>
                      ) : generated ? (
                        <>
                          <span className="artifact-badge generated">Artifact generated</span>
                          <span className="artifact-badge authority">
                            {recoveryAuthority(findings, distribution)}
                          </span>
                        </>
                      ) : (
                        <span className="artifact-badge missing">Not generated</span>
                      )}
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
        </>
      )}
    </section>
  );
}
