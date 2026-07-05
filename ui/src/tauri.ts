import { invoke } from '@tauri-apps/api/core';

/** True when running inside the Tauri webview (vs. a browser tab or Node). */
export function inTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

/**
 * Invoke a Tauri command, or return `fallback` when running outside Tauri so
 * the UI stays viewable in a browser during development.
 */
export async function invokeOr<T>(
  command: string,
  fallback: T,
  args?: Record<string, unknown>,
): Promise<T> {
  if (!inTauri()) return fallback;
  return invoke<T>(command, args);
}
