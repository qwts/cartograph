import { create } from 'zustand';
import { invokeOr } from './tauri';
import type { SurfaceView } from './views';
import type { EgressPreview } from './components/EgressConsentDialog';

export interface GraphStats {
  nodes: number;
  edges: number;
}

/** A durable job row from the state spine. The v2 fields (#117) are
 *  optional so the UI degrades gracefully against a pre-v2 core. */
export interface Job {
  id: number;
  kind: string;
  /** queued | running | done | failed | cancelled | interrupted. */
  status: string;
  /** Current pipeline stage while running. */
  stage?: string | null;
  /** Percent complete (0–100). */
  progress?: number | null;
  /** Failure detail once failed. */
  error?: string | null;
  /** Artifact identifiers produced by a completed job. */
  artifacts?: string[];
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
    go: LayerSummary;
    tf: LayerSummary;
    webext: LayerSummary;
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

/** Where the Connect step points Cartograph (handoff §Connect). */
export type IngestSource = 'github' | 'local' | 'manifest';

/** One detected language with adapter coverage (`ingest::preflight`). */
export interface LanguageDetection {
  language: string;
  files: number;
  /** Covering adapter id; null means the whole language is unsupported. */
  adapter: string | null;
}

/** One classified construct from local detection (`ingest::preflight`). */
export interface PatternFinding {
  kind: string;
  path: string;
  line: number;
  message: string;
  detector: string;
}

/** Local preflight output (#116): the three-way classification exists from
 *  first contact — potential gaps and unsupported patterns never conflate. */
export interface PreflightReport {
  languages: LanguageDetection[];
  frameworks: string[];
  unsupported: PatternFinding[];
  potential_gaps: PatternFinding[];
  detector: string;
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
  /** 1-based file line number of the window's first line; optional so the
   *  UI degrades against a pre-#113 core (assumes 1). */
  window_start_line?: number;
  truncated: boolean;
}

/** Source view state: loading → window, or unavailable (file moved, no root). */
export type SourceState = EvidenceSource | 'loading' | 'unavailable';

/** One persisted register finding (`list_findings`, #116): an unsupported
 *  pattern or a no-evidence question — tool limitations, never Gaps. */
export interface RegisterFinding {
  id: number;
  /** `unsupported` | `no-evidence` (Gap cannot enter by schema). */
  kind: string;
  detector: string;
  repo: string;
  path: string;
  line: number;
  message: string;
  created_at: string;
}

/** Register headline counts (`findings_summary`, #116): one definition —
 *  the spec register's own predicates — reused by every surface. */
export interface FindingsSummary {
  gaps: number;
  unsupported: number;
  no_evidence: number;
  drift: number;
  /** gaps + unsupported + no_evidence. */
  open_findings: number;
  /** Total graph facts (nodes + edges). */
  graph_facts: number;
}

/** One per-ingest history record (`ingest_history`, #119): tier tallies,
 *  register counts, and the whole-graph content hash that makes the
 *  determinism invariant observable. */
export interface IngestRecord {
  id: number;
  job_id: number;
  repo: string;
  commit_sha: string;
  confirmed: number;
  inferred_strong: number;
  inferred_weak: number;
  gap: number;
  unsupported: number;
  no_evidence: number;
  graph_facts: number;
  content_hash: string;
  created_at: string;
}

/** Per-extractor coverage for the latest ingest (`extractor_coverage`). */
export interface ExtractorCoverage {
  extractor: string;
  files_in_scope: number;
  files_with_facts: number;
  facts: number;
  /** Percent of scoped files with facts; null when the extractor was not
   *  in this ingest's scope (not applicable, never a misleading 0%). */
  coverage_pct: number | null;
}

/** One paired-eval calibration record (`list_evals`, M7/M10 gates). */
export interface EvalResult {
  id: number;
  provider: string;
  precision_floor: number;
  similarity_threshold: number;
  precision: number;
  recall: number;
  passed: boolean;
  proposals: number;
  approved: number;
}

/** Fact counts by confidence tier, derived from the same atlas projection
 *  every surface renders (handoff interaction #3: one source of truth). */
export interface TierDistribution {
  confirmed: number;
  inferredStrong: number;
  inferredWeak: number;
  gap: number;
  /** Facts carrying no provenance — never silently promoted. */
  unattributed: number;
  total: number;
}

export function tierDistribution(atlas: AtlasSnapshot): TierDistribution {
  const distribution: TierDistribution = {
    confirmed: 0,
    inferredStrong: 0,
    inferredWeak: 0,
    gap: 0,
    unattributed: 0,
    total: 0,
  };
  const facts = [...atlas.nodes, ...atlas.edges];
  for (const fact of facts) {
    distribution.total += 1;
    switch (fact.props.prov?.confidence_tier) {
      case 'Confirmed':
        distribution.confirmed += 1;
        break;
      case 'InferredStrong':
        distribution.inferredStrong += 1;
        break;
      case 'InferredWeak':
        distribution.inferredWeak += 1;
        break;
      case 'Gap':
        distribution.gap += 1;
        break;
      default:
        distribution.unattributed += 1;
    }
  }
  return distribution;
}

/** One escalation option (`gap_strategies`, #120). */
export interface StrategyCard {
  id: 'local-slm' | 'cloud-opus';
  tier: string;
  provider: string;
  locality: 'local' | 'cloud';
  egress_bytes: number;
  est_usd: number | null;
  latency: string;
  privacy: string;
  export_impact: string;
  available: boolean;
  unavailable_reason: string | null;
}

/** The Resolution Strategy report for one gap (`gap_strategies`, #120). */
export interface GapStrategyReport {
  gap_id: string;
  summary: string;
  stop_reason: string;
  attempted_tiers: string[];
  required_evidence: string[];
  candidates: number;
  strategies: StrategyCard[];
}

/** A staged propose-only escalation result (`run_escalation`, #120). */
export interface AgentProposal {
  gap_id: string;
  source_id: string;
  target_id: string;
  edge_label: string;
  annotation: string;
  basis_hash: string;
  provenance: Provenance;
}

/** UI state of the Resolution Strategy modal (#113). */
export interface EscalationState {
  gapId: string;
  report: GapStrategyReport | null;
  loading: boolean;
  error: string | null;
  running: boolean;
  /** Cloud one-action consent step: the exact preview awaiting a grant. */
  preview: EgressPreview | null;
  proposal: AgentProposal | null;
  /** Set once the user accepted/rejected the proposal. */
  decided: 'accepted' | 'rejected' | null;
}

/** Exact redacted payload preview for the one-action consent dialog. */
export type { EgressPreview } from './components/EgressConsentDialog';

/** One configurable recovery tier's persisted state (#118). T0 is absent by
 *  design — it is always on and never configurable. */
export interface TierSettings {
  tier: 'T1' | 'T2' | 'T3';
  enabled: boolean;
  provider: 'local' | 'cloud';
  consented: boolean;
  consent_disclosure: string | null;
  consented_at: string | null;
}

/** Live egress line for the status bar (#118). */
export interface EgressSummary {
  cloud_tiers: string[];
  bytes_sent: number;
  label: string;
}

/** Everything the fail-closed consent panel shows *before* consent
 *  (`llm::anthropic::disclosure`). */
export interface CloudDisclosure {
  provider: string;
  model: string;
  endpoint: string;
  input_usd_per_mtok: number;
  output_usd_per_mtok: number;
  notes: string[];
}

export interface AppStore {
  /** Active shell surface (handoff: the router is a single `view` value). */
  view: SurfaceView;
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
  /** Connect-step target kind; drives placeholder copy and preflight scope. */
  ingestSource: IngestSource;
  /** Connect-step target: repo URL, local path, or manifest path. */
  ingestTarget: string;
  /** Local detection result; null for remote targets (detected on clone). */
  preflight: PreflightReport | null;
  preflightBusy: boolean;
  preflightError: string | null;
  clearBusy: boolean;
  clearError: string | null;
  /** Register headline counts; null with no backend. */
  findings: FindingsSummary | null;
  /** Persisted unsupported/no-evidence rows for the register surface. */
  registerFindings: RegisterFinding[];
  /** Ingest history, newest first (#119). */
  ingestHistory: IngestRecord[];
  /** Per-extractor coverage for the latest ingest (#119). */
  coverage: ExtractorCoverage[];
  /** Paired-eval calibration records, newest first. */
  evals: EvalResult[];
  /** Resolution Strategy modal state; null while closed (#113). */
  escalation: EscalationState | null;
  /** Persisted tier configuration (T1/T2/T3; T0 is always-on, not stored). */
  tierSettings: TierSettings[];
  /** Live status-bar egress line; null with no backend (shown as local-only). */
  egress: EgressSummary | null;
  /** Disclosure for the tier currently offered cloud consent, keyed by tier.
   *  Loaded before the consent panel renders — fail closed: no disclosure,
   *  no recordable consent. */
  disclosures: Partial<Record<string, CloudDisclosure>>;
  settingsError: string | null;
  /** Node selected for evidence view, with its source window state. */
  selected: { node: GraphNode; source: SourceState; evidenceIndex: number } | null;
  refresh: () => Promise<void>;
  /** Remove terminal (done/failed/cancelled) jobs from the durable spine;
   *  queued, running, and interrupted (resumable) work is kept (AC-0076). */
  clearFinishedJobs: () => Promise<void>;
  /** Run recovery over a target. An explicit `source` (from the Connect
   *  step) picks the command directly; without one it is inferred from the
   *  target string. */
  ingest: (path: string, source?: IngestSource) => Promise<void>;
  clearGraph: () => Promise<void>;
  setSpecMode: (mode: SpecExportMode) => Promise<void>;
  curateAssertion: (
    assertion: SpecAssertion,
    decision: AssertionDecision,
    note?: string,
  ) => Promise<void>;
  /** Open the evidence drawer for a fact; `evidenceIndex` picks among its
   *  supporting evidence spans (default first). */
  select: (node: GraphNode, evidenceIndex?: number) => Promise<void>;
  clearSelection: () => void;
  /** Navigate the shell; clears the evidence selection (handoff §Interactions). */
  setView: (view: SurfaceView) => void;
  /** Cancel a queued/running job (#117); running work stops at its next
   *  stage boundary. */
  cancelJob: (id: number) => Promise<void>;
  /** Retry a failed/cancelled job or resume an interrupted one (#117). */
  retryJob: (id: number) => Promise<void>;
  /** Apply one job transition pushed by the core (`job://changed`). */
  applyJobEvent: (job: Job) => void;
  setIngestSource: (source: IngestSource) => void;
  setIngestTarget: (target: string) => void;
  /** Navigate to Preflight and run local detection (#116). Local targets get
   *  a real report; remote/manifest targets are detected during recovery. */
  runPreflight: () => Promise<void>;
  /** Navigate to Recover and run the staged pipeline. Returns to Workspace
   *  on success unless the user has already navigated away (Run in
   *  background); a failure stays on Recover so the error is never hidden. */
  startRecovery: () => Promise<void>;
  /** Enable/disable a configurable tier (#118); refreshes the egress line. */
  setTierEnabled: (tier: string, enabled: boolean) => Promise<void>;
  /** Choose local or cloud for an LLM tier. Choosing cloud loads the full
   *  disclosure so the consent panel can render it; leaving cloud revokes
   *  standing consent in the core. */
  setTierProvider: (tier: string, provider: 'local' | 'cloud') => Promise<void>;
  /** Record standing cloud consent, storing the exact disclosure shown. */
  grantCloudConsent: (tier: string) => Promise<void>;
  /** Revoke standing consent — immediate, the tier stays on cloud unconsented. */
  revokeCloudConsent: (tier: string) => Promise<void>;
  /** Open the Resolution Strategy modal for a gap and load its report. */
  openResolution: (gapId: string) => Promise<void>;
  closeResolution: () => void;
  /** Run a strategy. Local runs immediately; cloud first loads the exact
   *  egress preview so the one-action consent dialog can show it. */
  runStrategy: (strategyId: 'local-slm' | 'cloud-opus') => Promise<void>;
  /** The user approved the previewed payload — run cloud with its hash. */
  consentAndRun: (preview: EgressPreview) => Promise<void>;
  /** Keep local: dismiss the pending cloud preview without running. */
  dismissPreview: () => void;
  /** Record the human verdict on a staged proposal (R-INT-3). */
  decideProposal: (decision: 'accepted' | 'rejected') => Promise<void>;
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
  view: 'workspace',
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
  ingestSource: 'github',
  ingestTarget: '',
  preflight: null,
  preflightBusy: false,
  preflightError: null,
  clearBusy: false,
  clearError: null,
  findings: null,
  registerFindings: [],
  ingestHistory: [],
  coverage: [],
  evals: [],
  escalation: null,
  tierSettings: [],
  egress: null,
  disclosures: {},
  settingsError: null,
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
        findings: null,
        registerFindings: [],
        ingestHistory: [],
        coverage: [],
        evals: [],
        tierSettings: [],
        egress: null,
      });
      return;
    }
    const [stats, jobs, endpoints, atlas, topology, flows, flowList, specBundle, curation, findings, registerFindings, ingestHistory, coverage, evals, tierSettings, egress, disclosureT2, disclosureT3] = await Promise.all([
      invokeOr<GraphStats>('graph_stats', { nodes: 0, edges: 0 }),
      invokeOr<Job[]>('list_jobs', []),
      loadEndpoints(),
      invokeOr<AtlasSnapshot>('atlas_snapshot', { nodes: [], edges: [] }),
      invokeOr<string | null>('export_topology', null),
      invokeOr<string | null>('export_flows', null),
      invokeOr<Flow[]>('list_flows', []),
      invokeOr<SpecBundle | null>('export_spec', null, { mode: get().specMode }),
      invokeOr<AssertionDecisionRecord[]>('list_assertion_decisions', []),
      invokeOr<FindingsSummary | null>('findings_summary', null),
      invokeOr<RegisterFinding[]>('list_findings', []),
      invokeOr<IngestRecord[]>('ingest_history', []),
      invokeOr<ExtractorCoverage[]>('extractor_coverage', []),
      invokeOr<EvalResult[]>('list_evals', []),
      invokeOr<TierSettings[]>('get_settings', []),
      invokeOr<EgressSummary | null>('egress_summary', null),
      // Disclosures are static per tier — prefetched so the consent panel
      // can always show them before consent is recordable (fail closed).
      invokeOr<CloudDisclosure | null>('cloud_disclosure', null, { tier: 'T2' }),
      invokeOr<CloudDisclosure | null>('cloud_disclosure', null, { tier: 'T3' }),
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
      findings,
      registerFindings,
      ingestHistory,
      coverage,
      evals,
      tierSettings,
      egress,
      disclosures: { T2: disclosureT2 ?? undefined, T3: disclosureT3 ?? undefined },
    });
  },

  clearFinishedJobs: async () => {
    await invokeOr<number | null>('clear_finished_jobs', null);
    await get().refresh();
  },

  ingest: async (path: string, source?: IngestSource) => {
    // Clear prior outcome up front so a failed run never shows a stale summary.
    set({ ingestBusy: true, ingestError: null, ingestSummary: null });
    // A GitHub reference clones with real identity (US-0001); a topology
    // manifest ingests the whole declared system (AC-0002); anything else
    // ingests as a local tree. The Connect step's explicit source wins over
    // string inference — a manifest can also be a directory holding one.
    const trimmed = path.trim();
    const isRepoUrl =
      source === 'github' ||
      (source === undefined && /^(https:\/\/github\.com\/|git@github\.com:)/.test(trimmed));
    const isManifest =
      source === 'manifest' ||
      (source === undefined && trimmed.endsWith('cartograph.system.toml'));
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

  select: async (node: GraphNode, evidenceIndex = 0) => {
    set({ selected: { node, source: 'loading', evidenceIndex } });
    const done = (source: SourceState) => {
      // Ignore if the user selected something else meanwhile.
      const current = get().selected;
      if (current?.node.id === node.id && current.evidenceIndex === evidenceIndex) {
        set({ selected: { node, source, evidenceIndex } });
      }
    };
    const ev = node.props.prov?.evidence[evidenceIndex] ?? node.props.prov?.evidence[0];
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

  setView: (view) => set({ view, selected: null }),

  cancelJob: async (id) => {
    const job = await invokeOr<Job | null>('cancel_job', null, { id });
    if (job) get().applyJobEvent(job);
  },

  retryJob: async (id) => {
    const job = await invokeOr<Job | null>('retry_job', null, { id });
    if (job) get().applyJobEvent(job);
    // A retried ingest may have rebuilt graph artifacts — refresh everything.
    await get().refresh();
  },

  applyJobEvent: (job) =>
    set((state) => {
      const known = state.jobs.some((existing) => existing.id === job.id);
      const jobs = known
        ? state.jobs.map((existing) => (existing.id === job.id ? job : existing))
        : [job, ...state.jobs];
      return { jobs };
    }),

  setIngestSource: (ingestSource) => set({ ingestSource }),

  setIngestTarget: (ingestTarget) => set({ ingestTarget }),

  runPreflight: async () => {
    const target = get().ingestTarget.trim();
    set({ view: 'preflight', selected: null, preflight: null, preflightError: null });
    // Only a local tree can be detected before recovery; GitHub and manifest
    // targets are preflighted against the clone during recovery. Showing
    // nothing beats inventing a report (three-way honesty starts here).
    if (get().ingestSource !== 'local') return;
    set({ preflightBusy: true });
    try {
      const preflight = await invokeOr<PreflightReport | null>('preflight', null, {
        path: target,
      });
      set({ preflight });
    } catch (e) {
      set({ preflightError: String(e) });
    } finally {
      set({ preflightBusy: false });
    }
  },

  startRecovery: async () => {
    const target = get().ingestTarget.trim();
    set({ view: 'recover', selected: null });
    await get().ingest(target, get().ingestSource);
    if (get().view === 'recover' && !get().ingestError) set({ view: 'workspace' });
  },

  setTierEnabled: async (tier, enabled) => {
    set({ settingsError: null });
    try {
      const tierSettings = await invokeOr<TierSettings[] | null>('set_tier_enabled', null, {
        tier,
        enabled,
      });
      const egress = await invokeOr<EgressSummary | null>('egress_summary', null);
      if (tierSettings) set({ tierSettings, egress });
    } catch (e) {
      set({ settingsError: String(e) });
    }
  },

  setTierProvider: async (tier, provider) => {
    set({ settingsError: null });
    try {
      const tierSettings = await invokeOr<TierSettings[] | null>('set_tier_provider', null, {
        tier,
        provider,
      });
      const egress = await invokeOr<EgressSummary | null>('egress_summary', null);
      if (tierSettings) set({ tierSettings, egress });
    } catch (e) {
      set({ settingsError: String(e) });
    }
  },

  grantCloudConsent: async (tier) => {
    const disclosure = get().disclosures[tier];
    // Fail closed: consent is only recordable against a loaded disclosure,
    // and the stored consent carries exactly what the user saw.
    if (!disclosure) {
      set({ settingsError: `no disclosure loaded for ${tier} — consent not recorded` });
      return;
    }
    set({ settingsError: null });
    try {
      const tierSettings = await invokeOr<TierSettings[] | null>('grant_cloud_consent', null, {
        tier,
        disclosure: JSON.stringify(disclosure),
      });
      const egress = await invokeOr<EgressSummary | null>('egress_summary', null);
      if (tierSettings) set({ tierSettings, egress });
    } catch (e) {
      set({ settingsError: String(e) });
    }
  },

  openResolution: async (gapId) => {
    set({
      escalation: {
        gapId,
        report: null,
        loading: true,
        error: null,
        running: false,
        preview: null,
        proposal: null,
        decided: null,
      },
    });
    try {
      const report = await invokeOr<GapStrategyReport | null>('gap_strategies', null, {
        gapId,
      });
      set((state) =>
        state.escalation?.gapId === gapId
          ? { escalation: { ...state.escalation, report, loading: false } }
          : {},
      );
    } catch (e) {
      set((state) =>
        state.escalation?.gapId === gapId
          ? { escalation: { ...state.escalation, error: String(e), loading: false } }
          : {},
      );
    }
  },

  closeResolution: () => set({ escalation: null }),

  runStrategy: async (strategyId) => {
    const current = get().escalation;
    if (!current) return;
    // Results attach only to the gap that started them — the user may have
    // closed this modal and opened another gap before the run resolves.
    const gapId = current.gapId;
    const patch = (fields: Partial<EscalationState>) =>
      set((state) =>
        state.escalation?.gapId === gapId
          ? { escalation: { ...state.escalation, ...fields } }
          : {},
      );
    if (strategyId === 'cloud-opus') {
      // Cloud never runs from this click: load the exact preview so the
      // one-action consent dialog can show precisely what would leave.
      set({ escalation: { ...current, error: null } });
      try {
        const preview = await invokeOr<EgressPreview | null>('escalation_preview', null, {
          gapId,
        });
        patch({ preview });
      } catch (e) {
        patch({ error: String(e) });
      }
      return;
    }
    set({ escalation: { ...current, running: true, error: null } });
    try {
      const proposal = await invokeOr<AgentProposal | null>('run_escalation', null, {
        gapId,
        mode: 'local',
        approvedPayloadHash: null,
      });
      patch({ proposal, running: false });
    } catch (e) {
      patch({ error: String(e), running: false });
    }
  },

  consentAndRun: async (preview) => {
    const current = get().escalation;
    if (!current) return;
    const gapId = current.gapId;
    const patch = (fields: Partial<EscalationState>) =>
      set((state) =>
        state.escalation?.gapId === gapId
          ? { escalation: { ...state.escalation, ...fields } }
          : {},
      );
    set({ escalation: { ...current, preview: null, running: true, error: null } });
    try {
      const proposal = await invokeOr<AgentProposal | null>('run_escalation', null, {
        gapId,
        mode: 'cloud',
        approvedPayloadHash: preview.payload_hash,
      });
      const egress = await invokeOr<EgressSummary | null>('egress_summary', null);
      set({ egress });
      patch({ proposal, running: false });
    } catch (e) {
      patch({ error: String(e), running: false });
    }
  },

  dismissPreview: () =>
    set((state) =>
      state.escalation ? { escalation: { ...state.escalation, preview: null } } : {},
    ),

  decideProposal: async (decision) => {
    const current = get().escalation;
    if (!current?.proposal) return;
    try {
      await invokeOr('record_agent_decision', null, {
        proposal: current.proposal,
        decision,
        note: null,
      });
      set((state) =>
        state.escalation ? { escalation: { ...state.escalation, decided: decision } } : {},
      );
      // An accepted proposal changes best-effort exports — refresh them.
      await get().refresh();
    } catch (e) {
      set((state) =>
        state.escalation ? { escalation: { ...state.escalation, error: String(e) } } : {},
      );
    }
  },

  revokeCloudConsent: async (tier) => {
    set({ settingsError: null });
    try {
      const tierSettings = await invokeOr<TierSettings[] | null>('revoke_cloud_consent', null, {
        tier,
      });
      const egress = await invokeOr<EgressSummary | null>('egress_summary', null);
      if (tierSettings) set({ tierSettings, egress });
    } catch (e) {
      set({ settingsError: String(e) });
    }
  },
}));
