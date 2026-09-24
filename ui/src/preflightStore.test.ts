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

  // AC-0198 (#454): editing the local path abandons the running scan.
  it('cancels and retires a running local scan when the local path changes', async () => {
    const commands: string[] = [];
    let resolve: (value: PreflightReport) => void = () => {};
    let run = -1;
    mockIPC((command, args) => {
      commands.push(command);
      if (command === 'cancel_preflight') return null;
      run = (args as { run: number }).run;
      return new Promise<PreflightReport>((done) => { resolve = done; });
    });
    const local = useAppStore.getState().runPreflight();
    // A whitespace-only edit doesn't change what is being scanned.
    useAppStore.getState().setIngestTarget(' /repos/app ');
    expect(commands).toEqual(['preflight']);
    expect(useAppStore.getState().preflightBusy).toBe(true);

    // Back to Connect during the scan, then a different path.
    useAppStore.getState().setIngestTarget('/repos/other');
    expect(commands).toEqual(['preflight', 'cancel_preflight']);
    let state = useAppStore.getState();
    expect(state.ingestTarget).toBe('/repos/other');
    expect(state.preflightBusy).toBe(false);
    expect(state.preflightProgress).toBeNull();

    // The abandoned run's late ping and result (a cancel that lost the
    // persist race) never reach the store.
    useAppStore.getState().applyPreflightProgress({ run, path: 'old/a.ts', done: 1, total: 2 });
    resolve(report('old-path'));
    await local;
    state = useAppStore.getState();
    expect(state.preflight).toBeNull();
    expect(state.preflightProgress).toBeNull();
    expect(state.preflightBusy).toBe(false);

    // Editing while idle sends nothing.
    useAppStore.getState().setIngestTarget('/repos/third');
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

  it('shows the reconciled report once a local recovery completes (AC-0200, #243)', async () => {
    const reconciled = report('preflight@1');
    reconciled.potential_gaps = [
      {
        kind: 'inline-eval',
        path: 'app.ts',
        line: 4,
        message: 'const-shaped',
        detector: 'preflight@1',
        request_adapter: null,
      },
    ];
    mockIPC((command) =>
      command === 'ingest_path'
        ? { job_id: 1, files: 1, nodes: 0, edges: 0, layers: {}, preflight: reconciled }
        : null,
    );
    useAppStore.setState({ preflight: report('pending'), preflightError: 'cancelled earlier' });
    await useAppStore.getState().ingest('/repos/app', 'local');

    const state = useAppStore.getState();
    expect(state.preflight).toEqual(reconciled);
    expect(state.preflightError).toBeNull();
  });

  it("keeps a newer preflight's report when an older recovery settles late (#439 review)", async () => {
    let settleRecovery: (summary: unknown) => void = () => {};
    mockIPC((command) =>
      command === 'ingest_path'
        ? new Promise((resolve) => { settleRecovery = resolve; })
        : command === 'preflight'
          ? report('repo-b')
          : null,
    );
    const recovery = useAppStore.getState().ingest('/repos/a', 'local');
    useAppStore.setState({ ingestSource: 'local', ingestTarget: '/repos/b' });
    await useAppStore.getState().runPreflight();
    expect(useAppStore.getState().preflight?.detector).toBe('repo-b');

    settleRecovery({ job_id: 1, files: 1, nodes: 0, edges: 0, layers: {}, preflight: report('repo-a') });
    await recovery;
    expect(useAppStore.getState().preflight?.detector).toBe('repo-b');
  });
});
