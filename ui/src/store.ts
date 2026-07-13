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

export interface GraphEdge {
  src: string;
  dst: string;
  label: string;
  props: { prov?: Provenance; [key: string]: unknown };
}

export interface AtlasSnapshot {
  nodes: GraphNode[];
  edges: GraphEdge[];
}

export interface IngestSummary {
  job_id: number;
  files: number;
  nodes: number;
  edges: number;
  layers: {
    ts: LayerSummary;
    python: LayerSummary;
    tf: LayerSummary;
  };
  delta?: {
    recomputed_files: number;
    reused_files: number;
    deleted_files: number;
  };
  /** Set for GitHub adds: the cloned repo listed with its SHA (AC-0001). */
  repo?: string;
  commit_sha?: string;
  /** Set for system-manifest adds: `identity@sha12` per repo (AC-0002). */
  repos?: string[];
}

export interface LayerSummary {
  files: number;
  nodes: number;
  edges: number;
}

/** One provenance-bearing resolved hop returned by `flowtracer::Flow`. */
export interface FlowHop {
  label: string;
  src: string;
  dst: string;
  src_name: string;
  dst_name: string;
  tier: string;
  confidence: string;
  evidence: string | null;
  provenance: Provenance;
  gap_reason: string | null;
  attempted_tiers: string[];
}

/** One traced flow as returned by `list_flows` (flowtracer::Flow). */
export interface Flow {
  trigger: string;
  trigger_kind: string;
  trigger_name: string;
  hops: FlowHop[];
  status: 'Verified' | 'Partial' | 'Inferred';
  score: number;
  depth_limited: boolean;
}

export type SpecExportMode = 'verified-only' | 'best-effort';

export interface SpecAssertion {
  id: string;
  subject_id: string;
  subject_kind: string;
  summary: string;
  provenance: Provenance;
}

export interface SpecArtifact {
  id: string;
  file_name: string;
  title: string;
  format: 'markdown' | 'mermaid';
  content: string;
  assertions: SpecAssertion[];
}

export interface SpecBundle {
  mode: SpecExportMode;
  artifacts: SpecArtifact[];
  assertion_count: number;
  gap_count: number;
  drift_count: number;
  security_count: number;
}

export type AssertionDecision = 'accepted' | 'rejected' | 'annotated';

export interface CuratableAssertion {
  subject_id: string;
  summary: string;
  provenance: Provenance;
}

export interface AssertionDecisionRecord {
  assertion: CuratableAssertion;
  decision: AssertionDecision;
  note: string | null;
  updated_at: string;
}

export interface EvidenceSource {
  text: string;
  /** Byte offset of the window within the file (large files are windowed). */
  window_start: number;
  truncated: boolean;
}

/** Source view state: loading → window, or unavailable (file moved, no root). */
export type SourceState = EvidenceSource | 'loading' | 'unavailable';

export interface AppStore {
  /** Backend liveness: unknown until the first ping resolves. */
  backend: 'unknown' | 'up' | 'browser';
  version: string | null;
  stats: GraphStats | null;
  jobs: Job[];
  endpoints: GraphNode[];
  /** Complete deterministic graph projection for the read-only Atlas. */
  atlas: AtlasSnapshot;
  /** Topology map artifact (Mermaid text); null with no backend. */
  topology: string | null;
  /** Flow-dossier artifact (Markdown); null with no backend. */
  flows: string | null;
  /** Traced flows as data (status/score per R-INT-2). */
  flowList: Flow[];
  /** Full official artifact set under the active R-INT-5 mode. */
  specBundle: SpecBundle | null;
  specMode: SpecExportMode;
  curation: AssertionDecisionRecord[];
  specBusy: boolean;
  specError: string | null;
  ingestBusy: boolean;
  ingestSummary: IngestSummary | null;
  ingestError: string | null;
  clearBusy: boolean;
  clearError: string | null;
  /** Node selected for evidence view, with its source window state. */
  selected: { node: GraphNode; source: SourceState } | null;
  refresh: () => Promise<void>;
  enqueueJob: (kind: string) => Promise<void>;
  ingest: (path: string) => Promise<void>;
  clearGraph: () => Promise<void>;
  setSpecMode: (mode: SpecExportMode) => Promise<void>;
  curateAssertion: (
    assertion: SpecAssertion,
    decision: AssertionDecision,
    note?: string,
  ) => Promise<void>;
  select: (node: GraphNode) => Promise<void>;
  clearSelection: () => void;
}

async function loadEndpoints(): Promise<GraphNode[]> {
  return invokeOr<GraphNode[]>('list_nodes', [], { label: 'Endpoint' });
}

/** The ingest root for an evidence ref's repo — each Repo node carries its
 *  own tree root, so multi-repo graphs resolve evidence per repo. */
async function repoRoot(repo: string): Promise<string | null> {
  const repos = await invokeOr<GraphNode[]>('list_nodes', [], { label: 'Repo' });
  const match = repos.find((r) => r.id === `repo:${repo}`) ?? repos[0];
  const root = match?.props?.root;
  return typeof root === 'string' ? root : null;
}

