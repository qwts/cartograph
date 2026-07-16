import { useState } from 'react';
import type { Job } from '../store';
import { stageLabel } from '../stageLabels';

export interface JobsSurfaceProps {
  jobs: Job[];
  /** Disabled when there is no live backend to clear against. */
  canClear: boolean;
  onClearFinished: () => void;
  onCancel: (id: number) => void;
  onRetry: (id: number) => void;
  /** Jump to the live Recovering screen for a running/queued recovery job
   *  (#209) — the id isn't threaded further since the Recover surface finds
   *  its job by status, but it's passed through for future kind-specific use. */
  onViewLive?: (id: number) => void;
}

/** Recovery-flow job kinds (`store.ts`'s `ingest()` dispatches one of these
 *  three backend commands) — only these have a Recover surface to jump back
 *  to; other job kinds (plugin gates, escalations) do not. */
const RECOVERY_KIND_PREFIXES = ['ingest:', 'add-repo:', 'add-system:'];

function isRecoveryJob(kind: string): boolean {
  return RECOVERY_KIND_PREFIXES.some((prefix) => kind.startsWith(prefix));
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
  onClearFinished,
  onCancel,
  onRetry,
  onViewLive,
}: JobsSurfaceProps) {
  const [confirming, setConfirming] = useState(false);
  const finished = jobs.filter((job) => FINISHED.has(job.status)).length;
  return (
    <section className="jobs-surface">
      <header className="jobs-surface-header">
        <div>
          <h2>Jobs</h2>
          <p className="muted">
            The job spine is durable — restart the app and this list survives; interrupted work
            resumes.
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
      {jobs.length === 0 ? (
        <p className="muted">No jobs yet.</p>
      ) : (
        <ul className="jobs-rows">
          {jobs.map((job) => {
            const action = actionFor(job.status);
            return (
              <li key={job.id} className={`job-row job-${job.status}`}>
                <span
                  className={`material-symbols-outlined job-icon${
                    job.status === 'running' ? ' spinning' : ''
                  }`}
                  aria-hidden="true"
                >
                  {STATUS_ICON[job.status] ?? 'help'}
                </span>
                <div className="job-main">
                  <div className="job-title">
                    <code>#{job.id}</code> {job.kind}
                    <span className={`job-status job-status-${job.status}`}>{job.status}</span>
                    {job.stage && job.status === 'running' && (
                      <span className="job-stage">{stageLabel(job.stage)}</span>
                    )}
                  </div>
                  {job.status === 'running' && job.detail && (
                    <p className="job-detail muted">
                      <code>{job.detail}</code>
                    </p>
                  )}
                  {job.status === 'running' && typeof job.progress === 'number' && (
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
                </div>
                {onViewLive &&
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
                {action && (
                  <button
                    type="button"
                    className="job-action"
                    onClick={() =>
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
