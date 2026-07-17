import { describe, expect, it } from 'vitest';
import { useAppStore } from './store';

// Outside Tauri, invokeOr falls back — the store must degrade to a coherent
// "browser preview" state rather than erroring or faking backend data.
describe('app store outside Tauri', () => {
  it('starts unresolved', () => {
    expect(useAppStore.getState().backend).toBe('unknown');
  });

  it('refresh degrades to browser mode with empty data', async () => {
    await useAppStore.getState().refresh();
    const state = useAppStore.getState();
    expect(state.backend).toBe('browser');
    expect(state.stats).toBeNull();
    expect(state.jobs).toEqual([]);
  });
});

// #211 review: "View live" on a specific Jobs row must pin exactly that
// job — before this, Recover always showed "whichever job is running or
// queued," which could be a different job (a concurrent plugin gate,
// another recovery) than the one actually clicked.
describe('recoverJobId pinning', () => {
  it('viewRecoveryJob pins a specific job and navigates to Recover', () => {
    useAppStore.getState().viewRecoveryJob(42);
    const state = useAppStore.getState();
    expect(state.view).toBe('recover');
    expect(state.recoverJobId).toBe(42);
  });

  it('generic navigation forgets a pinned job', () => {
    useAppStore.getState().viewRecoveryJob(42);
    useAppStore.getState().setView('jobs');
    expect(useAppStore.getState().recoverJobId).toBeNull();
  });

  it('starting a fresh recovery never leaves a stale pin behind', async () => {
    // Simulate a pin left over from viewing a previous run's Jobs row.
    useAppStore.setState({ recoverJobId: 42 });
    await useAppStore.getState().startRecovery();
    // The new run's job id isn't known yet — must be the generic fallback,
    // never the old pinned id.
    expect(useAppStore.getState().recoverJobId).toBeNull();
  });
});
