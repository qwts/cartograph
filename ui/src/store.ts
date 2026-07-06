import { create } from 'zustand';
import { invokeOr } from './tauri';

export interface GraphStats {
  nodes: number;
  edges: number;
}

export interface Job {
  id: number;
  kind: string;
  status: string;
  created_at: string;
  updated_at: string;
}

/** Evidence span as stored by core-prov (SPEC-00 §4.3). */
export interface EvidenceRef {
  repo: string;
  path: string;
  byte_start: number;
  byte_end: number;
  commit_sha: string;
}

export type Tier = 'Confirmed' | 'InferredStrong' | 'InferredWeak' | 'Gap';

export interface Provenance {
  tier: string;
  confidence_tier: Tier;
  evidence: EvidenceRef[];
  extractor_id: string;
  content_hash: string;
}

/** A graph node as returned by the core (props schema varies by label). */
export interface GraphNode {
  id: string;
  label: string;
  props: { prov?: Provenance; [key: string]: unknown };
}

export interface IngestSummary {
  job_id: number;
  files: number;
  nodes: number;
  edges: number;
}

export interface EvidenceSource {
  text: string;
  truncated: boolean;
}

export interface AppStore {
  /** Backend liveness: unknown until the first ping resolves. */
  backend: 'unknown' | 'up' | 'browser';
  version: string | null;
  stats: GraphStats | null;
  jobs: Job[];
  endpoints: GraphNode[];
  ingestBusy: boolean;
  ingestSummary: IngestSummary | null;
  ingestError: string | null;
  /** Node selected for evidence view, with its loaded source. */
  selected: { node: GraphNode; source: EvidenceSource | null } | null;
  refresh: () => Promise<void>;
  enqueueJob: (kind: string) => Promise<void>;
  ingest: (path: string) => Promise<void>;
  select: (node: GraphNode) => Promise<void>;
  clearSelection: () => void;
}

async function loadEndpoints(): Promise<GraphNode[]> {
  return invokeOr<GraphNode[]>('list_nodes', [], { label: 'Endpoint' });
}

/** The ingest root recorded on the Repo node; evidence reads are confined to it. */
async function repoRoot(): Promise<string | null> {
  const repos = await invokeOr<GraphNode[]>('list_nodes', [], { label: 'Repo' });
  const root = repos[0]?.props?.root;
  return typeof root === 'string' ? root : null;
}

export const useAppStore = create<AppStore>((set, get) => ({
  backend: 'unknown',
  version: null,
  stats: null,
  jobs: [],
  endpoints: [],
  ingestBusy: false,
  ingestSummary: null,
  ingestError: null,
  selected: null,

  refresh: async () => {
    const ping = await invokeOr<{ app: string; version: string } | null>('ping', null);
    if (ping === null) {
      set({ backend: 'browser', version: null, stats: null, jobs: [], endpoints: [] });
      return;
    }
    const [stats, jobs, endpoints] = await Promise.all([
      invokeOr<GraphStats>('graph_stats', { nodes: 0, edges: 0 }),
      invokeOr<Job[]>('list_jobs', []),
      loadEndpoints(),
    ]);
    set({ backend: 'up', version: ping.version, stats, jobs, endpoints });
  },

  enqueueJob: async (kind: string) => {
    await invokeOr<Job | null>('enqueue_job', null, { kind });
    await get().refresh();
  },

  ingest: async (path: string) => {
    set({ ingestBusy: true, ingestError: null });
    try {
      const summary = await invokeOr<IngestSummary | null>('ingest_path', null, { path });
      set({ ingestSummary: summary });
    } catch (e) {
      set({ ingestError: String(e) });
    } finally {
      set({ ingestBusy: false });
      await get().refresh();
    }
  },

  select: async (node: GraphNode) => {
    set({ selected: { node, source: null } });
    const ev = node.props.prov?.evidence[0];
    if (!ev) return;
    const root = await repoRoot();
    if (root === null) return;
    try {
      const source = await invokeOr<EvidenceSource | null>('read_evidence', null, {
        root,
        path: ev.path,
      });
      // Ignore if the user selected something else meanwhile.
      if (get().selected?.node.id === node.id) set({ selected: { node, source } });
    } catch {
      // Source unavailable (file moved since ingest): panel shows metadata only.
    }
  },

  clearSelection: () => set({ selected: null }),
}));
