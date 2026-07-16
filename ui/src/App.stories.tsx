import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, userEvent, waitFor, within } from 'storybook/test';
import App from './App';
import {
  useAppStore,
  type AssertionDecisionRecord,
  type Provenance,
  type SpecAssertion,
  type SpecBundle,
} from './store';

/**
 * Full-app story against a fake Rust core: `mockIPC` installs a fake
 * `__TAURI_INTERNALS__`, so `inTauri()` is true and every command resolves
 * from the handler below — no backend involved. This is the pattern for any
 * story that needs command data.
 */
interface MockJob {
  id: number;
  kind: string;
  status: string;
  created_at: string;
  updated_at: string;
}

// Non-ASCII before the span keeps this honest: provenance spans are UTF-8
// byte offsets, so compute them with TextEncoder, never UTF-16 indexOf.
const FAKE_SOURCE = `// naïve café route — voilà 🚀
import express from 'express';
const app = express();
app.get('/users', listUsers);
`;
const byteLen = (s: string) => new TextEncoder().encode(s).length;
const SPAN_TEXT = "app.get('/users', listUsers)";
const SPAN_START = byteLen(FAKE_SOURCE.slice(0, FAKE_SOURCE.indexOf(SPAN_TEXT)));
const SPAN_END = SPAN_START + byteLen(SPAN_TEXT);

const FAKE_PROVENANCE: Provenance = {
  tier: 'Deterministic',
  confidence_tier: 'Confirmed',
  evidence: [
    {
      repo: 'local',
      path: 'src/app.ts',
      byte_start: SPAN_START,
      byte_end: SPAN_END,
      commit_sha: 'workdir',
    },
  ],
  extractor_id: 't0.adapter-ts',
  content_hash: 'a'.repeat(64),
};

const FAKE_ENDPOINT = {
  id: 'ep:GET:/users',
  label: 'Endpoint',
  props: {
    method: 'GET',
    path: '/users',
    prov: FAKE_PROVENANCE,
  },
};

const FAKE_REPO = {
  id: 'repo:local',
  label: 'Repo',
  props: { root: '/fake/repo' },
};

const FAKE_INFERRED: SpecAssertion = {
  id: 'node:adr:async-orders',
  subject_id: 'adr:async-orders',
  subject_kind: 'ADR',
  summary: 'ADR: asynchronous order fulfillment',
  provenance: {
    tier: 'Semantic',
    confidence_tier: 'InferredStrong',
    evidence: [{
      repo: 'local',
      path: 'src/orders.ts',
      byte_start: 20,
      byte_end: 64,
      commit_sha: 'workdir',
    }],
    extractor_id: 't2.semantic',
    content_hash: 'b'.repeat(64),
  },
};

const FAKE_SPEC: SpecBundle = {
  mode: 'best-effort',
  artifacts: [
    'user_stories.md',
    'US-TM.md',
    'flow_dossiers.md',
    'topology.md',
    'data_model.md',
    'adrs.md',
    'gap_register.md',
    'drift_register.md',
    'security.md',
  ].map((fileName, index) => ({
    id: `artifact-${index}`,
    file_name: fileName,
    title: index === 0 ? 'User stories' : index === 5 ? 'Architecture decisions' : fileName,
    format: fileName.endsWith('.mmd') ? 'mermaid' : 'markdown',
    content: `# ${fileName}\n\n## Assertions and inline provenance\n`,
    assertions: index === 0 ? [{
        id: 'node:ep:GET:/users',
        subject_id: FAKE_ENDPOINT.id,
        subject_kind: 'Endpoint',
        summary: 'Endpoint: GET /users',
        provenance: FAKE_PROVENANCE,
      }] : index === 5 ? [FAKE_INFERRED] : index === 6 ? [{
        // An edge gap: no atlas node carries this id, so the register row
        // must open evidence from the assertion's own provenance (#137).
        id: 'edge:sym:capture CALLS sym:sync',
        subject_id: 'sym:capture CALLS sym:sync',
        subject_kind: 'CALLS',
        summary: 'sym:capture CALLS sym:sync — callee not statically resolvable',
        provenance: { ...FAKE_PROVENANCE, confidence_tier: 'Gap' as const },
      }, {
        // A gap node: its row opens the Resolution Strategy modal (#120).
        id: 'node:gap:sync',
        subject_id: 'gap:sync',
        subject_kind: 'Gap',
        summary: 'Gap: remote sync target computed at runtime',
        provenance: { ...FAKE_PROVENANCE, confidence_tier: 'Gap' as const },
      }] : [],
  })),
  assertion_count: 2,
  gap_count: 0,
  drift_count: 0,
  security_count: 0,
};

