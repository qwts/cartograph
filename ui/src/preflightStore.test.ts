import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { useAppStore, type PreflightReport } from './store';

const report = (detector: string): PreflightReport => ({
  languages: [],
  frameworks: [],
  unsupported: [],
  potential_gaps: [],
  detector,
});

const reset = {
  ingestSource: 'local' as const,
  ingestTarget: '/repos/app',
  preflight: null,
  preflightBusy: false,
  preflightError: null,
  preflightProgress: null,
};

beforeEach(() => {
  vi.stubGlobal('window', {});
  useAppStore.setState(reset);
});

afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
  useAppStore.setState(reset);
});

describe('preflight progress and cancellation (AC-0197/AC-0198, #235)', () => {
  it('shows progress only while running and clears it when the scan settles', async () => {
    let finish: (value: PreflightReport) => void = () => {};
    let run = -1;
    mockIPC((_command, args) => {
      run = (args as { run: number }).run;
      return new Promise<PreflightReport>((resolve) => { finish = resolve; });
    });
    const running = useAppStore.getState().runPreflight();
    const progress = { run, path: 'src/a.ts', done: 0, total: 2 };
    useAppStore.getState().applyPreflightProgress(progress);
    expect(useAppStore.getState().preflightProgress).toEqual(progress);

    finish(report('preflight@1'));
    await running;
    const state = useAppStore.getState();
    expect(state.preflight?.detector).toBe('preflight@1');
    expect(state.preflightBusy).toBe(false);
    expect(state.preflightProgress).toBeNull();

    useAppStore.getState().applyPreflightProgress(progress);
    expect(useAppStore.getState().preflightProgress).toBeNull();
  });

  it('reports a cancelled scan without a raw error and sends cancel_preflight', async () => {
    const commands: string[] = [];
    let reject: (reason: string) => void = () => {};
    mockIPC((command) => {
      commands.push(command);
      if (command === 'cancel_preflight') {
        reject('cancelled');
        return null;
      }
      return new Promise((_resolve, fail) => { reject = fail; });
    });
    const running = useAppStore.getState().runPreflight();
    await useAppStore.getState().cancelPreflight();
    await running;
    expect(commands).toEqual(['preflight', 'cancel_preflight']);
    const state = useAppStore.getState();
    expect(state.preflightError).toBe('Preflight cancelled. No findings were recorded.');
    expect(state.preflightBusy).toBe(false);
    expect(state.preflight).toBeNull();
  });

  it('never lets a superseded scan overwrite the current one', async () => {
    const pending: Array<{ resolve: (value: PreflightReport) => void; reject: (reason: string) => void }> = [];
    mockIPC(() => new Promise<PreflightReport>((resolve, reject) => { pending.push({ resolve, reject }); }));
    const first = useAppStore.getState().runPreflight();
    const second = useAppStore.getState().runPreflight();

    pending[1].resolve(report('current'));
    await second;
    pending[0].reject('cancelled');
    await first;

    const state = useAppStore.getState();
    expect(state.preflight?.detector).toBe('current');
    expect(state.preflightError).toBeNull();
    expect(state.preflightBusy).toBe(false);
  });

  it('cancels a running local scan when the source switches to GitHub', async () => {
    const commands: string[] = [];
    let reject: (reason: string) => void = () => {};
    mockIPC((command) => {
      commands.push(command);
      if (command === 'cancel_preflight') {
        reject('cancelled');
        return null;
      }
      return new Promise((_resolve, fail) => { reject = fail; });
    });
    const local = useAppStore.getState().runPreflight();
    // The Connect screen's source picker — no second Preflight click.
    useAppStore.getState().setIngestSource('github');
    await local;

    expect(commands).toEqual(['preflight', 'cancel_preflight']);
    const state = useAppStore.getState();
    expect(state.ingestSource).toBe('github');
    expect(state.preflightBusy).toBe(false);
    expect(state.preflight).toBeNull();

    // Switching while idle sends nothing.
    useAppStore.getState().setIngestSource('manifest');
    expect(commands).toEqual(['preflight', 'cancel_preflight']);
  });

  it("ignores a superseded scan's late progress ping (#434 review)", async () => {
    const runs: number[] = [];
    const pending: Array<(value: PreflightReport) => void> = [];
    mockIPC((_command, args) => {
      runs.push((args as { run: number }).run);
      return new Promise<PreflightReport>((resolve) => { pending.push(resolve); });
    });
    const first = useAppStore.getState().runPreflight();
    const second = useAppStore.getState().runPreflight();
    expect(runs[1]).toBeGreaterThan(runs[0]);

    const current = { run: runs[1], path: 'new/a.ts', done: 0, total: 9 };
    useAppStore.getState().applyPreflightProgress(current);
    useAppStore.getState().applyPreflightProgress({ run: runs[0], path: 'old/z.ts', done: 5, total: 6 });
    expect(useAppStore.getState().preflightProgress).toEqual(current);

    pending[0](report('old'));
    pending[1](report('new'));
    await Promise.all([first, second]);
  });
});
