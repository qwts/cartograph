import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { useAppStore, type IngestParallelism } from './store';

beforeEach(() => {
  vi.stubGlobal('window', {});
  useAppStore.setState({ ingestParallelism: null, settingsError: null });
});

afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
  useAppStore.setState({ ingestParallelism: null, settingsError: null });
});

describe('ingest parallelism setting (AC-0208, #236)', () => {
  it('persists the choice and shows what the core applied', async () => {
    const calls: unknown[] = [];
    mockIPC((command, args) => {
      calls.push([command, args]);
      return {
        setting: 3,
        auto_workers: 7,
        workers: 3,
        max_workers: 12,
      } satisfies IngestParallelism;
    });
    await useAppStore.getState().setIngestParallelism(3);
    expect(calls).toEqual([['set_ingest_parallelism', { setting: 3 }]]);
    expect(useAppStore.getState().ingestParallelism).toEqual({
      setting: 3,
      auto_workers: 7,
      workers: 3,
      max_workers: 12,
    });
  });

  it('keeps the previous setting and surfaces the error when the core refuses', async () => {
    const previous: IngestParallelism = { setting: 0, auto_workers: 7, workers: 7, max_workers: 12 };
    useAppStore.setState({ ingestParallelism: previous });
    mockIPC(() => {
      throw new Error('ingest parallelism must be Auto (0) or 1..=12 workers, got 99');
    });
    await useAppStore.getState().setIngestParallelism(99);
    expect(useAppStore.getState().ingestParallelism).toEqual(previous);
    expect(useAppStore.getState().settingsError).toContain('got 99');
  });
});