interface MockTier {
  tier: string;
  enabled: boolean;
  provider: string;
  consented: boolean;
  consent_disclosure: string | null;
  consented_at: string | null;
}

function installFakeCore() {
  // The fake core boots with one queued job: the production surface offers
  // no job-creation control (AC-0077), so lifecycle stories act on it.
  let jobs: MockJob[] = [
    {
      id: 1,
      kind: 'ingest:/seed',
      status: 'queued',
      created_at: '2026-07-05T19:00:00Z',
      updated_at: '2026-07-05T19:00:00Z',
    },
  ];
  let curation: AssertionDecisionRecord[] = [];
  let graphStats = { nodes: 42, edges: 99 };
  // Mirrors the #118 SettingsStore defaults and invariants.
  let tiers: MockTier[] = ['T1', 'T2', 'T3'].map((tier) => ({
    tier,
    enabled: tier !== 'T3',
    provider: 'local',
    consented: false,
    consent_disclosure: null,
    consented_at: null,
  }));
  const egressSummary = () => {
    const cloud = tiers.filter((t) => t.enabled && t.provider === 'cloud' && t.consented);
    return {
      cloud_tiers: cloud.map((t) => t.tier),
      bytes_sent: 0,
      label: cloud.length
        ? `Cloud enabled (${cloud.map((t) => t.tier).join(', ')}) · 0 bytes egress`
        : 'Local-only · 0 bytes egress',
    };
  };
  mockIPC((cmd, args) => {
    switch (cmd) {
      case 'ping':
        return { app: 'cartograph', version: '0.0.1' };
      case 'graph_stats':
        return graphStats;
      case 'clear_graph':
        graphStats = { nodes: 0, edges: 0 };
        return graphStats;
      case 'list_jobs':
        return jobs;
      case 'list_nodes': {
        const label = (args as { label: string }).label;
        if (label === 'Endpoint') return [FAKE_ENDPOINT];
        if (label === 'Repo') return [FAKE_REPO];
        return [];
      }
      case 'atlas_snapshot':
        return {
          nodes: [
            FAKE_ENDPOINT,
            {
              id: 'sym:app.ts#listUsers',
              label: 'Symbol',
              props: { name: 'listUsers', prov: FAKE_PROVENANCE },
            },
          ],
          edges: [
            {
              src: FAKE_ENDPOINT.id,
              dst: 'sym:app.ts#listUsers',
              label: 'HANDLES',
              props: { prov: FAKE_PROVENANCE },
            },
          ],
        };
      case 'read_evidence':
        return { text: FAKE_SOURCE, window_start: 0, truncated: false };
      case 'export_flows':
        return '# Flow dossier\n\n## GET /users — Verified (score 1.00)\n';
      case 'list_flows':
        return [
          {
            trigger: 'ep:GET:/users',
            trigger_kind: 'Endpoint',
            trigger_name: 'GET /users',
            hops: [
              {
                label: 'HANDLES',
                src: 'ep:GET:/users',
                dst: 'sym:app.ts#listUsers',
                src_name: 'GET /users',
                dst_name: 'listUsers',
                tier: 'Deterministic',
                confidence: 'Confirmed',
                evidence: 'src/app.ts bytes 92..120',
                provenance: FAKE_PROVENANCE,
                gap_reason: null,
                attempted_tiers: [],
              },
              {
                label: 'CALLS',
                src: 'sym:app.ts#listUsers',
                dst: 'gap:sync',
                src_name: 'listUsers',
                dst_name: 'GAP: remote sync target computed at runtime',
                tier: 'Deterministic',
                confidence: 'Gap',
                evidence: 'src/app.ts bytes 92..120',
                provenance: { ...FAKE_PROVENANCE, confidence_tier: 'Gap' },
                gap_reason: 'remote sync target computed at runtime',
                attempted_tiers: ['T0', 'T1'],
              },
            ],
            status: 'Partial',
            score: 0.5,
            depth_limited: false,
          },
        ];
      case 'list_flow_anchors':
        return [
          { kind: 'screens (routes/pages)', found: 0 },
          { kind: 'extension contexts (popup, background, content scripts)', found: 0 },
          { kind: 'extension commands (keyboard shortcuts)', found: 0 },
          { kind: 'HTTP endpoints', found: 1 },
          { kind: 'externally published event channels', found: 0 },
        ];
      case 'system_contents':
        return [{ repo: 'local/image-trail', commit: 'workdir' }];
      case 'list_plugins':
        return [];
      case 'set_plugin_enabled':
        return null;
      case 'adapter_inventory':
        return {
          installed: [
            {
              id: 't0.adapter-ts',
              language: 'TypeScript/JavaScript',
              extensions: ['ts', 'tsx'],
              covers: 'imports, call graph, endpoints',
            },
          ],
          planned: [{ language: 'Kotlin', extensions: ['kt', 'kts'] }],
          detector: 'preflight@1',
        };
      case 'export_topology':
        return 'flowchart LR\n    res_aws_sqs_queue_orders["aws_sqs_queue.orders"]\n';
      case 'export_spec':
        return { ...FAKE_SPEC, mode: (args as { mode: SpecBundle['mode'] }).mode };
      case 'list_assertion_decisions':
        return curation;
      case 'record_assertion_decision': {
        const input = args as {
          assertion: AssertionDecisionRecord['assertion'];
          decision: AssertionDecisionRecord['decision'];
          note: string | null;
        };
        const record: AssertionDecisionRecord = {
          assertion: input.assertion,
          decision: input.decision,
          note: input.note,
          updated_at: '2026-07-13T18:00:00Z',
        };
        curation = [record];
        return record;
      }
      case 'findings_summary':
        // Derives from live graph state like the real core: a cleared graph
        // reports zero facts and the landing collapses honestly.
        return {
          gaps: graphStats.nodes > 0 ? 1 : 0,
          unsupported: 0,
          no_evidence: 0,
          drift: 0,
          open_findings: graphStats.nodes > 0 ? 1 : 0,
          graph_facts: graphStats.nodes + graphStats.edges,
        };
      case 'list_findings':
        return [];
      case 'list_evals':
        return [];
      case 'gap_strategies':
        return {
          gap_id: (args as { gapId: string }).gapId,
          summary: 'Gap: remote sync target computed at runtime',
          stop_reason: 'endpoint host computed from config at runtime',
          attempted_tiers: ['T0'],
          required_evidence: ['E1 · local:src/app.ts'],
          candidates: 1,
          strategies: [
            {
              id: 'local-slm',
              tier: 'T3',
              provider: 'ollama:qwen3:8b',
              locality: 'local',
              egress_bytes: 0,
              est_usd: null,
              latency: 'seconds on-device',
              privacy: 'payload never leaves the device',
              export_impact: 'Best-effort only until accepted (R-INT-5).',
              available: true,
              unavailable_reason: null,
            },
          ],
        };
      case 'run_escalation':
        return {
          gap_id: (args as { gapId: string }).gapId,
          source_id: 'sym:capture',
          target_id: FAKE_ENDPOINT.id,
          edge_label: 'CALLS',
          annotation: 'capture() resolves to the users endpoint per E1.',
          basis_hash: 'b'.repeat(64),
          provenance: {
            ...FAKE_PROVENANCE,
            tier: 'Agentic',
            confidence_tier: 'InferredWeak',
            extractor_id: 't3.agent',
          },
        };
      case 'record_agent_decision': {
        const input = args as { decision: string };
        return { decision: input.decision, recorded_at: '2026-07-14T22:00:00Z' };
      }
      case 'extractor_coverage':
        return [
          {
            extractor: 't0.adapter-ts',
            files_in_scope: 2,
            files_with_facts: 1,
            facts: 12,
            coverage_pct: 50,
          },
        ];
      case 'ingest_history':
        return [
          {
            id: 2,
            job_id: 2,
            repo: 'local/fixture',
            commit_sha: 'workdir',
            confirmed: 40,
            inferred_strong: 2,
            inferred_weak: 0,
            gap: 1,
            unsupported: 0,
            no_evidence: 0,
            graph_facts: 43,
            content_hash: 'd'.repeat(64),
            created_at: '2026-07-14T11:00:00Z',
          },
          {
            id: 1,
            job_id: 1,
            repo: 'local/fixture',
            commit_sha: 'workdir',
            confirmed: 40,
            inferred_strong: 2,
            inferred_weak: 0,
            gap: 1,
            unsupported: 0,
            no_evidence: 0,
            graph_facts: 43,
            content_hash: 'd'.repeat(64),
            created_at: '2026-07-14T10:00:00Z',
          },
        ];
      case 'get_settings':
        return tiers;
      case 'egress_summary':
        return egressSummary();
      case 'cloud_disclosure': {
        const tier = (args as { tier: string }).tier;
        return {
          provider: 'Anthropic',
          model: tier === 'T2' ? 'claude-haiku-4-5-20251001' : 'claude-opus-4-8',
          endpoint: 'https://api.anthropic.com/v1/messages',
          input_usd_per_mtok: tier === 'T2' ? 1 : 5,
          output_usd_per_mtok: tier === 'T2' ? 5 : 25,
          notes: ['payload is the exact redacted span set shown by the egress firewall'],
        };
      }
      case 'set_tier_enabled': {
        const input = args as { tier: string; enabled: boolean };
        tiers = tiers.map((t) => (t.tier === input.tier ? { ...t, enabled: input.enabled } : t));
        return tiers;
      }
      case 'set_tier_provider': {
        const input = args as { tier: string; provider: string };
        // Leaving cloud revokes consent (SettingsStore invariant).
        tiers = tiers.map((t) =>
          t.tier === input.tier
            ? {
                ...t,
                provider: input.provider,
                consented: input.provider === 'cloud' ? t.consented : false,
                consent_disclosure:
                  input.provider === 'cloud' ? t.consent_disclosure : null,
                consented_at: input.provider === 'cloud' ? t.consented_at : null,
              }
            : t,
        );
        return tiers;
      }
      case 'grant_cloud_consent': {
        const input = args as { tier: string; disclosure: string };
        tiers = tiers.map((t) =>
          t.tier === input.tier
            ? {
                ...t,
                consented: true,
                consent_disclosure: input.disclosure,
                consented_at: '2026-07-14T20:00:00Z',
              }
            : t,
        );
        return tiers;
      }
      case 'revoke_cloud_consent': {
        const tier = (args as { tier: string }).tier;
        tiers = tiers.map((t) =>
          t.tier === tier
            ? { ...t, consented: false, consent_disclosure: null, consented_at: null }
            : t,
        );
        return tiers;
      }
      case 'preflight':
        return {
          languages: [
            { language: 'TypeScript/JavaScript', files: 9, adapter: 't0.adapter-ts' },
          ],
          frameworks: ['Chrome Extension MV3'],
          potential_gaps: [
            {
              kind: 'dynamic-injection',
              path: 'src/background.ts',
              line: 41,
              message: 'Dynamically injected function bodies (executeScript)',
              detector: 'preflight@1',
            },
          ],
          unsupported: [
            {
              kind: 'inline-eval',
              path: 'src/legacy.js',
              line: 120,
              message: 'Inline eval()',
              detector: 'preflight@1',
            },
          ],
          detector: 'preflight@1',
        };
      case 'ingest_path':
        return {
          job_id: 1,
          files: 2,
          nodes: 12,
          edges: 18,
          layers: {
            ts: { files: 1, nodes: 8, edges: 12 },
            python: { files: 0, nodes: 0, edges: 0 },
            go: { files: 0, nodes: 0, edges: 0 },
            tf: { files: 1, nodes: 4, edges: 6 },
            java: { files: 0, nodes: 0, edges: 0 },            webext: { files: 0, nodes: 0, edges: 0 },
          },
          delta: { recomputed_files: 2, reused_files: 0, deleted_files: 0 },
        };
      case 'add_system':
        return {
          job_id: 3,
          repos: ['acme/shop@a1b2c3d4e5f6', 'local/infra@workdir'],
          files: 5,
          nodes: 40,
          edges: 60,
          layers: {
            ts: { files: 3, nodes: 25, edges: 38 },
            python: { files: 0, nodes: 0, edges: 0 },
            go: { files: 0, nodes: 0, edges: 0 },
            tf: { files: 2, nodes: 15, edges: 22 },
            java: { files: 0, nodes: 0, edges: 0 },            webext: { files: 0, nodes: 0, edges: 0 },
          },
          delta: { recomputed_files: 5, reused_files: 0, deleted_files: 0 },
        };
      case 'add_repo':
        return {
          job_id: 2,
          repo: 'acme/shop',
          commit_sha: 'a'.repeat(40),
          files: 3,
          nodes: 20,
          edges: 30,
          layers: {
            ts: { files: 3, nodes: 20, edges: 30 },
            python: { files: 0, nodes: 0, edges: 0 },
            go: { files: 0, nodes: 0, edges: 0 },
            tf: { files: 0, nodes: 0, edges: 0 },
            java: { files: 0, nodes: 0, edges: 0 },            webext: { files: 0, nodes: 0, edges: 0 },
          },
          delta: { recomputed_files: 3, reused_files: 0, deleted_files: 0 },
        };
      case 'cancel_job': {
        const id = (args as { id: number }).id;
        jobs = jobs.map((job) => (job.id === id ? { ...job, status: 'cancelled' } : job));
        return jobs.find((job) => job.id === id);
      }
      case 'retry_job': {
        // The fake core mirrors #117's noop re-dispatch: re-queue then done.
        const id = (args as { id: number }).id;
        jobs = jobs.map((job) => (job.id === id ? { ...job, status: 'done' } : job));
        return jobs.find((job) => job.id === id);
      }
      case 'clear_finished_jobs': {
        const before = jobs.length;
        jobs = jobs.filter((job) => !['done', 'failed', 'cancelled'].includes(job.status));
        return before - jobs.length;
      }
      default:
        throw new Error(`unmocked command: ${cmd}`);
    }
  });
}

