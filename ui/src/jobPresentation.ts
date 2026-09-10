import type { Job } from './store';

/** Tracking describes protocol participation; it does not establish liveness. */
export function hasRecordedExecution(job: Job): boolean {
  return job.execution_tracking === 'recorded';
}

export function isRecoveryJob(kind: string): boolean {
  return ['ingest-source-v1:', 'ingest:', 'add-repo:', 'add-system:'].some((prefix) => kind.startsWith(prefix));
}

/** An explicit pin never falls back to another job. Unknown pinned records can
 * be shown as history; automatic selection admits only tracked recovery work. */
export function selectRecoveryJob(jobs: Job[], pinnedId: number | null): Job | null {
  if (pinnedId !== null) return jobs.find((job) => job.id === pinnedId && isRecoveryJob(job.kind)) ?? null;
  return jobs.find((job) => hasRecordedExecution(job) && isRecoveryJob(job.kind) &&
    (job.status === 'running' || job.status === 'queued')) ?? null;
}
