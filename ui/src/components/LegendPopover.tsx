import { TIER_LABELS } from './TierBadge';
import type { Tier } from '../store';

export interface LegendPopoverProps {
  open: boolean;
  onClose: () => void;
}

const TIER_ROWS: Array<{ tier: Tier; code: string; method: string }> = [
  { tier: 'Confirmed', code: 'T0/T1', method: 'Static parse / observed evidence' },
  { tier: 'InferredStrong', code: 'T2', method: 'Local semantic matching, eval-gated' },
  { tier: 'InferredWeak', code: 'T3', method: 'Bounded agent proposal, human-accepted' },
  { tier: 'Gap', code: 'GAP', method: 'Deterministic recovery stopped — explicit' },
];

/** The canonical non-color-alone key: tier codes, node shapes, and edge line
 *  treatments (handoff §Overlays / Legend popover). */
export function LegendPopover({ open, onClose }: LegendPopoverProps) {
  if (!open) return null;
  return (
    <div className="legend-backdrop" role="presentation" onClick={onClose}>
      <div
        className="legend-popover"
        role="dialog"
        aria-modal="true"
        aria-label="Tier, shape, and edge legend"
        onClick={(event) => event.stopPropagation()}
      >
        <header className="legend-header">
          <h2>Legend</h2>
          <button type="button" aria-label="Close legend" onClick={onClose}>
            <span className="material-symbols-outlined" aria-hidden="true">
              close
            </span>
          </button>
        </header>

        <h3>Confidence tiers</h3>
        <table className="legend-table">
          <tbody>
            {TIER_ROWS.map(({ tier, code, method }) => (
              <tr key={code}>
                <td>
                  <code className={`legend-code tier-${tier.toLowerCase()}`}>{code}</code>
                </td>
                <td>{TIER_LABELS[tier]}</td>
                <td className="legend-method">{method}</td>
              </tr>
            ))}
          </tbody>
        </table>

        <h3>Node shapes</h3>
        <ul className="legend-shapes">
          <li>
            <span className="legend-shape shape-rect" aria-hidden="true" /> Rectangle — service /
            component
          </li>
          <li>
            <span className="legend-shape shape-diamond" aria-hidden="true" /> Diamond — gateway /
            channel
          </li>
          <li>
            <span className="legend-shape shape-octagon" aria-hidden="true" /> Octagon (dashed red)
            — Gap
          </li>
        </ul>

        <h3>Edges</h3>
        <ul className="legend-edges">
          <li>
            <span className="legend-edge edge-solid" aria-hidden="true" /> Solid — confirmed
            relation
          </li>
          <li>
            <span className="legend-edge edge-dashed" aria-hidden="true" /> Dashed — inferred or
            gap relation (animated)
          </li>
        </ul>
      </div>
    </div>
  );
}