const meta = {
  title: 'Shell/App',
  component: App,
  beforeEach: () => {
    // Fresh fake core and store per story run (module state persists between
    // stories otherwise); cleanup drops the fake __TAURI_INTERNALS__ so other
    // story files see a clean window.
    installFakeCore();
    useAppStore.setState({
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
    });
    return () => clearMocks();
  },
} satisfies Meta<typeof App>;

export default meta;
type Story = StoryObj<typeof meta>;

export const ConnectedToCore: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    // Boot: ping resolves and the status bar reports the core version.
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await expect(canvas.getByText('42')).toBeInTheDocument();
    await expect(canvas.getByText('99')).toBeInTheDocument();

    // Spec Workbench lives on its own surface now.
    await userEvent.click(canvas.getByRole('button', { name: 'Spec Workbench' }));
    await waitFor(() => expect(canvas.getByText('9 artifacts')).toBeInTheDocument());

    // Lifecycle round-trip (#117/#111) on the seeded queued job: cancel,
    // then retry. No creation control ships on the surface (AC-0077).
    await userEvent.click(canvas.getByRole('button', { name: 'Jobs' }));
    await waitFor(() => expect(canvas.getByText('ingest:/seed')).toBeInTheDocument());
    await expect(canvas.getByText('queued')).toBeInTheDocument();
    await expect(canvas.queryByRole('button', { name: /enqueue/i })).not.toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(canvas.getByText('cancelled')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Retry' }));
    await waitFor(() => expect(canvas.getByText('done')).toBeInTheDocument());

    // Clear finished round-trip (AC-0076): the now-done job is removable.
    await userEvent.click(canvas.getByRole('button', { name: 'Clear finished' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Confirm clear' }));
    await waitFor(() => expect(canvas.getByText('No jobs yet.')).toBeInTheDocument());
  },
};

export const IngestFlowEndToEnd: Story = {
  // #104 / US-0016: Connect → Preflight → Recover against the fake core.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    const breadcrumb = () => within(canvas.getByRole('navigation', { name: 'Breadcrumb' }));

    // Workspace → Connect. The ingest flow routes without joining the rail.
    // (The fake core boots with facts, so the landing offers Re-ingest.)
    await userEvent.click(canvas.getByRole('button', { name: /re-ingest/i }));
    await expect(breadcrumb().getByText('Connect')).toBeInTheDocument();
    await expect(
      canvas.getByText(/Nothing leaves the device unless you opt a tier into cloud/),
    ).toBeInTheDocument();

    // Local target → Preflight runs real (mocked-core) detection.
    await userEvent.click(canvas.getByRole('radio', { name: 'Local folder' }));
    await userEvent.type(canvas.getByRole('textbox'), '/fake/repo');
    await userEvent.click(canvas.getByRole('button', { name: /preflight/i }));
    await expect(breadcrumb().getByText('Preflight')).toBeInTheDocument();
    await waitFor(() =>
      expect(canvas.getByText('Potential system gaps')).toBeInTheDocument(),
    );
    // The three-way split renders from detection output, never conflated.
    await expect(
      canvas.getByText(/Dynamically injected function bodies \(executeScript\)/),
    ).toBeInTheDocument();
    await expect(canvas.getByText(/a tool limitation, not a System Gap/)).toBeInTheDocument();
    await expect(canvas.getByText('0 bytes egress')).toBeInTheDocument();

    // Run full recovery: the fake core resolves instantly, so the flow lands
    // back on Workspace with the recovery outcome visible.
    await userEvent.click(canvas.getByRole('button', { name: /run full recovery/i }));
    await waitFor(() => expect(breadcrumb().getByText('Workspace')).toBeInTheDocument());
    // The landing shows the honest outcome from the register summary.
    await waitFor(() =>
      expect(within(canvas.getByTestId('outcome-card')).getByText('1 open findings')).toBeInTheDocument(),
    );
  },
};

export const ManifestDirectoryUsesAddSystem: Story = {
  // The Connect step's explicit source wins over string inference: a manifest
  // target that is a *directory* (no cartograph.system.toml suffix) must
  // still dispatch add_system, never ingest_path (#104 review).
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());

    await userEvent.click(canvas.getByRole('button', { name: /re-ingest/i }));
    await userEvent.click(canvas.getByRole('radio', { name: 'System manifest' }));
    await userEvent.type(canvas.getByRole('textbox'), '/fake/system-checkout');
    await userEvent.click(canvas.getByRole('button', { name: /preflight/i }));
    // Manifest detection defers to recovery rather than inventing a report.
    await expect(canvas.getByText('Detection deferred')).toBeInTheDocument();

    await userEvent.click(canvas.getByRole('button', { name: /run full recovery/i }));
    // add_system's summary lists every declared repo — the landing titles
    // itself from that multi-repo identity, proof the manifest loader ran
    // rather than the single-repo local path.
    await waitFor(() =>
      expect(canvas.getByText('2 repos as one system')).toBeInTheDocument(),
    );
  },
};

