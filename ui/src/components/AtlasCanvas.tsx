import cytoscape from 'cytoscape';
import { useEffect, useMemo, useRef, useState } from 'react';
import { BAND_LABELS, buildAtlasScene, type AtlasScene } from '../atlasLayout';
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

/** Ego projection for focus mode (#160): the root, every node one edge
 * away in the active projection, and the edges among that kept set — a
 * deterministic function of (snapshot, root). A root that is not in the
 * snapshot projects nothing away (fail open to the unfocused view). */
export function focusAtlasGraph(snapshot: AtlasSnapshot, root: string): AtlasSnapshot {
  if (!snapshot.nodes.some((node) => node.id === root)) return snapshot;
  const keep = new Set([root]);
  for (const edge of snapshot.edges) {
    if (edge.src === root) keep.add(edge.dst);
    if (edge.dst === root) keep.add(edge.src);
  }
  return {
    nodes: snapshot.nodes.filter((node) => keep.has(node.id)),
    edges: snapshot.edges.filter((edge) => keep.has(edge.src) && keep.has(edge.dst)),
  };
}

/** Mono chip prefix per producing tier (handoff: `T0 TRIGGERS`, `GAP …`). */
function tierCode(props: GraphNode['props'] | GraphEdge['props']): string {
  if (confidence(props) === 'Gap') return 'GAP';
  switch (props.prov?.tier) {
    case 'Dynamic':
      return 'T1';
    case 'Semantic':
      return 'T2';
    case 'Agentic':
      return 'T3';
    default:
      return 'T0';
  }
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

/** Shape encodes kind, never color alone (handoff §Atlas): octagon with a
 *  dashed red border = Gap, diamond = gateway/channel, rectangle = the
 *  rest. Exported so the mapping itself is pinned by tests. */
export function nodeShapeClass(node: GraphNode): 'atlas-gap' | 'kind-channel' | 'kind-box' {
  if (node.label === 'Gap') return 'atlas-gap';
  if (node.label === 'Channel' || node.label === 'Gateway') return 'kind-channel';
  return 'kind-box';
}

/** Scene → Cytoscape elements: band compounds, real nodes at deterministic
 *  banded positions, collapsed-cluster proxies, and edges (real ones keep
 *  the tier chip; aggregated ones carry a relation count). */
function elementsFor(scene: AtlasScene, overlay: boolean): cytoscape.ElementDefinition[] {
  const bandParents = scene.bands.map(({ band, label, count }) => ({
    data: { id: `band:${band}`, label: `${label} · ${count}` },
    classes: 'atlas-band',
  }));
  const nodeElements = scene.nodes.map((item) => {
    if (item.cluster) {
      return {
        data: { id: item.id, label: item.label, parent: `band:${item.band}` },
        position: { ...item.position },
        classes: 'atlas-cluster tier-neutral',
      };
    }
    const node = item.node as GraphNode;
    const tier = confidence(node.props);
    return {
      data: {
        id: node.id,
        label: displayName(node),
        kind: node.label,
        tier,
        parent: `band:${item.band}`,
      },
      position: { ...item.position },
      classes: `${overlay ? `tier-${tier.toLowerCase()}` : 'tier-neutral'} ${nodeShapeClass(node)}`,
    };
  });
  const edgeElements = scene.edges.map((item) => {
    if (!item.edge) {
      return {
        data: {
          id: item.id,
          source: item.source,
          target: item.target,
          label: `${item.count ?? 1} relations`,
        },
        classes: 'atlas-agg tier-neutral',
      };
    }
    const edge = item.edge;
    const tier = confidence(edge.props);
    return {
      data: {
        id: `${edge.src}\u0000${edge.label}\u0000${edge.dst}`,
        source: item.source,
        target: item.target,
        // The clickable mono chip: producing tier + relation.
        label: `${tierCode(edge.props)} ${edge.label}`,
        tier,
      },
      classes: overlay ? `tier-${tier.toLowerCase()}` : 'tier-neutral',
    };
  });
  return [...bandParents, ...nodeElements, ...edgeElements];
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
      // The chip IS the click target: without text-events Cytoscape only
      // hit-tests the thin edge geometry (#143 review).
      'text-events': 'yes',
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
    selector: '.kind-box',
    style: { shape: 'round-rectangle' },
  },
  {
    selector: '.kind-channel',
    style: { shape: 'diamond' },
  },
  {
    selector: '.atlas-gap',
    // The red dashed border is the Gap's identity — it must survive the
    // confidence overlay being off (#143 review), so it lives here too.
    style: {
      shape: 'octagon',
      'border-style': 'dashed',
      'border-width': 2,
      'border-color': '#eb5757',
    },
  },
  {
    // Labeled architecture band (compound parent): a quiet container, its
    // label reads top-left; never a selection target of its own.
    selector: '.atlas-band',
    style: {
      shape: 'round-rectangle',
      'background-color': '#191919',
      'background-opacity': 0.4,
      'border-width': 1,
      'border-color': '#333a46',
      label: 'data(label)',
      'font-size': 13,
      color: '#8b919f',
      'text-valign': 'top',
      'text-halign': 'center',
      'text-margin-y': -8,
      'text-background-opacity': 0,
      events: 'no',
    },
  },
  {
    // Collapsed cluster proxy: tap to expand.
    selector: '.atlas-cluster',
    style: {
      shape: 'round-rectangle',
      width: 58,
      height: 44,
      'border-width': 3,
      'border-style': 'double',
      'border-color': '#8b919f',
      'background-color': '#20242c',
    },
  },
  {
    // Aggregated relations between collapsed endpoints.
    selector: '.atlas-agg',
    style: {
      'line-style': 'dotted',
      width: 2,
    },
  },
  {
    selector: ':selected',
    style: { 'border-color': '#abc7ff', 'border-width': 4 },
  },
];

