import { describe, expect, it } from 'vitest';
import { isEscalatable } from './gapClasses';
import type { SpecAssertion } from './store';

const assertion = (overrides: Partial<SpecAssertion> = {}): SpecAssertion => ({
  id: 'node:gap:orders',
  subject_id: 'gap:orders',
  subject_kind: 'Gap',
  summary: 'Gap: unresolved',
  provenance: {
    tier: 'Static',
    confidence_tier: 'Gap',
    evidence: [],
    extractor_id: 'ts',
    content_hash: 'h'.repeat(64),
  },
  ...overrides,
});

describe('isEscalatable (AC-0231, #238)', () => {
  it('offers escalation for a relation the broker allows', () => {
    const gap = assertion({ edge_label: 'PUBLISHES' });
    expect(isEscalatable(gap, ['PUBLISHES', 'CALLS'])).toBe(true);
  });

  it('withholds the offer for a relation outside the broker allowlist', () => {
    const gap = assertion({ edge_label: 'IMPORTS' });
    expect(isEscalatable(gap, ['PUBLISHES', 'CALLS'])).toBe(false);
  });

  it('stays optimistic while capability has not loaded yet', () => {
    const gap = assertion({ edge_label: 'IMPORTS' });
    expect(isEscalatable(gap, null)).toBe(true);
  });

  it('stays optimistic for a Gap node whose eventual relation is not yet known', () => {
    const gap = assertion({ edge_label: null });
    expect(isEscalatable(gap, ['PUBLISHES', 'CALLS'])).toBe(true);
    const withoutField = assertion();
    delete (withoutField as { edge_label?: string | null }).edge_label;
    expect(isEscalatable(withoutField, ['PUBLISHES', 'CALLS'])).toBe(true);
  });

  it('checks an edge-shaped gap by its own edge_label, matching subject_kind', () => {
    const edgeGap = assertion({
      id: 'edge:a->b:IMPORTS',
      subject_kind: 'IMPORTS',
      edge_label: 'IMPORTS',
    });
    expect(isEscalatable(edgeGap, ['PUBLISHES', 'CALLS'])).toBe(false);
    expect(isEscalatable(edgeGap, ['IMPORTS'])).toBe(true);
  });
});
