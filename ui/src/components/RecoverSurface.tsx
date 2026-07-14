import type { Job } from '../store';

export interface RecoverSurfaceProps {
  /** The pipeline job driving this recovery, once its first event lands. */
  job: Job | null;
  /** True while the ingest command is in flight (covers the pre-job window). */
  busy: boolean;
  error: string | null;
  onBack: () => void;
  /** Hand the work to the Jobs surface and free the UI. */
  onBackground: () => void;
}

/** Human labels for the core's pipeline stages (`run_ingest` in the app
 *  crate); unknown stages fall through verbatim so a newer core still reads. */
const STAGE_LABELS: Record<string, string> = {
  scan: 'Scanning sources',
  extract: 'T0 parse — call graph & endpoints',
  load: 'Loading the graph',
  stitch: 'Adapters, channel identity & flow tracer',
};

/** Step 3 of the ingest flow (handoff §Recover): progress streamed from real
 *  core job events (`job://changed`), never a timed simulation. Run in
 *  background moves the work to Jobs and returns a usable UI. */
export function RecoverSurface({ job, busy, error, onBack, onBackground }: RecoverSurfaceProps) {
  const failed = !busy && error !== null;
  const stage = job?.stage ? (STAGE_LABELS[job.stage] ?? job.stage) : 'Preparing…';
  const progress = typeof job?.progress === 'number' ? Math.round(job.progress) : null;

  if (failed) {
    return (
      <section className="ingest-flow recover-center" aria-label="Recovery failed">
        <span className="material-symbols-outlined recover-icon failed" aria-hidden="true">
          error
        </span>
        <h2>Recovery failed</h2>
        <p className="error-text">{error}</p>
        <footer className="flow-actions">
          <button type="button" className="secondary-button" onClick={onBack}>
            Back
          </button>
          <button type="button" onClick={onBackground}>
            View in Jobs
          </button>
        </footer>
      </section>
    );
  }

  return (
    <section className="ingest-flow recover-center" aria-label="Recovering">
      <span
        className="material-symbols-outlined recover-icon spinning"
        aria-hidden="true"
        data-testid="recover-spinner"
      >
        progress_activity
      </span>
      <h2>Recovering</h2>
      <p className="recover-stage" role="status">
        {stage}
      </p>
      {progress !== null && (
        <>
          <code className="recover-percent">{progress}%</code>
          <div
            className="recover-progress"
            role="progressbar"
            aria-valuenow={progress}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-label="Recovery progress"
          >
            <div className="recover-progress-bar" style={{ width: `${progress}%` }} />
          </div>
        </>
      )}
      <button type="button" className="secondary-button" onClick={onBackground}>
        Run in background
      </button>
    </section>
  );
}
