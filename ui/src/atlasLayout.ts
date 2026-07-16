import type { AtlasSnapshot, GraphEdge, GraphNode } from './store';

/**
 * Deterministic layer-banded Atlas layout (#159, AC-0081).
 *
 * Nodes are placed into labeled architecture bands (Infrastructure → Cloud →
 * Server → Events → Client), clustered within each band by repo/module, and
 * positioned purely from sorted ids — the same snapshot always yields the
 * same scene, regardless of input order. Past a size threshold the initial
 * scene renders collapsed clusters that expand on demand, so reading a
 * large graph never requires manual arrangement.
 */

export type Band = 'infra' | 'cloud' | 'server' | 'events' | 'client' | 'other';

export const BAND_ORDER: readonly Band[] = ['infra', 'cloud', 'server', 'events', 'client', 'other'];

export const BAND_LABELS: Record<Band, string> = {
  infra: 'Infrastructure',
  cloud: 'Cloud',
  server: 'Server',
  events: 'Events',
  client: 'Client',
  other: 'Unclassified',
};

/** Collapse clusters by default above this many nodes (AC-0081). */
export const CLUSTER_THRESHOLD = 200;

/** The primary band a node kind belongs to; Gaps inherit from neighbors. */
function kindBand(node: GraphNode): Band | null {
  switch (node.label) {
    case 'Resource':
      return 'infra';
    case 'Gateway':
      return 'cloud';
    case 'Channel':
      return 'events';
    case 'Screen':
    case 'Component':
    case 'Extension':
    case 'ExtensionContext':
    case 'Command':
    case 'Permission':
      return 'client';
    case 'Module':
    case 'File':
    case 'Symbol':
    case 'Endpoint':
    case 'DataEntity':
    case 'Config':
      return 'server';
    case 'Gap':
      return null; // resolved from neighbors below
    default:
      return 'other';
  }
}

/**
 * Band per node id. A Gap sits in the band of its first banded neighbor
 * (neighbors visited in sorted edge order — deterministic), falling back
 * to `other` when nothing anchors it.
 */
export function assignBands(snapshot: AtlasSnapshot): Map<string, Band> {
  const bands = new Map<string, Band>();
  const nodes = [...snapshot.nodes].sort((a, b) => a.id.localeCompare(b.id));
  const pending: GraphNode[] = [];
  for (const node of nodes) {
    const band = kindBand(node);
    if (band) bands.set(node.id, band);
    else pending.push(node);
  }
  const edges = [...snapshot.edges].sort(
    (a, b) => a.src.localeCompare(b.src) || a.dst.localeCompare(b.dst) || a.label.localeCompare(b.label),
  );
  for (const node of pending) {
    let resolved: Band | null = null;
    for (const edge of edges) {
      const neighbor = edge.src === node.id ? edge.dst : edge.dst === node.id ? edge.src : null;
      if (!neighbor) continue;
      const band = bands.get(neighbor);
      if (band && band !== 'other') {
        resolved = band;
        break;
      }
    }
    bands.set(node.id, resolved ?? 'other');
  }
  return bands;
}

/**
 * Cluster key within a band: `repo · first-path-segment` for ids shaped
 * `prefix:{repo}@{tail}`, the channel kind for `chan:{kind}:{identity}`,
 * else the node kind. Purely id-derived, so stable across re-ingests.
 */
export function clusterKeyFor(node: GraphNode): string {
  const channel = node.id.match(/^chan:([^:]+):/u);
  if (channel) return channel[1];
  const scoped = node.id.match(/^[a-z-]+:([^@]+)@(.+)$/u);
  if (scoped) {
    const [, repo, tail] = scoped;
    // Routed ids (screens) carry a leading slash — strip it so the first
    // real segment names the cluster, and a root route stays `/` (#173
    // review: `repo ·  · N` labels).
    const path = tail.split('#')[0].replace(/^\/+/u, '');
    const segment = path.includes('/') ? path.split('/')[0] : path.split('.')[0];
    return `${repo} · ${segment || '/'}`;
  }
  return node.label;
}

export interface AtlasCluster {
  /** Stable id: `cluster:{band}:{key}`. */
  id: string;
  band: Band;
  key: string;
  /** Member node ids, sorted. */
  members: string[];
}

export interface ScenePosition {
  x: number;
  y: number;
}

export interface SceneNode {
  id: string;
  label: string;
  band: Band;
  /** Present for synthetic collapsed-cluster nodes. */
  cluster?: AtlasCluster;
  /** Present for real graph nodes. */
  node?: GraphNode;
  position: ScenePosition;
}

export interface SceneEdge {
  id: string;
  source: string;
  target: string;
  /** Real edge (chip label + evidence) — absent on aggregated edges. */
  edge?: GraphEdge;
  /** Aggregated relation count between collapsed endpoints. */
  count?: number;
}

