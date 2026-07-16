import { useState } from 'react';
import { GROUP_THRESHOLD, groupGapClasses, nextTier, type GapClass } from '../gapClasses';
import { HelpTip } from './HelpTip';
import type {
  AgentProposal,
  ClassEscalationOutcome,
  FindingsSummary,
  RegisterFinding,
  SpecAssertion,
} from '../store';

export interface GapsDriftSurfaceProps {
  /** Header tally — the same register summary Workspace quotes. */
  summary: FindingsSummary | null;
  /** Raw gap-register assertions from the spec bundle (wired, not
   *  re-derived). The surface filters to the tally's own definition. */
  gaps: SpecAssertion[];
  /** Raw drift-register assertions from the spec bundle. */
  drift: SpecAssertion[];
  /** Persisted unsupported/no-evidence rows (#116). */
  registerFindings: RegisterFinding[];
  /** Open a gap's evidence/resolution view (the Resolution Strategy modal
   *  takes over this seam with #113). */
  onOpenGap: (assertion: SpecAssertion) => void;
  /** Gap ids that truncate traced flows (#167): flow-blocking classes rank
   *  first. */
  flowGapIds?: string[];
  /** Batch-escalate a whole class locally as one durable job (#167). */
  onEscalateClass?: (gapIds: string[]) => Promise<ClassEscalationOutcome | null>;
  /** Record accept/reject for one staged proposal from a batch run. */
  onDecideProposal?: (
    proposal: AgentProposal,
    decision: 'accepted' | 'rejected',
  ) => Promise<boolean>;
}

type Tab = 'lanes' | 'tiers' | 'drift';

/** Stable presentational gap ids (G-01…) over the register's sorted order. */
function gapId(index: number): string {
  return `G-${String(index + 1).padStart(2, '0')}`;
}

function GapRows({
  gaps,
  onOpenGap,
}: {
  gaps: SpecAssertion[];
  onOpenGap: (assertion: SpecAssertion) => void;
}) {
  return (
    <ul className="register-rows">
      {gaps.map((gap, index) => (
        <li key={gap.id}>
          <button type="button" className="register-row gap-row" onClick={() => onOpenGap(gap)}>
            <code className="register-id">{gapId(index)}</code>
            <span className="register-text">{gap.summary}</span>
            <span className="register-tail">{nextTier(gap)} next</span>
            <span className="material-symbols-outlined" aria-hidden="true">
              chevron_right
            </span>
          </button>
        </li>
      ))}
    </ul>
  );
}

/** Instances render in pages so a thousand-member class stays responsive. */
const CLASS_PAGE = 50;

