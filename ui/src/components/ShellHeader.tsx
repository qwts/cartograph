export type ScopeKind = 'system' | 'trail' | 'layer';

export interface Scope {
  kind: ScopeKind;
  label: string;
}

export interface ShellHeaderProps {
  /** Ingested system name, or null before any ingest. */
  system: string | null;
  /** Active surface label (breadcrumb tail). */
  surface: string;
  /** Context chip: whole system / single evidence trail / atlas layer. */
  scope: Scope;
  onShowLegend: () => void;
}

const SCOPE_ICON: Record<ScopeKind, string> = {
  system: 'public',
  trail: 'my_location',
  layer: 'filter_alt',
};

/** Header (50px): breadcrumb, Legend, and the scope chip (handoff §App Shell). */
export function ShellHeader({ system, surface, scope, onShowLegend }: ShellHeaderProps) {
  return (
    <header className="shell-header">
      <nav className="breadcrumb" aria-label="Breadcrumb">
        <span className="breadcrumb-system">{system ?? 'No system'}</span>
        <span className="breadcrumb-sep" aria-hidden="true">
          ›
        </span>
        <span className="breadcrumb-surface" aria-current="page">
          {surface}
        </span>
      </nav>
      <div className="shell-header-actions">
        <button type="button" className="legend-button" onClick={onShowLegend}>
          <span className="material-symbols-outlined" aria-hidden="true">
            style
          </span>
          Legend
        </button>
        <span className={`scope-chip scope-${scope.kind}`}>
          <span className="material-symbols-outlined" aria-hidden="true">
            {SCOPE_ICON[scope.kind]}
          </span>
          {scope.label}
        </span>
      </div>
    </header>
  );
}
