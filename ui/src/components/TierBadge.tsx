import type { Tier } from '../store';

const LABEL: Record<Tier, string> = {
  Confirmed: 'Confirmed',
  InferredStrong: 'Inferred (strong)',
  InferredWeak: 'Inferred (weak)',
  Gap: 'Gap',
};

/**
 * Confidence-tier badge (R-INT-2): inferred content must never be visually
 * indistinguishable from confirmed. Colors come from docs/design/DESIGN.md.
 */
export function TierBadge({ tier }: { tier: Tier }) {
  return <span className={`tier-badge tier-${tier.toLowerCase()}`}>{LABEL[tier]}</span>;
}
