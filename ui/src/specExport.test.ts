import { describe, expect, it } from 'vitest';
import { resolveArtifactForCopy, withFullArtifacts } from './specExport';
import type { SpecArtifact } from './store';

const artifact = (id: string, overrides: Partial<SpecArtifact> = {}): SpecArtifact => ({
  id, file_name: `${id}.md`, title: id, format: 'markdown', content: `${id} preview`,
  content_truncated: false, content_byte_len: 7, assertions: [], assertions_truncated: false,
  assertions_total: 0, ...overrides,
});

describe('withFullArtifacts aborts on a failed on-demand fetch (PR #533 review thread)', () => {
  it('passes untruncated artifacts through without calling readSpecArtifact', async () => {
    const whole = artifact('whole');
    const calls: string[] = [];
    const result = await withFullArtifacts([whole], async (id) => {
      calls.push(id);
      return artifact(id);
    });
    expect(result).toEqual([whole]);
    expect(calls).toEqual([]);
  });

  it('replaces a truncated artifact with its fetched full content on success', async () => {
    const capped = artifact('gap-register', { content_truncated: true, content: 'capped preview' });
    const full = artifact('gap-register', { content: 'the complete register' });
    const result = await withFullArtifacts([capped], async () => full);
    expect(result).toEqual([full]);
  });

  it('aborts the whole export (returns null) when any truncated artifact fetch fails, instead of falling back to the capped preview', async () => {
    const ok = artifact('drift-register', { content_truncated: true });
    const failing = artifact('gap-register', { assertions_truncated: true });
    const result = await withFullArtifacts([ok, failing], async (id) =>
      id === 'gap-register' ? null : artifact(id),
    );
    expect(result).toBeNull();
  });
});

describe('resolveArtifactForCopy aborts on a failed on-demand fetch (PR #533 review thread)', () => {
  it('returns the artifact as-is when its content was never truncated', async () => {
    const whole = artifact('whole');
    const result = await resolveArtifactForCopy(whole, async () => {
      throw new Error('should not be called');
    });
    expect(result).toBe(whole);
  });

  it('returns the fetched full artifact when the preview was truncated', async () => {
    const capped = artifact('gap-register', { content_truncated: true, content: 'capped preview' });
    const full = artifact('gap-register', { content: 'the complete register' });
    const result = await resolveArtifactForCopy(capped, async () => full);
    expect(result).toBe(full);
  });

  it('returns null instead of the capped preview when the on-demand fetch fails', async () => {
    const capped = artifact('gap-register', { content_truncated: true, content: 'capped preview' });
    const result = await resolveArtifactForCopy(capped, async () => null);
    expect(result).toBeNull();
  });
});
