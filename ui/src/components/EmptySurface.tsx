export interface EmptySurfaceProps {
  icon: string;
  title: string;
  /** Honest interim copy: what this surface will show and what it waits on. */
  description: string;
}

/** Placeholder for surfaces whose implementation lands in a later issue —
 *  a real, styled empty state so no route is ever dead or blank. */
export function EmptySurface({ icon, title, description }: EmptySurfaceProps) {
  return (
    <section className="empty-surface">
      <span className="material-symbols-outlined" aria-hidden="true">
        {icon}
      </span>
      <h2>{title}</h2>
      <p>{description}</p>
    </section>
  );
}
