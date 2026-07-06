import type { GraphNode } from '../store';
import { TierBadge } from './TierBadge';

export interface EndpointsCardProps {
  endpoints: GraphNode[];
  onSelect: (node: GraphNode) => void;
}

/** Recovered endpoints; selecting one opens its evidence (jump-to-source). */
export function EndpointsCard({ endpoints, onSelect }: EndpointsCardProps) {
  return (
    <section className="card">
      <h2>Endpoints</h2>
      {endpoints.length === 0 ? (
        <p className="muted">None recovered yet — ingest a repo with HTTP routes.</p>
      ) : (
        <ul className="endpoint-list">
          {endpoints.map((ep) => (
            <li key={ep.id}>
              <button type="button" className="endpoint-row" onClick={() => onSelect(ep)}>
                <span className="endpoint-method">{String(ep.props.method ?? '?')}</span>
                <span className="endpoint-path">{String(ep.props.path ?? ep.id)}</span>
                <TierBadge tier={ep.props.prov?.confidence_tier ?? 'Gap'} />
              </button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
