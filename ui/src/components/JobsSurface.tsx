import type { Job } from '../store';

export interface JobsSurfaceProps {
  jobs: Job[];
  /** Disabled when there is no live backend to enqueue into. */
  canEnqueue: boolean;
  onEnqueue: (kind: string) => void;
  onCancel: (id: number) => void;
  onRetry: (id: number) => void;
}

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
export function JobsSurface({ jobs, canEnqueue, onEnqueue, onCancel, onRetry }: JobsSurfaceProps) {
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
        <button type="button" onClick={() => onEnqueue('noop')} disabled={!canEnqueue}>
          Enqueue test job
        </button>
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
                      <span className="job-stage">{job.stage}</span>
                    )}
                  </div>
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
