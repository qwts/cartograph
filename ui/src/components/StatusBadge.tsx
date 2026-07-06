import type { AppStore } from '../store';

export interface StatusBadgeProps {
  backend: AppStore['backend'];
  version: string | null;
}

/** Backend liveness badge shown in the topbar. */
export function StatusBadge({ backend, version }: StatusBadgeProps) {
  if (backend === 'up') return <span className="badge up">core v{version}</span>;
  if (backend === 'browser') return <span className="badge browser">browser preview — no core</span>;
  return <span className="badge">connecting…</span>;
}