export const GapRowOpensEvidenceForEdgeGap: Story = {
  // #109 review fix: an edge gap has no atlas node, yet its register row
  // must still open the evidence drawer from the assertion's provenance.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Gaps & Drift' }));
    await waitFor(() =>
      expect(canvas.getByText(/callee not statically resolvable/)).toBeInTheDocument(),
    );
    await userEvent.click(canvas.getByText(/callee not statically resolvable/));
    await waitFor(() => {
      const mark = canvasElement.querySelector('.evidence-source mark');
      expect(mark?.textContent).toBe(SPAN_TEXT);
    });
  },
};

export const EscalationRoundTrip: Story = {
  // #113/#120 end to end: gap row → Resolution Strategy → local run →
  // staged proposal → accept records the decision (propose-only, R-INT-3).
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Gaps & Drift' }));
    await waitFor(() =>
      expect(canvas.getByText(/remote sync target computed at runtime/)).toBeInTheDocument(),
    );

    await userEvent.click(canvas.getByText(/remote sync target computed at runtime/));
    await waitFor(() =>
      expect(
        canvas.getByRole('dialog', { name: /remote sync target/i }),
      ).toBeInTheDocument(),
    );
    await expect(
      canvas.getByText(/Why deterministic recovery stopped: endpoint host computed/),
    ).toBeInTheDocument();

    await userEvent.click(canvas.getByRole('button', { name: 'Run locally' }));
    await waitFor(() => expect(canvas.getByTestId('proposal-card')).toBeInTheDocument());
    await expect(canvas.getByText(/never joins the spec until accepted/)).toBeInTheDocument();

    await userEvent.click(canvas.getByRole('button', { name: 'Accept as InferredWeak' }));
    await waitFor(() =>
      expect(canvas.getByTestId('decision-recorded')).toHaveTextContent(
        'Decision recorded: accepted',
      ),
    );
  },
};

