import { useEffect, useRef, useState } from 'react';
import { SURFACES, type SurfaceView } from '../views';

export interface CommandPaletteProps {
  open: boolean;
  onClose: () => void;
  onNavigate: (view: SurfaceView) => void;
}

const isMac = typeof navigator !== 'undefined' && /Mac/.test(navigator.platform);
const MOD = isMac ? '⌘' : 'Ctrl+';

/** ⌘K palette: centered 520px overlay listing all eight surfaces with icon,
 *  hint, and shortcut tag. Esc closes; arrows + Enter are keyboard-first
 *  (handoff §App Shell). */
export function CommandPalette({ open, onClose, onNavigate }: CommandPaletteProps) {
  // Mount the content per open so cursor state starts fresh each time.
  if (!open) return null;
  return <PaletteContent onClose={onClose} onNavigate={onNavigate} />;
}

function PaletteContent({ onClose, onNavigate }: Omit<CommandPaletteProps, 'open'>) {
  const [cursor, setCursor] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    listRef.current?.focus();
  }, []);

  const go = (view: SurfaceView) => {
    onNavigate(view);
    onClose();
  };

  const onKeyDown = (event: React.KeyboardEvent) => {
    if (event.key === 'Escape') {
      event.stopPropagation();
      onClose();
    } else if (event.key === 'ArrowDown') {
      event.preventDefault();
      setCursor((c) => (c + 1) % SURFACES.length);
    } else if (event.key === 'ArrowUp') {
      event.preventDefault();
      setCursor((c) => (c + SURFACES.length - 1) % SURFACES.length);
    } else if (event.key === 'Enter') {
      event.preventDefault();
      go(SURFACES[cursor].id);
    }
  };

  return (
    <div className="cmdk-backdrop" role="presentation" onClick={onClose}>
      <div
        className="cmdk"
        role="dialog"
        aria-modal="true"
        aria-label="Command palette"
        onClick={(event) => event.stopPropagation()}
      >
        <p className="cmdk-title">Go to surface</p>
        <div
          ref={listRef}
          className="cmdk-list"
          role="listbox"
          aria-label="Surfaces"
          aria-activedescendant={`cmdk-${SURFACES[cursor].id}`}
          tabIndex={0}
          onKeyDown={onKeyDown}
        >
          {SURFACES.map((surface, index) => (
            <div
              key={surface.id}
              id={`cmdk-${surface.id}`}
              role="option"
              aria-selected={index === cursor}
              className={`cmdk-row${index === cursor ? ' cursor' : ''}`}
              onMouseEnter={() => setCursor(index)}
              onClick={() => go(surface.id)}
            >
              <span className="material-symbols-outlined" aria-hidden="true">
                {surface.icon}
              </span>
              <span className="cmdk-label">{surface.label}</span>
              <span className="cmdk-hint">{surface.hint}</span>
              <kbd>{`${MOD}${index + 1}`}</kbd>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
