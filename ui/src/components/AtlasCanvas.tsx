import cytoscape from 'cytoscape';
import { useEffect, useMemo, useRef, useState } from 'react';
import type { AtlasSnapshot, GraphEdge, GraphNode, Tier } from '../store';

export type AtlasLayer = 'all' | 'infra' | 'cloud' | 'server' | 'events' | 'client';
type GraphLayer = Exclude<AtlasLayer, 'all'>;

const LAYERS: ReadonlyArray<{ id: AtlasLayer; label: string }> = [
  { id: 'all', label: 'All layers' },
  { id: 'infra', label: 'Infrastructure' },
  { id: 'cloud', label: 'Cloud' },
  { id: 'server', label: 'Server' },
  { id: 'events', label: 'Events' },
  { id: 'client', label: 'Client' },
];

const TIERS: ReadonlyArray<{ id: Tier; label: string }> = [
  { id: 'Confirmed', label: 'Confirmed' },
  { id: 'InferredStrong', label: 'Inferred strong' },
  { id: 'InferredWeak', label: 'Inferred weak' },
  { id: 'Gap', label: 'Gap' },
];

function baseLayers(node: GraphNode): GraphLayer[] {
  switch (node.label) {
    case 'Resource':
      return ['infra', 'cloud'];
    case 'Channel':
      return ['events', 'cloud'];
    case 'Screen':
    case 'Component':
      return ['client'];
    case 'Module':
    case 'File':
    case 'Symbol':
    case 'Endpoint':
    case 'DataEntity':
    case 'Config':
      return ['server'];
    default:
      return [];
  }
}

function edgeLayers(edge: GraphEdge): GraphLayer[] {
  switch (edge.label) {
    case 'DEPENDS_ON':
    case 'REFERENCES':
      return ['infra'];
    case 'TRIGGERS':
    case 'ROUTES':
    case 'GRANTS':
      return ['cloud'];
    case 'BACKS':
      return ['cloud', 'events'];
    case 'PUBLISHES':
    case 'SUBSCRIBES':
      return ['events'];
    case 'RENDERS':
    case 'FETCHES':
      return ['client'];
    case 'DEFINED_IN':
    case 'IMPORTS':
    case 'CALLS':
    case 'HANDLES':
    case 'READS':
    case 'WRITES':
      return ['server'];
    default:
      return [];
  }
}

/** Pure O(N+E) layer projection used before handing facts to Cytoscape. */
export function filterAtlasGraph(snapshot: AtlasSnapshot, layer: AtlasLayer): AtlasSnapshot {
  if (layer === 'all') return snapshot;

  const edges = snapshot.edges.filter((edge) => edgeLayers(edge).includes(layer));
  const visibleIds = new Set(
    snapshot.nodes.filter((node) => baseLayers(node).includes(layer)).map((node) => node.id),
  );
  for (const edge of edges) {
    visibleIds.add(edge.src);
    visibleIds.add(edge.dst);
  }

  const nodes = snapshot.nodes.filter((node) => visibleIds.has(node.id));
  const existingIds = new Set(nodes.map((node) => node.id));
  return {
    nodes,
    edges: edges.filter((edge) => existingIds.has(edge.src) && existingIds.has(edge.dst)),
  };
}

function confidence(props: GraphNode['props'] | GraphEdge['props']): Tier {
  const tier = props.prov?.confidence_tier;
  return tier === 'Confirmed' ||
    tier === 'InferredStrong' ||
    tier === 'InferredWeak' ||
    tier === 'Gap'
    ? tier
    : 'Gap';
}

function displayName(node: GraphNode): string {
  const method = typeof node.props.method === 'string' ? node.props.method : null;
  const path = typeof node.props.path === 'string' ? node.props.path : null;
  if (method && path) return `${method} ${path}`;
  for (const key of ['name', 'identity', 'logical_id', 'type']) {
    const value = node.props[key];
    if (typeof value === 'string' && value.length > 0) return value;
  }
  return node.id;
}

function elementsFor(snapshot: AtlasSnapshot, overlay: boolean): cytoscape.ElementDefinition[] {
  const nodeElements = snapshot.nodes.map((node) => {
    const tier = confidence(node.props);
    return {
      data: { id: node.id, label: displayName(node), kind: node.label, tier },
      classes: `${overlay ? `tier-${tier.toLowerCase()}` : 'tier-neutral'} ${node.label === 'Gap' ? 'atlas-gap' : ''}`,
    };
  });
  const edgeElements = snapshot.edges.map((edge) => {
    const tier = confidence(edge.props);
    return {
      data: {
        id: `${edge.src}\u0000${edge.label}\u0000${edge.dst}`,
        source: edge.src,
        target: edge.dst,
        label: edge.label,
        tier,
      },
      classes: overlay ? `tier-${tier.toLowerCase()}` : 'tier-neutral',
    };
  });
  return [...nodeElements, ...edgeElements];
}