export const FlowHopsRouteByKind: Story = {
  // #107 end to end: on the Flows surface a confirmed hop card opens the
  // evidence drawer from the hop's own provenance, while a Gap hop card
  // opens the Resolution Strategy modal for its Gap node.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Flows' }));
    await waitFor(() =>
      expect(canvas.getByText('PARTIAL (1 gap)')).toBeInTheDocument(),
    );

    await userEvent.click(
      canvas.getByRole('button', { name: 'HANDLES: GET /users to listUsers' }),
    );
    await waitFor(() => {
      const mark = canvasElement.querySelector('.evidence-source mark');
      expect(mark?.textContent).toBe(SPAN_TEXT);
    });
    await userEvent.keyboard('{Escape}');

    await userEvent.click(canvas.getByRole('button', { name: /Unresolved hop/ }));
    await waitFor(() =>
      expect(
        canvas.getByRole('dialog', { name: /remote sync target/i }),
      ).toBeInTheDocument(),
    );
  },
};

export const SettingsConsentRoundTrip: Story = {
  // #112 / US-0009: opt T2 into cloud, grant consent, watch the status bar
  // flip — then revoke and watch it fail closed again.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    const statusBar = () => within(canvasElement.querySelector('.status-bar') as HTMLElement);
    await expect(statusBar().getByText('Local-only · 0 bytes egress')).toBeInTheDocument();

    await userEvent.click(canvas.getByRole('button', { name: 'Settings' }));
    await waitFor(() =>
      expect(canvas.getByRole('switch', { name: 'Semantic enabled' })).toBeInTheDocument(),
    );

    // Cloud selection reveals the full disclosure before consent exists.
    const t2Group = within(canvas.getByRole('radiogroup', { name: 'T2 provider' }));
    await userEvent.click(t2Group.getByRole('radio', { name: 'Cloud (opt-in)' }));
    await waitFor(() =>
      expect(canvas.getByText('Cloud egress consent — T2')).toBeInTheDocument(),
    );
    await expect(canvas.getByText('claude-haiku-4-5-20251001')).toBeInTheDocument();
    // Still consentless: the status bar has not moved.
    await expect(statusBar().getByText('Local-only · 0 bytes egress')).toBeInTheDocument();

    // Grant: the summary derives from settings state everywhere.
    await userEvent.click(canvas.getByRole('button', { name: 'Grant revocable consent' }));
    await waitFor(() =>
      expect(
        statusBar().getByText('Cloud enabled (T2) · 0 bytes egress'),
      ).toBeInTheDocument(),
    );
    await expect(canvas.getByText(/Standing cloud consent granted/)).toBeInTheDocument();

    // Revoke: immediate, and the shell reads local-only again.
    await userEvent.click(canvas.getByRole('button', { name: 'Revoke consent' }));
    await waitFor(() =>
      expect(statusBar().getByText('Local-only · 0 bytes egress')).toBeInTheDocument(),
    );
  },
};