export interface AtlasScene {
  nodes: SceneNode[];
  edges: SceneEdge[];
  clusters: AtlasCluster[];
  collapsed: AtlasCluster[];
  bands: Array<{ band: Band; label: string; count: number }>;
  /** True when the snapshot is small enough to open fully expanded. */
  autoExpanded: boolean;
}

const NODE_DX = 96;
const NODE_DY = 84;
const CLUSTER_GAP = 72;
const BAND_GAP = 140;

/**
 * Build the deterministic banded scene. `expanded` holds cluster ids the
 * user opened; snapshots at or under {@link CLUSTER_THRESHOLD} nodes are
 * always fully expanded.
 */
export function buildAtlasScene(snapshot: AtlasSnapshot, expanded: ReadonlySet<string>): AtlasScene {
  const bands = assignBands(snapshot);
  const autoExpanded = snapshot.nodes.length <= CLUSTER_THRESHOLD;

  const byCluster = new Map<string, AtlasCluster>();
  for (const node of [...snapshot.nodes].sort((a, b) => a.id.localeCompare(b.id))) {
    const band = bands.get(node.id) ?? 'other';
    const key = clusterKeyFor(node);
    const id = `cluster:${band}:${key}`;
    const cluster = byCluster.get(id) ?? { id, band, key, members: [] };
    cluster.members.push(node.id);
    byCluster.set(id, cluster);
  }
  const clusters = [...byCluster.values()].sort(
    (a, b) => BAND_ORDER.indexOf(a.band) - BAND_ORDER.indexOf(b.band) || a.key.localeCompare(b.key),
  );

  const isOpen = (cluster: AtlasCluster) => autoExpanded || expanded.has(cluster.id);
  const nodeById = new Map(snapshot.nodes.map((node) => [node.id, node]));
  const sceneNodes: SceneNode[] = [];
  const collapsed: AtlasCluster[] = [];

  let y = 0;
  const bandCounts = new Map<Band, number>();
  for (const band of BAND_ORDER) {
    const bandClusters = clusters.filter((cluster) => cluster.band === band);
    if (bandClusters.length === 0) continue;
    bandCounts.set(
      band,
      bandClusters.reduce((total, cluster) => total + cluster.members.length, 0),
    );
    let x = 0;
    let bandHeight = NODE_DY;
    for (const cluster of bandClusters) {
      if (isOpen(cluster)) {
        const cols = Math.max(1, Math.ceil(Math.sqrt(cluster.members.length)));
        cluster.members.forEach((memberId, index) => {
          const member = nodeById.get(memberId);
          if (!member) return;
          sceneNodes.push({
            id: memberId,
            label: memberId,
            band,
            node: member,
            position: {
              x: x + (index % cols) * NODE_DX,
              y: y + Math.floor(index / cols) * NODE_DY,
            },
          });
        });
        const rows = Math.ceil(cluster.members.length / cols);
        bandHeight = Math.max(bandHeight, rows * NODE_DY);
        x += cols * NODE_DX + CLUSTER_GAP;
      } else {
        collapsed.push(cluster);
        sceneNodes.push({
          id: cluster.id,
          label: `${cluster.key} · ${cluster.members.length}`,
          band,
          cluster,
          position: { x, y },
        });
        x += NODE_DX + CLUSTER_GAP;
      }
    }
    y += bandHeight + BAND_GAP;
  }

  // Edges: endpoints re-target their collapsed cluster; edges internal to a
  // collapsed cluster vanish; parallel collapsed links aggregate to a count.
  const clusterOf = new Map<string, AtlasCluster>();
  for (const cluster of clusters) {
    for (const member of cluster.members) clusterOf.set(member, cluster);
  }
  const endpointFor = (id: string): string | null => {
    const cluster = clusterOf.get(id);
    if (!cluster) return null;
    return isOpen(cluster) ? id : cluster.id;
  };
  const real: SceneEdge[] = [];
  const aggregated = new Map<string, SceneEdge>();
  const sortedEdges = [...snapshot.edges].sort(
    (a, b) => a.src.localeCompare(b.src) || a.dst.localeCompare(b.dst) || a.label.localeCompare(b.label),
  );
  for (const edge of sortedEdges) {
    const source = endpointFor(edge.src);
    const target = endpointFor(edge.dst);
    if (!source || !target || source === target) continue;
    if (source === edge.src && target === edge.dst) {
      real.push({ id: `${edge.src} ${edge.label} ${edge.dst}`, source, target, edge });
      continue;
    }
    const id = `agg:${source} ${target}`;
    const existing = aggregated.get(id);
    if (existing) existing.count = (existing.count ?? 1) + 1;
    else aggregated.set(id, { id, source, target, count: 1 });
  }

  return {
    nodes: sceneNodes,
    edges: [...real, ...aggregated.values()],
    clusters,
    collapsed,
    bands: BAND_ORDER.filter((band) => bandCounts.has(band)).map((band) => ({
      band,
      label: BAND_LABELS[band],
      count: bandCounts.get(band) ?? 0,
    })),
    autoExpanded,
  };
}