export const useAppStore = create<AppStore>((set, get) => ({
  backend: 'unknown',
  version: null,
  stats: null,
  jobs: [],
  endpoints: [],
  atlas: { nodes: [], edges: [] },
  topology: null,
  flows: null,
  flowList: [],
  specBundle: null,
  specMode: 'best-effort',
  curation: [],
  specBusy: false,
  specError: null,
  ingestBusy: false,
  ingestSummary: null,
  ingestError: null,
  clearBusy: false,
  clearError: null,
  selected: null,

  refresh: async () => {
    const ping = await invokeOr<{ app: string; version: string } | null>('ping', null);
    if (ping === null) {
      set({
        backend: 'browser',
        version: null,
        stats: null,
        jobs: [],
        endpoints: [],
        atlas: { nodes: [], edges: [] },
        topology: null,
        flows: null,
        flowList: [],
        specBundle: null,
        curation: [],
      });
      return;
    }
    const [stats, jobs, endpoints, atlas, topology, flows, flowList, specBundle, curation] = await Promise.all([
      invokeOr<GraphStats>('graph_stats', { nodes: 0, edges: 0 }),
      invokeOr<Job[]>('list_jobs', []),
      loadEndpoints(),
      invokeOr<AtlasSnapshot>('atlas_snapshot', { nodes: [], edges: [] }),
      invokeOr<string | null>('export_topology', null),
      invokeOr<string | null>('export_flows', null),
      invokeOr<Flow[]>('list_flows', []),
      invokeOr<SpecBundle | null>('export_spec', null, { mode: get().specMode }),
      invokeOr<AssertionDecisionRecord[]>('list_assertion_decisions', []),
    ]);
    set({
      backend: 'up',
      version: ping.version,
      stats,
      jobs,
      endpoints,
      atlas,
      topology,
      flows,
      flowList,
      specBundle,
      curation,
    });
  },

  enqueueJob: async (kind: string) => {
    await invokeOr<Job | null>('enqueue_job', null, { kind });
    await get().refresh();
  },

  ingest: async (path: string) => {
    // Clear prior outcome up front so a failed run never shows a stale summary.
    set({ ingestBusy: true, ingestError: null, ingestSummary: null });
    // A GitHub reference clones with real identity (US-0001); a topology
    // manifest ingests the whole declared system (AC-0002); anything else
    // ingests as a local tree.
    const trimmed = path.trim();
    const isRepoUrl = /^(https:\/\/github\.com\/|git@github\.com:)/.test(trimmed);
    const isManifest = trimmed.endsWith('cartograph.system.toml');
    const command = isManifest ? 'add_system' : isRepoUrl ? 'add_repo' : 'ingest_path';
    try {
      const summary = await invokeOr<IngestSummary | null>(
        command,
        null,
        isRepoUrl ? { url: trimmed } : { path: trimmed },
      );
      set({ ingestSummary: summary });
    } catch (e) {
      set({ ingestError: String(e) });
    } finally {
      set({ ingestBusy: false });
      await get().refresh();
    }
  },

  clearGraph: async () => {
    set({ clearBusy: true, clearError: null });
    try {
      const stats = await invokeOr<GraphStats | null>('clear_graph', null);
      if (stats !== null) {
        set({ stats, ingestSummary: null, selected: null });
      }
    } catch (e) {
      set({ clearError: String(e) });
    } finally {
      set({ clearBusy: false });
      await get().refresh();
    }
  },

  setSpecMode: async (mode: SpecExportMode) => {
    set({ specMode: mode, specBusy: true, specError: null });
    try {
      const specBundle = await invokeOr<SpecBundle | null>('export_spec', null, { mode });
      set({ specBundle });
    } catch (error) {
      set({ specError: String(error) });
    } finally {
      set({ specBusy: false });
    }
  },

  curateAssertion: async (
    assertion: SpecAssertion,
    decision: AssertionDecision,
    note?: string,
  ) => {
    set({ specBusy: true, specError: null });
    try {
      await invokeOr<AssertionDecisionRecord | null>('record_assertion_decision', null, {
        assertion: {
          subject_id: assertion.subject_id,
          summary: assertion.summary,
          provenance: assertion.provenance,
        },
        decision,
        note: note?.trim() || null,
      });
      const [specBundle, curation] = await Promise.all([
        invokeOr<SpecBundle | null>('export_spec', null, { mode: get().specMode }),
        invokeOr<AssertionDecisionRecord[]>('list_assertion_decisions', []),
      ]);
      set({ specBundle, curation });
    } catch (error) {
      set({ specError: String(error) });
    } finally {
      set({ specBusy: false });
    }
  },

  select: async (node: GraphNode) => {
    set({ selected: { node, source: 'loading' } });
    const done = (source: SourceState) => {
      // Ignore if the user selected something else meanwhile.
      if (get().selected?.node.id === node.id) set({ selected: { node, source } });
    };
    const ev = node.props.prov?.evidence[0];
    if (!ev) return done('unavailable');
    const root = await repoRoot(ev.repo);
    if (root === null) return done('unavailable');
    try {
      const source = await invokeOr<EvidenceSource | null>('read_evidence', null, {
        root,
        path: ev.path,
        byteStart: ev.byte_start,
        byteEnd: ev.byte_end,
      });
      done(source ?? 'unavailable');
    } catch {
      // Source unavailable (file moved since ingest): panel shows metadata only.
      done('unavailable');
    }
  },

  clearSelection: () => set({ selected: null }),
}));