export const ShellNavigation: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    const breadcrumb = () => within(canvas.getByRole('navigation', { name: 'Breadcrumb' }));

    // ⌘K opens the palette; Esc closes it without navigating.
    await userEvent.keyboard('{Meta>}k{/Meta}');
    await expect(canvas.getByRole('dialog', { name: 'Command palette' })).toBeInTheDocument();
    await userEvent.keyboard('{Escape}');
    await waitFor(() =>
      expect(canvas.queryByRole('dialog', { name: 'Command palette' })).not.toBeInTheDocument(),
    );
    await expect(breadcrumb().getByText('Workspace')).toBeInTheDocument();

    // Palette navigation: ⌘K → pick a surface.
    await userEvent.keyboard('{Meta>}k{/Meta}');
    await userEvent.click(canvas.getByRole('option', { name: /Gaps & Drift/ }));
    await expect(breadcrumb().getByText('Gaps & Drift')).toBeInTheDocument();

    // ⌘-digit shortcuts jump directly.
    await userEvent.keyboard('{Meta>}3{/Meta}');
    await expect(breadcrumb().getByText('Flows')).toBeInTheDocument();

    // Every rail surface renders — no dead or crashing route (handoff #2).
    for (const label of [
      'Workspace',
      'Atlas',
      'Flows',
      'Spec Workbench',
      'Gaps & Drift',
      'Provenance & Eval',
      'Jobs',
      'Settings',
    ]) {
      await userEvent.click(canvas.getByRole('button', { name: label }));
      await expect(breadcrumb().getByText(label)).toBeInTheDocument();
      await expect(canvas.queryByRole('alert')).not.toBeInTheDocument();
    }

    // The Legend popover is reachable from the header.
    await userEvent.click(canvas.getByRole('button', { name: /legend/i }));
    await expect(
      canvas.getByRole('dialog', { name: 'Tier, shape, and edge legend' }),
    ).toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Close legend' }));
  },
};

