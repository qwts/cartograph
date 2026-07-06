import type { Job } from '../store';

export interface JobsCardProps {
  jobs: Job[];
  /** Disabled when there is no live backend to enqueue into. */
  canEnqueue: boolean;
  onEnqueue: (kind: string) => void;
}

/** Durable job list from the state spine, plus a test-job trigger. */
export function JobsCard({ jobs, canEnqueue, onEnqueue }: JobsCardProps) {
  return (
    <section className="card">
      <h2>Jobs</h2>
      {jobs.length === 0 ? (
        <p className="muted">
          No jobs yet. The job spine is durable — restart the app and this list survives.
        </p>
      ) : (
        <ul className="jobs-list">
          {jobs.map((job) => (
            <li key={job.id}>
              <span>
                #{job.id} {job.kind}
              </span>
              <span className="muted">{job.status}</span>
            </li>
          ))}
        </ul>
      )}
      <p style={{ marginTop: '0.75rem' }}>
        <button onClick={() => onEnqueue('noop')} disabled={!canEnqueue}>
          Enqueue test job
        </button>
      </p>
    </section>
  );
}
