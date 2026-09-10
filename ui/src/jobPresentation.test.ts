import { describe, expect, it } from 'vitest';
import { selectRecoveryJob } from './jobPresentation';
import type { Job } from './store';

const job = (id: number, overrides: Partial<Job> = {}): Job => ({
  id, kind: 'ingest-source-v1:src_fixture', status: 'running',
  execution_tracking: 'recorded', created_at: 'created', updated_at: 'updated', ...overrides,
});

describe('recovery selection respects execution tracking (AC-0162)', () => {
  it('skips legacy rows and non-recovery workers before selecting recorded recovery', () => {
    const current = job(4);
    const jobs = [job(1, { execution_tracking: 'legacy_unknown' }),
      job(2, { execution_tracking: undefined }), job(3, { kind: 'plugin-gate:fixture' }), current];
    expect(selectRecoveryJob(jobs, null)).toBe(current);
  });

  it('preserves an exact legacy pin for historical display without substituting another job', () => {
    const historical = job(1, { execution_tracking: 'legacy_unknown' });
    expect(selectRecoveryJob([job(2), historical], 1)).toBe(historical);
    expect(selectRecoveryJob([job(2)], 1)).toBeNull();
  });

  it('does not invent active recovery from legacy, terminal, or unrelated rows', () => {
    const jobs = [job(1, { execution_tracking: undefined }), job(2, { status: 'done' }),
      job(3, { kind: 'escalate:gap' })];
    expect(selectRecoveryJob(jobs, null)).toBeNull();
    expect(selectRecoveryJob(jobs, 3)).toBeNull();
  });
});