export const EvidenceJumpToSource: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    // M1 exit gate, end to end: recovered endpoint -> evidence -> source span.
    await waitFor(() => expect(canvas.getByText('/users')).toBeInTheDocument());
    await userEvent.click(canvas.getByText('/users'));
    await waitFor(() => {
      const mark = canvasElement.querySelector('.evidence-source mark');
      expect(mark?.textContent).toBe(SPAN_TEXT);
    });
    const evidence = within(canvasElement.querySelector('.evidence-panel') as HTMLElement);
    await expect(evidence.getByText(/t0\.adapter-ts/)).toBeInTheDocument();

    // Close returns to the dashboard.
    await userEvent.click(canvas.getByRole('button', { name: /close/i }));
    await waitFor(() =>
      expect(canvasElement.querySelector('.evidence-panel')).not.toBeInTheDocument(),
    );
  },
};

export const AtlasNodeToEvidence: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Atlas' }));
    await waitFor(() =>
      expect(within(canvas.getByLabelText('Confidence legend')).getByRole('status')).toHaveTextContent(
        '2 nodes · 1 edges',
      ),
    );
    await userEvent.click(canvas.getByRole('button', { name: /^GET \/users$/ }));
    await waitFor(() => {
      const mark = canvasElement.querySelector('.evidence-source mark');
      expect(mark?.textContent).toBe(SPAN_TEXT);
    });
    const evidence = within(canvasElement.querySelector('.evidence-panel') as HTMLElement);
    await expect(evidence.getByText(/src\/app\.ts/)).toBeInTheDocument();
    await expect(evidence.getByText(/workdir/)).toBeInTheDocument();

    // #106: with the drawer open, ONE click on an edge chip re-selects —
    // the overlay never swallows it (handoff interaction #1) — and the
    // drawer now shows the edge's own evidence.
    await userEvent.click(canvas.getByRole('button', { name: 'T0 HANDLES: ep:GET:/users to sym:app.ts#listUsers' }));
    await waitFor(() =>
      expect(
        within(canvasElement.querySelector('.evidence-panel') as HTMLElement).getByText(
          /ep:GET:\/users HANDLES sym:app\.ts#listUsers/,
        ),
      ).toBeInTheDocument(),
    );
  },
};

