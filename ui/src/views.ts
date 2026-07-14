/** The eight primary surfaces of the IDE shell (design handoff §App Shell).
 *  Single source of truth for the nav rail, command palette, and ⌘1–⌘8
 *  shortcuts — order here IS the shortcut order. */

export type SurfaceView =
  | 'workspace'
  | 'atlas'
  | 'flows'
  | 'spec'
  | 'gaps'
  | 'prov'
  | 'jobs'
  | 'settings'
  // The ingest flow (handoff §Screens 1–3): routed views, not rail surfaces.
  | 'connect'
  | 'preflight'
  | 'recover';

export interface SurfaceDef {
  id: SurfaceView;
  label: string;
  /** Material Symbols Outlined glyph name. */
  icon: string;
  /** One-line purpose shown in the command palette. */
  hint: string;
}

export const SURFACES: readonly SurfaceDef[] = [
  {
    id: 'workspace',
    label: 'Workspace',
    icon: 'space_dashboard',
    hint: 'Recovery outcome and artifacts',
  },
  { id: 'atlas', label: 'Atlas', icon: 'map', hint: 'System topology graph' },
  { id: 'flows', label: 'Flows', icon: 'account_tree', hint: 'Business-flow inspector' },
  {
    id: 'spec',
    label: 'Spec Workbench',
    icon: 'description',
    hint: 'Read and curate the recovered spec',
  },
  { id: 'gaps', label: 'Gaps & Drift', icon: 'report', hint: 'Open findings register' },
  {
    id: 'prov',
    label: 'Provenance & Eval',
    icon: 'analytics',
    hint: 'Tier distribution and quality gates',
  },
  { id: 'jobs', label: 'Jobs', icon: 'terminal', hint: 'Durable background work' },
  { id: 'settings', label: 'Settings', icon: 'settings', hint: 'Tiers, providers, egress' },
] as const;

/** Breadcrumb labels for the ingest-flow views (not on the rail/palette). */
const INGEST_LABELS: Partial<Record<SurfaceView, string>> = {
  connect: 'Connect',
  preflight: 'Preflight',
  recover: 'Recover',
};

export function surfaceLabel(view: SurfaceView): string {
  return (
    INGEST_LABELS[view] ?? SURFACES.find((surface) => surface.id === view)?.label ?? view
  );
}

/** The rail surface to highlight for a view (ingest flow lights Workspace). */
export function railSurface(view: SurfaceView): SurfaceView {
  return view in INGEST_LABELS ? 'workspace' : view;
}
