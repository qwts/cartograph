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
