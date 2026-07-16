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
  /** Jump back to the live Recovering screen (#209); only set while a real
   *  ingest is running, so the status text is only clickable when there's
   *  actually somewhere to go back to. */
  onOpenRecovery?: () => void;
}

/** Status bar (30px): live status left, egress summary right (handoff §App Shell). */
export function StatusBar({ status, busy, egress, children, onOpenRecovery }: StatusBarProps) {
  const icon = (
    <span className={`material-symbols-outlined${busy ? ' spinning' : ''}`} aria-hidden="true">
      {busy ? 'progress_activity' : 'check_circle'}
    </span>
  );
  return (
    <footer className="status-bar">
      {onOpenRecovery ? (
        <button type="button" className="status-live status-live-link" onClick={onOpenRecovery}>
          {icon}
          <span role="status">{status}</span>
        </button>
      ) : (
        <span className="status-live" role="status">
          {icon}
          {status}
        </span>
      )}
      {children}
      <span className="status-egress">{egress}</span>
    </footer>
  );
}
