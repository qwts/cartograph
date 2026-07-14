import type { ReactNode } from 'react';

export interface StatusBarProps {
  /** Live status text (left side); animates while busy. */
  status: string;
  busy: boolean;
  /** Egress summary (right side, mono) — `Local-only · 0 bytes egress`
   *  unless a cloud tier is consented (fed by settings state once #118 lands). */
  egress: string;
  /** Extra left-side content (e.g. the core-version badge). */
  children?: ReactNode;
}

/** Status bar (30px): live status left, egress summary right (handoff §App Shell). */
export function StatusBar({ status, busy, egress, children }: StatusBarProps) {
  return (
    <footer className="status-bar">
      <span className="status-live" role="status">
        <span
          className={`material-symbols-outlined${busy ? ' spinning' : ''}`}
          aria-hidden="true"
        >
          {busy ? 'progress_activity' : 'check_circle'}
        </span>
        {status}
      </span>
      {children}
      <span className="status-egress">{egress}</span>
    </footer>
  );
}
