import { SURFACES, type SurfaceView } from '../views';

export interface NavRailProps {
  active: SurfaceView;
  onNavigate: (view: SurfaceView) => void;
  onOpenPalette: () => void;
}

/** Left rail (54px): logo tile, the eight surfaces, and the ⌘K launcher.
 *  Icon-only controls carry `title` + `aria-label` (handoff §App Shell). */
export function NavRail({ active, onNavigate, onOpenPalette }: NavRailProps) {
  return (
    <nav className="nav-rail" aria-label="Primary surfaces">
      <div className="nav-logo" aria-hidden="true" />
      {SURFACES.map((surface) => (
        <button
          key={surface.id}
          type="button"
          className={`nav-btn${surface.id === active ? ' active' : ''}`}
          title={surface.label}
          aria-label={surface.label}
          aria-current={surface.id === active ? 'page' : undefined}
          onClick={() => onNavigate(surface.id)}
        >
          <span className="material-symbols-outlined" aria-hidden="true">
            {surface.icon}
          </span>
        </button>
      ))}
      <button
        type="button"
        className="nav-btn nav-palette"
        title="Command palette (⌘K)"
        aria-label="Command palette"
        onClick={onOpenPalette}
      >
        <span className="material-symbols-outlined" aria-hidden="true">
          keyboard_command_key
        </span>
      </button>
    </nav>
  );
}