export interface AtlasCanvasProps {
  snapshot: AtlasSnapshot;
  onSelect: (node: GraphNode) => void;
  /** Edge tap → evidence drawer for the edge (same contract as nodes). */
  onSelectEdge?: (edge: GraphEdge) => void;
  /** The active layer drives the header scope chip (`Atlas · <layer>`). */
  onLayerChange?: (label: string) => void;
}

/** Unified read-only graph with layer and confidence projections (US-0010). */
export function AtlasCanvas({ snapshot, onSelect, onSelectEdge, onLayerChange }: AtlasCanvasProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const onSelectRef = useRef(onSelect);
  const onSelectEdgeRef = useRef(onSelectEdge);
  const [layer, setLayer] = useState<AtlasLayer>('all');
  const [overlay, setOverlay] = useState(true);
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set());
  // Focus mode (#160): a navigable stack of roots. Each level is the ego
  // graph of its root within the level above; Esc pops one level.
  const [focusStack, setFocusStack] = useState<string[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const layered = useMemo(() => filterAtlasGraph(snapshot, layer), [snapshot, layer]);
  const visible = useMemo(
    () => focusStack.reduce((snap, root) => focusAtlasGraph(snap, root), layered),
    [layered, focusStack],
  );
  // Deterministic banded scene (#159): same snapshot → same positions.
  const scene = useMemo(() => buildAtlasScene(visible, expanded), [visible, expanded]);
  const namesById = useMemo(
    () => new Map(snapshot.nodes.map((node) => [node.id, displayName(node)])),
    [snapshot],
  );

  const focusOn = (id: string | null) => {
    if (!id || focusStack[focusStack.length - 1] === id) return;
    if (!visible.nodes.some((node) => node.id === id)) return;
    setFocusStack((stack) => [...stack, id]);
    setExpanded(new Set());
    // Keep the keyboard on the canvas: the control just used may unmount
    // with the new projection, and Esc must keep landing inside the card.
    containerRef.current?.focus();
  };
  const backOut = (depth: number) => {
    setFocusStack((stack) => (stack.length > depth ? stack.slice(0, depth) : stack));
    setExpanded(new Set());
    containerRef.current?.focus();
  };

  useEffect(() => {
    onSelectRef.current = onSelect;
    onSelectEdgeRef.current = onSelectEdge;
  }, [onSelect, onSelectEdge]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container || visible.nodes.length === 0) return;
    const byId = new Map(visible.nodes.map((node) => [node.id, node]));
    const cy = cytoscape({
      container,
      elements: elementsFor(scene, overlay),
      style: CY_STYLE,
      layout: { name: 'preset', fit: true, padding: 36 },
      minZoom: 0.08,
      maxZoom: 4,
    });
    // Re-fit once compound band boxes have sized around their children.
    cy.fit(undefined, 36);
    const byEdgeId = new Map(
      visible.edges.map((edge) => [`${edge.src}\u0000${edge.label}\u0000${edge.dst}`, edge]),
    );
    cy.on('tap', 'node', (event) => {
      const id = event.target.id() as string;
      // A collapsed cluster expands in place; bands are inert containers.
      if (id.startsWith('cluster:')) {
        setExpanded((open) => new Set([...open, id]));
        return;
      }
      const node = byId.get(id);
      if (node) {
        setSelectedId(node.id);
        onSelectRef.current(node);
      }
    });
    // Edges (and their label chips) are first-class evidence subjects; an
    // aggregated relation expands the clusters it connects.
    cy.on('tap', 'edge', (event) => {
      const id = event.target.id() as string;
      if (id.startsWith('agg:')) {
        setExpanded((open) => {
          const next = new Set(open);
          for (const endpoint of [event.target.data('source'), event.target.data('target')]) {
            if (typeof endpoint === 'string' && endpoint.startsWith('cluster:')) next.add(endpoint);
          }
          return next;
        });
        return;
      }
      const edge = byEdgeId.get(id);
      if (edge) onSelectEdgeRef.current?.(edge);
    });
    return () => cy.destroy();
  }, [visible, scene, overlay]);

  // Focus mode is keyboard-first (#160): Enter roots the selection, Esc
  // backs out one level. Buttons and form controls keep Enter for their
  // own activation; Esc works from anywhere inside the card.
  const handleFocusKeys = (event: React.KeyboardEvent) => {
    if (event.key === 'Escape' && focusStack.length > 0) {
      event.preventDefault();
      backOut(focusStack.length - 1);
      return;
    }
    if (event.key === 'Enter' && selectedId) {
      if ((event.target as HTMLElement).closest('button, select, input, a, textarea')) return;
      event.preventDefault();
      focusOn(selectedId);
    }
  };

  const currentRoot = focusStack[focusStack.length - 1];
  const focusStatus =
    focusStack.length > 0
      ? `Focused on ${namesById.get(currentRoot) ?? currentRoot} — ${visible.nodes.length} nodes at level ${focusStack.length}. Esc backs out one level.`
      : selectedId
        ? `${namesById.get(selectedId) ?? selectedId} selected — press Enter to focus its connections.`
        : '';

  return (
    <section className="card atlas-card" aria-labelledby="atlas-title" onKeyDown={handleFocusKeys}>
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
            onClick={() => {
              setLayer(item.id);
              setExpanded(new Set());
              // A layer switch changes the projection focus was built on —
              // reset rather than carry roots that may not exist there.
              setFocusStack([]);
              setSelectedId(null);
              onLayerChange?.(item.label);
            }}
          >
            {item.label}
          </button>
        ))}
      </div>

      <nav className="atlas-focus-path" aria-label="Focus path">
        <button
          type="button"
          className={focusStack.length === 0 ? 'active' : ''}
          aria-current={focusStack.length === 0 ? 'true' : undefined}
          onClick={() => backOut(0)}
        >
          Full graph
        </button>
        {focusStack.map((id, index) => (
          <button
            key={`${id}:${index}`}
            type="button"
            className={index === focusStack.length - 1 ? 'active' : ''}
            aria-current={index === focusStack.length - 1 ? 'true' : undefined}
            onClick={() => backOut(index + 1)}
          >
            ▸ {namesById.get(id) ?? id}
          </button>
        ))}
        {selectedId && selectedId !== currentRoot && visible.nodes.some((n) => n.id === selectedId) && (
          <button type="button" className="atlas-focus-cta" onClick={() => focusOn(selectedId)}>
            Focus on {namesById.get(selectedId) ?? selectedId} (Enter)
          </button>
        )}
        {focusStack.length > 0 && (
          <button type="button" onClick={() => backOut(focusStack.length - 1)}>
            Back out (Esc)
          </button>
        )}
        <span aria-live="polite" className="muted atlas-focus-status">
          {focusStatus}
        </span>
      </nav>

      <div className="atlas-legend" aria-label="Confidence legend">
        {TIERS.map((tier) => (
          <span key={tier.id} className={`atlas-legend-${tier.id.toLowerCase()}`}>
            {tier.label}
          </span>
        ))}
        <strong role="status">
          {visible.nodes.length} nodes · {visible.edges.length} edges
          {!scene.autoExpanded &&
            ` · ${scene.collapsed.length} of ${scene.clusters.length} clusters collapsed`}
        </strong>
        {!scene.autoExpanded && (
          <span className="atlas-cluster-controls">
            <button
              type="button"
              onClick={() => setExpanded(new Set(scene.clusters.map((cluster) => cluster.id)))}
            >
              Expand all clusters
            </button>
            <button type="button" onClick={() => setExpanded(new Set())}>
              Collapse clusters
            </button>
          </span>
        )}
      </div>

      <div className="atlas-surface">
        {visible.nodes.length === 0 ? (
          <p className="muted atlas-empty">No graph facts in this layer.</p>
        ) : (
          <div
            ref={containerRef}
            className="atlas-cytoscape"
            data-testid="atlas-canvas"
            tabIndex={0}
            aria-label={`${LAYERS.find((item) => item.id === layer)?.label ?? layer} graph canvas`}
          />
        )}
      </div>

      {scene.collapsed.length > 0 && (
        <div className="atlas-entity-index" aria-label="Collapsed clusters">
          <span className="muted">Collapsed clusters — click to expand</span>
          <div>
            {scene.collapsed.slice(0, 24).map((cluster) => (
              <button
                key={cluster.id}
                type="button"
                onClick={() => setExpanded((open) => new Set([...open, cluster.id]))}
              >
                ▸ {BAND_LABELS[cluster.band]} · {cluster.key} · {cluster.members.length}
              </button>
            ))}
            {scene.collapsed.length > 24 && (
              <span className="muted">+{scene.collapsed.length - 24} more on canvas</span>
            )}
          </div>
        </div>
      )}

      {visible.nodes.length > 0 && (
        <div className="atlas-entity-index">
          <span className="muted">Visible entities</span>
          <div>
            {visible.nodes.slice(0, 24).map((node) => (
              <button
                key={node.id}
                type="button"
                onClick={() => {
                  setSelectedId(node.id);
                  onSelect(node);
                }}
              >
                {displayName(node)}
              </button>
            ))}
            {visible.nodes.length > 24 && <span className="muted">+{visible.nodes.length - 24} more on canvas</span>}
          </div>
        </div>
      )}

      {visible.edges.length > 0 && onSelectEdge && (
        <div className="atlas-entity-index" aria-label="Visible relations">
          <span className="muted">Visible relations</span>
          <div>
            {visible.edges.slice(0, 24).map((edge) => (
              <button
                key={`${edge.src} ${edge.label} ${edge.dst}`}
                type="button"
                className="atlas-edge-chip"
                aria-label={`${tierCode(edge.props)} ${edge.label}: ${edge.src} to ${edge.dst}`}
                title={`${edge.src} → ${edge.dst}`}
                onClick={() => onSelectEdge(edge)}
              >
                {tierCode(edge.props)} {edge.label}
              </button>
            ))}
            {visible.edges.length > 24 && (
              <span className="muted">+{visible.edges.length - 24} more on canvas</span>
            )}
          </div>
        </div>
      )}
    </section>
  );
}