function GapClassRow({
  gapClass,
  onOpenGap,
  onEscalateClass,
  onDecideProposal,
}: {
  gapClass: GapClass;
  onOpenGap: (assertion: SpecAssertion) => void;
  onEscalateClass?: (gapIds: string[]) => Promise<ClassEscalationOutcome | null>;
  onDecideProposal?: (
    proposal: AgentProposal,
    decision: 'accepted' | 'rejected',
  ) => Promise<boolean>;
}) {
  const [open, setOpen] = useState(false);
  const [shown, setShown] = useState(CLASS_PAGE);
  const [running, setRunning] = useState(false);
  const [outcome, setOutcome] = useState<ClassEscalationOutcome | null>(null);
  const [decided, setDecided] = useState<ReadonlyMap<string, 'accepted' | 'rejected'>>(new Map());

  const escalate = async () => {
    if (!onEscalateClass || running) return;
    setRunning(true);
    setOutcome(null);
    setDecided(new Map());
    try {
      setOutcome(await onEscalateClass(gapClass.members.map((member) => member.subject_id)));
    } finally {
      setRunning(false);
    }
  };
  const decide = async (proposal: AgentProposal, decision: 'accepted' | 'rejected') => {
    if (!onDecideProposal) return;
    if (await onDecideProposal(proposal, decision)) {
      setDecided((map) => new Map([...map, [proposal.gap_id, decision]]));
    }
  };
  const undecided = outcome?.proposals.filter((proposal) => !decided.has(proposal.gap_id)) ?? [];

  return (
    <li className="register-class">
      <button
        type="button"
        className="register-row gap-row register-class-head"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        <code className="register-id">×{gapClass.members.length}</code>
        <span className="register-text">{gapClass.label}</span>
        {gapClass.flowImpact > 0 && (
          <span className="register-tail flow-impact">
            blocks {gapClass.flowImpact} flow hop{gapClass.flowImpact === 1 ? '' : 's'}
          </span>
        )}
        <code className="register-tail">{gapClass.extractor}</code>
        <span className="register-tail">{gapClass.tier} next</span>
        <span className="material-symbols-outlined" aria-hidden="true">
          {open ? 'expand_less' : 'expand_more'}
        </span>
      </button>
      {open && (
        <>
          {onEscalateClass && (
            <div className="class-escalation">
              <button type="button" disabled={running} onClick={() => void escalate()}>
                {running
                  ? 'Escalating class…'
                  : `Escalate class locally (${gapClass.members.length} instances)`}
              </button>
              <span className="muted">
                One durable job, one proposal per instance — staged only, nothing joins the
                graph unaccepted. Cloud escalation stays per-instance (consent binds to one
                exact payload).
              </span>
            </div>
          )}
          {outcome && (
            <div className="class-escalation-outcome" role="status">
              <p>
                {outcome.proposals.length} staged proposal
                {outcome.proposals.length === 1 ? '' : 's'} · {outcome.failures.length} failed
                {outcome.cancelled ? ' · cancelled early' : ''}
              </p>
              {undecided.length > 0 && (
                <ul className="class-proposals" aria-label="Staged class proposals">
                  {undecided.map((proposal) => (
                    <li key={proposal.gap_id}>
                      <span className="register-text">
                        {proposal.source_id} —{proposal.edge_label}→ {proposal.target_id}
                      </span>
                      <button type="button" onClick={() => void decide(proposal, 'accepted')}>
                        Accept
                      </button>
                      <button type="button" onClick={() => void decide(proposal, 'rejected')}>
                        Reject
                      </button>
                    </li>
                  ))}
                </ul>
              )}
              {outcome.failures.length > 0 && (
                <ul className="class-failures" aria-label="Failed instances">
                  {outcome.failures.slice(0, 10).map((failure) => (
                    <li key={failure.gap_id} className="muted">
                      {failure.gap_id}: {failure.error}
                    </li>
                  ))}
                </ul>
              )}
            </div>
          )}
          <GapRows gaps={gapClass.members.slice(0, shown)} onOpenGap={onOpenGap} />
          {gapClass.members.length > shown && (
            <button
              type="button"
              className="register-show-more"
              onClick={() => setShown((value) => value + CLASS_PAGE * 4)}
            >
              Show more ({gapClass.members.length - shown} remaining)
            </button>
          )}
        </>
      )}
    </li>
  );
}

/** Classes, not instances, are the unit of triage at scale (#167): each
 *  class is one cause (stop-reason × extractor) with its count — “8,352
 *  gaps” reads as a ranked handful of causes. */
function GapClassRows({
  classes,
  onOpenGap,
  onEscalateClass,
  onDecideProposal,
}: {
  classes: GapClass[];
  onOpenGap: (assertion: SpecAssertion) => void;
  onEscalateClass?: (gapIds: string[]) => Promise<ClassEscalationOutcome | null>;
  onDecideProposal?: (
    proposal: AgentProposal,
    decision: 'accepted' | 'rejected',
  ) => Promise<boolean>;
}) {
  return (
    <ul className="register-rows" aria-label="Gap classes">
      {classes.map((gapClass) => (
        <GapClassRow
          key={gapClass.key}
          gapClass={gapClass}
          onOpenGap={onOpenGap}
          onEscalateClass={onEscalateClass}
          onDecideProposal={onDecideProposal}
        />
      ))}
    </ul>
  );
}