export const WorkbenchCurationRoundTrip: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Spec Workbench' }));
    await waitFor(() => expect(canvas.getByText('9 artifacts')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: /Architecture decisions/ }));
    await expect(canvas.getByText(FAKE_INFERRED.summary)).toBeInTheDocument();
    await userEvent.type(canvas.getByLabelText('Annotation'), 'Confirmed by system owner');
    await userEvent.click(canvas.getByRole('button', { name: 'Annotate' }));
    await waitFor(() => expect(canvas.getAllByText('Annotated').length).toBeGreaterThan(0));
    await expect(canvas.getAllByText('Confirmed by system owner').length).toBeGreaterThan(0);
    await expect(canvas.getAllByText('Inferred (strong)').length).toBeGreaterThan(0);
  },
};

export const ClearGraphPreservesJobs: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByTestId('graph-node-count')).toHaveTextContent('42'));

    // The seeded job exists, then clear the graph from Workspace: the
    // durable job spine must survive a graph clear.
    await userEvent.click(canvas.getByRole('button', { name: 'Jobs' }));
    await waitFor(() => expect(canvas.getByText('ingest:/seed')).toBeInTheDocument());

    await userEvent.click(canvas.getByRole('button', { name: 'Workspace' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Clear system' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Confirm clear' }));
    await waitFor(() => expect(canvas.getByTestId('graph-node-count')).toHaveTextContent('0'));
    await expect(canvas.getByTestId('graph-edge-count')).toHaveTextContent('0');

    await userEvent.click(canvas.getByRole('button', { name: 'Jobs' }));
    await expect(canvas.getByText('ingest:/seed')).toBeInTheDocument();
  },
};
