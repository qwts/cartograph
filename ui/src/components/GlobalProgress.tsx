export interface GlobalProgressProps {
  /** True while any long job runs. Long work is non-blocking — this thin
   *  bar is the only global indicator, never a modal spinner (handoff #5). */
  active: boolean;
}

/** 2px indeterminate accent bar pinned to the top edge while work runs. */
export function GlobalProgress({ active }: GlobalProgressProps) {
  if (!active) return null;
  return (
    <div className="global-progress" role="progressbar" aria-label="Background work running">
      <div className="global-progress-bar" />
    </div>
  );
}