/** Gaps & Drift register (handoff screenshot 05): the honest slice the
 *  deterministic core could not confirm. The three lanes never conflate —
 *  a System Gap gets a Resolution Strategy; unsupported and no-evidence
 *  findings are tool limitations, explicitly not gaps (R-INT-4). */
export function GapsDriftSurface({
  summary,
  gaps,
  drift,
  registerFindings,
  onOpenGap,
  flowGapIds,
  onEscalateClass,
  onDecideProposal,
}: GapsDriftSurfaceProps) {
  const [tab, setTab] = useState<Tab>('lanes');
  // Reconcile with findings_summary's own definitions: the gap tally counts
  // gap nodes + gap edges (flow-hop assertions restate the same gaps inside
  // flows), and the drift headline counts drift nodes (CONFLICTS/DRIFTS_FROM
  // edges are supporting assertions of the same finding, not new findings).
  const gapFindings = gaps.filter((assertion) => !assertion.id.startsWith('flow:'));
  // At scale the lane triages by cause class, never row-by-row (#167).
  const flowGapSet = new Set(flowGapIds ?? []);
  const gapClasses =
    gapFindings.length > GROUP_THRESHOLD ? groupGapClasses(gapFindings, flowGapSet) : null;
  const driftFindings = drift.filter((assertion) => assertion.id.startsWith('node:'));
  const unsupported = registerFindings.filter((finding) => finding.kind === 'unsupported');
  const noEvidence = registerFindings.filter((finding) => finding.kind === 'no-evidence');
  const tiers: ('T1' | 'T2' | 'T3')[] = ['T1', 'T2', 'T3'];

  return (
    <section className="register-surface" aria-label="Gap and Drift register">
      <header className="ingest-hero">
        <h2>Gap &amp; Drift register</h2>
        {summary ? (
          <p className="muted">
            The honest slice the deterministic core could not confirm. Register complete ·{' '}
            <strong>{summary.open_findings} open findings</strong> —{' '}
            <span className="lane-gap">{summary.gaps} gaps</span> ·{' '}
            <span className="lane-unsupported">{summary.unsupported} unsupported</span> ·{' '}
            <strong>{summary.no_evidence} no-evidence</strong>. Each gap carries a Resolution
            Strategy — escalate to T1/T2/T3 and review the proposal.
          </p>
        ) : (
          <p className="muted">No backend connected — the register lives in the core.</p>
        )}
      </header>

      <div className="register-tabs" role="tablist" aria-label="Register views">
        {(
          [
            ['lanes', 'Lanes'],
            ['tiers', 'By escalation tier'],
            ['drift', 'Drift'],
          ] as [Tab, string][]
        ).map(([id, label]) => (
          <button
            key={id}
            type="button"
            role="tab"
            aria-selected={tab === id}
            className={`register-tab${tab === id ? ' active' : ''}`}
            onClick={() => setTab(id)}
          >
            {label}
          </button>
        ))}
      </div>

      {tab === 'lanes' && (
        <div className="register-lanes">
          <div className="register-lane gap-lane">
            <h3>
              <span className="material-symbols-outlined" aria-hidden="true">
                link_off
              </span>
              System gaps · {gapFindings.length}
              <HelpTip topic="gap" />
              {gapClasses && ` — ${gapClasses.length} ${gapClasses.length === 1 ? 'cause' : 'causes'}`}
            </h3>
            <p className="muted">
              {gapClasses
                ? 'Grouped by cause (stop reason × extractor), largest first — expand a class for its instances. Each gap has a Resolution Strategy.'
                : 'Hops the deterministic tier could not resolve. Each has a Resolution Strategy.'}
            </p>
          </div>
          {gapFindings.length === 0 ? (
            <p className="muted">No unresolved facts.</p>
          ) : gapClasses ? (
            <GapClassRows
              classes={gapClasses}
              onOpenGap={onOpenGap}
              onEscalateClass={onEscalateClass}
              onDecideProposal={onDecideProposal}
            />
          ) : (
            <GapRows gaps={gapFindings} onOpenGap={onOpenGap} />
          )}

          <div className="register-lane unsupported-lane">
            <h3>
              <span className="material-symbols-outlined" aria-hidden="true">
                block
              </span>
              Unsupported patterns · {unsupported.length}
              <HelpTip topic="unsupported" />
            </h3>
            <p className="muted">
              Constructs no adapter covers yet — a tool limitation, not a System Gap. Reported
              honestly, never guessed.
            </p>
          </div>
          {unsupported.length === 0 ? (
            <p className="muted">None detected.</p>
          ) : (
            <ul className="register-rows">
              {unsupported.map((finding) => (
                <li key={finding.id} className="register-row static">
                  <span className="register-text">{finding.message}</span>
                  <code className="register-tail">
                    {finding.path}:{finding.line}
                  </code>
                </li>
              ))}
            </ul>
          )}

          <div className="register-lane no-evidence-lane">
            <h3>
              <span className="material-symbols-outlined" aria-hidden="true">
                search_off
              </span>
              No evidence found · {noEvidence.length}
              <HelpTip topic="noEvidence" />
            </h3>
            <p className="muted">
              Questions recovery looked for and could not answer — absence is stated, not
              implied.
            </p>
          </div>
          {noEvidence.length === 0 ? (
            <p className="muted">None recorded.</p>
          ) : (
            <ul className="register-rows">
              {noEvidence.map((finding) => (
                <li key={finding.id} className="register-row static">
                  <span className="register-text">{finding.message}</span>
                  <code className="register-tail">
                    {finding.path}:{finding.line}
                  </code>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      {tab === 'tiers' && (
        <div className="register-lanes">
          {tiers.map((tier) => {
            const tierGaps = gapFindings.filter((gap) => nextTier(gap) === tier);
            return (
              <div key={tier}>
                <div className="register-lane">
                  <h3>
                    <code className="register-id">{tier}</code> next escalation ·{' '}
                    {tierGaps.length} open
                  </h3>
                </div>
                {tierGaps.length === 0 ? (
                  <p className="muted">No gaps waiting on {tier}.</p>
                ) : tierGaps.length > GROUP_THRESHOLD ? (
                  <GapClassRows
                    classes={groupGapClasses(tierGaps, flowGapSet)}
                    onOpenGap={onOpenGap}
                    onEscalateClass={onEscalateClass}
                    onDecideProposal={onDecideProposal}
                  />
                ) : (
                  <GapRows gaps={tierGaps} onOpenGap={onOpenGap} />
                )}
              </div>
            );
          })}
          <p className="muted">
            T2/T3 escalations propose only — they never overwrite T0/T1 facts (R-INT-1).
          </p>
        </div>
      )}

      {tab === 'drift' && (
        <div className="register-lanes">
          <div className="register-lane drift-lane">
            <h3>
              <span className="material-symbols-outlined" aria-hidden="true">
                gavel
              </span>
              ADR / code drift · {driftFindings.length}
            </h3>
            <p className="muted">
              Declared decisions the recovered behavior conflicts with — mapped to the offending
              edge, confidence preserved.
            </p>
          </div>
          {driftFindings.length === 0 ? (
            <p className="muted">No ADR/code conflicts recovered.</p>
          ) : (
            <ul className="register-rows">
              {driftFindings.map((finding) => (
                <li key={finding.id} className="register-row static">
                  <span className="register-text">{finding.summary}</span>
                  <code className="register-tail">{finding.subject_id}</code>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </section>
  );
}