const CY_STYLE: cytoscape.StylesheetStyle[] = [
  {
    selector: 'node',
    style: {
      width: 34,
      height: 34,
      label: 'data(label)',
      'font-size': 9,
      color: '#e5e2e1',
      'text-background-color': '#131313',
      'text-background-opacity': 0.88,
      'text-background-padding': '3px',
      'text-valign': 'bottom',
      'text-margin-y': 7,
      'border-width': 2,
      'background-color': '#2a2a2a',
      'border-color': '#8b919f',
    },
  },
  {
    selector: 'edge',
    style: {
      width: 1.5,
      label: 'data(label)',
      'font-size': 7,
      color: '#888888',
      'curve-style': 'bezier',
      'target-arrow-shape': 'triangle',
      'line-color': '#414753',
      'target-arrow-color': '#414753',
      'text-rotation': 'autorotate',
      'text-background-color': '#131313',
      'text-background-opacity': 0.8,
      'text-background-padding': '2px',
    },
  },
  {
    selector: '.tier-confirmed',
    style: { 'background-color': '#27c93f', 'line-color': '#27c93f', 'target-arrow-color': '#27c93f' },
  },
  {
    selector: '.tier-inferredstrong',
    style: { 'background-color': '#2d9cdb', 'line-color': '#2d9cdb', 'target-arrow-color': '#2d9cdb' },
  },
  {
    selector: '.tier-inferredweak',
    style: { 'background-color': '#f2c94c', 'line-color': '#f2c94c', 'target-arrow-color': '#f2c94c' },
  },
  {
    selector: '.tier-gap',
    style: {
      'background-color': '#131313',
      'border-color': '#eb5757',
      'line-color': '#eb5757',
      'target-arrow-color': '#eb5757',
      'line-style': 'dashed',
    },
  },
  {
    selector: '.atlas-gap',
    style: { shape: 'diamond', 'border-style': 'dashed', 'border-width': 3 },
  },
  {
    selector: ':selected',
    style: { 'border-color': '#abc7ff', 'border-width': 4 },
  },
];

export interface AtlasCanvasProps {
  snapshot: AtlasSnapshot;
  onSelect: (node: GraphNode) => void;
}

/** Unified read-only graph with layer and confidence projections (US-0010). */
export function AtlasCanvas({ snapshot, onSelect }: AtlasCanvasProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const onSelectRef = useRef(onSelect);
  const [layer, setLayer] = useState<AtlasLayer>('all');
  const [overlay, setOverlay] = useState(true);
  const visible = useMemo(() => filterAtlasGraph(snapshot, layer), [snapshot, layer]);

  useEffect(() => {
    onSelectRef.current = onSelect;
  }, [onSelect]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container || visible.nodes.length === 0) return;
    const byId = new Map(visible.nodes.map((node) => [node.id, node]));
    const cy = cytoscape({
      container,
      elements: elementsFor(visible, overlay),
      style: CY_STYLE,
      layout: { name: 'grid', fit: true, padding: 36, avoidOverlap: true },
      minZoom: 0.08,
      maxZoom: 4,
    });
    cy.on('tap', 'node', (event) => {
      const node = byId.get(event.target.id());
      if (node) onSelectRef.current(node);
    });
    return () => cy.destroy();
  }, [visible, overlay]);

  return (
    <section className="card atlas-card" aria-labelledby="atlas-title">
      <div className="atlas-heading">
        <div>
          <h2 id="atlas-title">Atlas · unified graph</h2>
          <p className="muted">Five layers, one provenance-bearing graph. Select any entity for read-only evidence.</p>
        </div>
        <button
          type="button"
          className={`atlas-overlay-toggle ${overlay ? 'active' : ''}`}
          aria-pressed={overlay}
          onClick={() => setOverlay((value) => !value)}
        >
          Confidence overlay
        </button>
      </div>

      <div className="atlas-toolbar" aria-label="Atlas layer filters">
        {LAYERS.map((item) => (
          <button
            key={item.id}
            type="button"
            className={layer === item.id ? 'active' : ''}
            aria-pressed={layer === item.id}
            onClick={() => setLayer(item.id)}
          >
            {item.label}
          </button>
        ))}
      </div>

      <div className="atlas-legend" aria-label="Confidence legend">
        {TIERS.map((tier) => (
          <span key={tier.id} className={`atlas-legend-${tier.id.toLowerCase()}`}>
            {tier.label}
          </span>
        ))}
        <strong role="status">
          {visible.nodes.length} nodes · {visible.edges.length} edges
        </strong>
      </div>

      <div className="atlas-surface">
        {visible.nodes.length === 0 ? (
          <p className="muted atlas-empty">No graph facts in this layer.</p>
        ) : (
          <div
            ref={containerRef}
            className="atlas-cytoscape"
            data-testid="atlas-canvas"
            aria-label={`${LAYERS.find((item) => item.id === layer)?.label ?? layer} graph canvas`}
          />
        )}
      </div>

      {visible.nodes.length > 0 && (
        <div className="atlas-entity-index">
          <span className="muted">Visible entities</span>
          <div>
            {visible.nodes.slice(0, 24).map((node) => (
              <button key={node.id} type="button" onClick={() => onSelect(node)}>
                {displayName(node)}
              </button>
            ))}
            {visible.nodes.length > 24 && <span className="muted">+{visible.nodes.length - 24} more on canvas</span>}
          </div>
        </div>
      )}
    </section>
  );
}
