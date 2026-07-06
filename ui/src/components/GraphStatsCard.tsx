import type { GraphStats } from '../store';

export interface GraphStatsCardProps {
  stats: GraphStats | null;
}

/** Node/edge counts of the unified graph; renders dashes with no backend. */
export function GraphStatsCard({ stats }: GraphStatsCardProps) {
  return (
    <section className="card">
      <h2>Unified graph</h2>
      <div className="stat">{stats ? stats.nodes : '—'}</div>
      <div className="stat-label">nodes</div>
      <div className="stat">{stats ? stats.edges : '—'}</div>
      <div className="stat-label">edges</div>
    </section>
  );
}
