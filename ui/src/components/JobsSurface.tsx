import { useState } from 'react';
import type { Job } from '../store';
import { stageLabel } from '../stageLabels';
import { hasRecordedExecution, isRecoveryJob } from '../jobPresentation';

export interface JobsSurfaceProps {
  jobs: Job[];
  /** Disabled when there is no live backend to clear against. */
  canClear: boolean;
  actionError?: string | null;
  onClearFinished: () => void;
  onCancel: (id: number) => void;
  onRetry: (id: number) => void;
  /** Pin this recorded recovery job in the Recover surface. */
  onViewLive?: (id: number) => void;
  onViewInvestigation?: (id: string) => void;
  onCancelInvestigation?: (id: string) => void;
}

/** Terminal statuses removed by Clear finished; resumable work never is. */
const FINISHED = new Set(['done', 'failed', 'cancelled']);

const STATUS_ICON: Record<string, string> = {
  queued: 'schedule',
  running: 'progress_activity',
  done: 'check_circle',
  failed: 'error',
  cancelled: 'block',
  interrupted: 'motion_photos_paused',
};

/** The action a status affords: cancel while pending, retry after failure,
 *  resume after an interruption (handoff §Jobs; #117 lifecycle verbs). */
function actionFor(status: string): { label: string; kind: 'cancel' | 'retry' } | null {
  switch (status) {
    case 'queued':
    case 'running':
      return { label: 'Cancel', kind: 'cancel' };
    case 'failed':
    case 'cancelled':
      return { label: 'Retry', kind: 'retry' };
    case 'interrupted':
      return { label: 'Resume', kind: 'retry' };
    default:
      return null;
  }
}

/** Durable job management (handoff §Jobs, interaction #5): every state with
 *  progress, stage, timestamps, failure detail, and artifact links. Long
 *  work stays non-blocking — this surface observes, never blocks. */
export function JobsSurface({
  jobs,
  canClear,
  actionError = null,
  onClearFinished,
  onCancel,
  onRetry,
  onViewLive,
  onViewInvestigation,
  onCancelInvestigation,
}: JobsSurfaceProps) {
  const [confirming, setConfirming] = useState(false);
  const finished = jobs.filter((job) => FINISHED.has(job.status)).length;
  return (
    <section className="jobs-surface">
      <header className="jobs-surface-header">
        <div>
          <h2>Jobs</h2>
          <p className="muted">
            Job records survive restart. Recorded execution tracking enables guarded controls;
            a stored status alone does not prove a worker is live. Cancellation is cooperative.
          </p>
        </div>
        {!confirming ? (
          <button
            type="button"
            disabled={!canClear || finished === 0}
            onClick={() => setConfirming(true)}
          >
            Clear finished
          </button>
        ) : (
          <div className="clear-confirmation" role="alert">
            <p>
              Remove {finished} finished {finished === 1 ? 'job' : 'jobs'}? Queued, running, and
              resumable work is kept.
            </p>
            <div className="clear-confirmation-actions">
              <button
                type="button"
                className="secondary-button"
                onClick={() => setConfirming(false)}
              >
                Keep history
              </button>
              <button
                type="button"
                className="danger-button"
                onClick={() => {
                  setConfirming(false);
                  onClearFinished();
                }}
              >
                Confirm clear
              </button>
            </div>
          </div>
        )}
      </header>
      {actionError && <p className="job-error" role="alert">{actionError}</p>}
      {jobs.length === 0 ? (
        <p className="muted">No jobs yet.</p>
      ) : (
        <ul className="jobs-rows">
          {jobs.map((job) => {
            const action = actionFor(job.status);
            const unknown = !hasRecordedExecution(job);
            const investigation = job.investigation_id;
            const legacyRetry = (unknown || job.kind.startsWith('ingest:')) && action?.kind === 'retry';
            return (
              <li key={job.id} className={`job-row job-${job.status}`}>
                <span
                  className={`material-symbols-outlined job-icon${
                    !unknown && job.status === 'running' ? ' spinning' : ''
                  }`}
                  aria-hidden="true"
                >
                  {unknown ? 'history' : STATUS_ICON[job.status] ?? 'help'}
                </span>
                <div className="job-main">
                  <div className="job-title">
                    <code>#{job.id}</code> {job.kind}
                    <span className={`job-status job-status-${job.status}`}>{job.status}</span>
                    {!unknown && job.stage && job.status === 'running' && (
                      <span className="job-stage">{stageLabel(job.stage)}</span>
                    )}
                  </div>
                  {!unknown && job.status === 'running' && job.detail && (
                    <p className="job-detail muted">
                      <code>{job.detail}</code>
                    </p>
                  )}
                  {unknown && <>
                    <p className="muted">Execution ownership unknown — this is a legacy record, not proof of live work. Start a fresh operation from its source; same-ID Retry and Resume are unavailable.</p>
                    {(job.stage || typeof job.progress === 'number') && <p className="muted">
                      {job.stage && `Stored stage: ${stageLabel(job.stage)}. `}
                      {typeof job.progress === 'number' && `Stored progress: ${Math.round(job.progress)}%.`}
                    </p>}
                  </>}
                  {legacyRetry && !unknown && (
                    <p className="muted">
                      This historical job has no registered source. Run a new ingestion to retry.
                    </p>
                  )}
                  {!unknown && job.status === 'running' && typeof job.progress === 'number' && (
                    <div
                      className="job-progress"
                      role="progressbar"
                      aria-valuenow={Math.round(job.progress)}
                      aria-valuemin={0}
                      aria-valuemax={100}
                      aria-label={`Job ${job.id} progress`}
                    >
                      <div className="job-progress-bar" style={{ width: `${job.progress}%` }} />
                    </div>
                  )}
                  {job.status === 'failed' && job.error && (
                    <p className="job-error">{job.error}</p>
                  )}
                  {job.status === 'done' && (job.artifacts?.length ?? 0) > 0 && (
                    <p className="job-artifacts">
                      {job.artifacts?.map((artifact) => (
                        <code key={artifact}>{artifact}</code>
                      ))}
                    </p>
                  )}
                  <p className="job-times muted">
                    created {job.created_at} · updated {job.updated_at}
                  </p>
                  {investigation && <p className="muted">Specialist history is kept separately. Open the investigation for its findings, consent and execution outcome; uncertain calls are never replayed automatically.</p>}
                </div>
                {investigation && onViewInvestigation && <button type="button" className="job-action secondary-button"
                  onClick={() => onViewInvestigation(investigation)}>View investigation</button>}
                {onViewLive &&
                  !unknown &&
                  (job.status === 'running' || job.status === 'queued') &&
                  isRecoveryJob(job.kind) && (
                    <button
                      type="button"
                      className="job-action secondary-button"
                      onClick={() => onViewLive(job.id)}
                    >
                      View live
                    </button>
                  )}
                {action && (!investigation || action.kind === 'cancel') && (
                  <button
                    type="button"
                    className="job-action"
                    disabled={legacyRetry || Boolean(investigation && !onCancelInvestigation)}
                    title={legacyRetry ? 'Start a fresh operation from the source; this historical execution cannot be resumed.' : undefined}
                    onClick={() =>
                      investigation ? onCancelInvestigation?.(investigation) :
                        action.kind === 'cancel' ? onCancel(job.id) : onRetry(job.id)
                    }
                  >
                    {action.label}
                  </button>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
