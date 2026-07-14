import type { IngestSource } from '../store';

export interface ConnectSurfaceProps {
  source: IngestSource;
  target: string;
  /** Disabled when there is no live backend to preflight against. */
  canPreflight: boolean;
  onSourceChange: (source: IngestSource) => void;
  onTargetChange: (target: string) => void;
  onBack: () => void;
  onPreflight: () => void;
}

const SOURCES: { id: IngestSource; label: string; icon: string; placeholder: string }[] = [
  {
    id: 'github',
    label: 'GitHub',
    icon: 'code',
    placeholder: 'https://github.com/owner/repo',
  },
  {
    id: 'local',
    label: 'Local folder',
    icon: 'folder',
    placeholder: '/path/to/project',
  },
  {
    id: 'manifest',
    label: 'System manifest',
    icon: 'description',
    placeholder: '/path/to/cartograph.system.toml',
  },
];

/** Step 1 of the ingest flow (handoff §Connect): pick a target. The local-only
 *  reassurance strip states the egress contract before any work starts. */
export function ConnectSurface({
  source,
  target,
  canPreflight,
  onSourceChange,
  onTargetChange,
  onBack,
  onPreflight,
}: ConnectSurfaceProps) {
  const active = SOURCES.find((s) => s.id === source) ?? SOURCES[0];
  return (
    <section className="ingest-flow" aria-label="Connect a target">
      <header className="ingest-hero">
        <h2>Connect a target</h2>
        <p className="muted">
          Point Cartograph at a system. It recovers structure, flows, and a provenance-tagged
          spec — everything runs on-device.
        </p>
      </header>
      <div className="source-picker" role="radiogroup" aria-label="Source kind">
        {SOURCES.map((s) => (
          <button
            key={s.id}
            type="button"
            role="radio"
            aria-checked={s.id === source}
            className={`source-option${s.id === source ? ' active' : ''}`}
            onClick={() => onSourceChange(s.id)}
          >
            <span className="material-symbols-outlined" aria-hidden="true">
              {s.icon}
            </span>
            {s.label}
          </button>
        ))}
      </div>
      <label className="target-field">
        <span className="target-label">Target</span>
        <input
          type="text"
          value={target}
          placeholder={active.placeholder}
          onChange={(e) => onTargetChange(e.target.value)}
        />
      </label>
      <p className="reassure-strip">
        <span className="material-symbols-outlined" aria-hidden="true">
          verified_user
        </span>
        Local-only preflight. Nothing leaves the device unless you opt a tier into cloud in
        Settings.
      </p>
      <footer className="flow-actions">
        <button type="button" className="secondary-button" onClick={onBack}>
          Back
        </button>
        <button
          type="button"
          onClick={onPreflight}
          disabled={!canPreflight || target.trim() === ''}
        >
          <span className="material-symbols-outlined" aria-hidden="true">
            arrow_forward
          </span>
          Preflight
        </button>
      </footer>
    </section>
  );
}
