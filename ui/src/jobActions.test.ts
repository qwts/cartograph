import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { useAppStore, type Job } from './store';

const recorded = (status = 'running'): Job => ({
  id: 41, kind: 'ingest-source-v1:src_fixture', status, execution_tracking: 'recorded',
  created_at: 'original-created', updated_at: 'original-updated', stage: 'extract', progress: 37,
});

beforeEach(() => {
  vi.stubGlobal('window', {});
  useAppStore.setState({ jobs: [], jobActionError: null });
});

afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
  useAppStore.setState({ jobs: [], jobActionError: null });
});

describe('rejected job actions preserve visible history (AC-0162)', () => {
  it('reports busy retry without changing jobs or refreshing away the failure', async () => {
    const jobs = [recorded('cancelled')];
    const commands: string[] = [];
    useAppStore.setState({ jobs });
    mockIPC((command) => { commands.push(command); throw new Error('execution ownership is busy'); });
    await expect(useAppStore.getState().retryJob(41)).resolves.toBeUndefined();
    expect(useAppStore.getState().jobs).toBe(jobs);
    expect(commands).toEqual(['retry_job']);
    expect(useAppStore.getState().jobActionError).toContain('Retry for job #41');
    expect(useAppStore.getState().jobActionError).toContain('execution ownership is busy');
  });

  it('reports rejected cancellation without changing the stored status', async () => {
    const jobs = [recorded()];
    useAppStore.setState({ jobs });
    mockIPC(() => { throw new Error('job state changed'); });
    await expect(useAppStore.getState().cancelJob(41)).resolves.toBeUndefined();
    expect(useAppStore.getState().jobs).toBe(jobs);
    expect(useAppStore.getState().jobActionError).toContain('Cancel for job #41');
    expect(useAppStore.getState().jobActionError).toContain('Refresh Jobs');
  });

  it('does not treat absent cancel or retry receipts as successful transitions', async () => {
    mockIPC(() => null);
    const jobs = [recorded('cancelled')];
    useAppStore.setState({ jobs });
    await useAppStore.getState().retryJob(41);
    expect(useAppStore.getState().jobs).toBe(jobs);
    expect(useAppStore.getState().jobActionError).toContain('did not confirm');
    await useAppStore.getState().cancelJob(41);
    expect(useAppStore.getState().jobs).toBe(jobs);
    expect(useAppStore.getState().jobActionError).toContain('did not confirm');
  });

  it('requires a fresh operation for explicit or absent legacy tracking without invoking retry', async () => {
    const commands: string[] = [];
    mockIPC((command) => { commands.push(command); return null; });
    for (const tracking of ['legacy_unknown', undefined] as const) {
      const jobs = [{ ...recorded('interrupted'), execution_tracking: tracking }];
      useAppStore.setState({ jobs });
      await useAppStore.getState().retryJob(41);
      expect(useAppStore.getState().jobs).toBe(jobs);
      expect(useAppStore.getState().jobActionError).toContain('Start a fresh operation');
    }
    expect(commands).toEqual([]);
  });

  it('clears the action error after a confirmed cancellation and keeps the returned history', async () => {
    const cancelled = { ...recorded('cancelled'), updated_at: 'confirmed-updated' };
    useAppStore.setState({ jobs: [recorded()], jobActionError: 'earlier rejection' });
    mockIPC(() => cancelled);
    await useAppStore.getState().cancelJob(41);
    expect(useAppStore.getState().jobs).toEqual([cancelled]);
    expect(useAppStore.getState().jobActionError).toBeNull();
  });

  it('distinguishes a confirmed retry from a later refresh failure', async () => {
    const running = recorded();
    useAppStore.setState({ jobs: [recorded('cancelled')], jobActionError: 'earlier rejection' });
    mockIPC((command) => {
      if (command === 'retry_job') return running;
      throw new Error('refresh unavailable');
    });
    await useAppStore.getState().retryJob(41);
    expect(useAppStore.getState().jobs).toEqual([running]);
    expect(useAppStore.getState().jobActionError).toContain('Retry for job #41 was confirmed');
  });

  it('does not let a late rejected action replace a newer successful action result', async () => {
    let rejectFirst: (reason: Error) => void = () => {};
    let requests = 0;
    const cancelled = recorded('cancelled');
    mockIPC(() => ++requests === 1 ? new Promise((_resolve, reject) => { rejectFirst = reject; }) : cancelled);
    useAppStore.setState({ jobs: [recorded()] });
    const first = useAppStore.getState().cancelJob(41);
    await useAppStore.getState().cancelJob(41);
    rejectFirst(new Error('older failure'));
    await first;
    expect(useAppStore.getState().jobActionError).toBeNull();
    expect(useAppStore.getState().jobs).toEqual([cancelled]);
  });
});
