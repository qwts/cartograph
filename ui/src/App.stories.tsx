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
      }] : index === 5 ? [FAKE_INFERRED] : [],
  })),
  assertion_count: 2,
  gap_count: 0,
  drift_count: 0,
  security_count: 0,
};

function installFakeCore() {
  let jobs: MockJob[] = [];
  let curation: AssertionDecisionRecord[] = [];
  let graphStats = { nodes: 42, edges: 99 };
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
        return { nodes: [FAKE_ENDPOINT], edges: [] };
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
            ],
            status: 'Verified',
            score: 1.0,
            depth_limited: false,
          },
        ];
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
      case 'enqueue_job': {
        const job: MockJob = {
          id: jobs.length + 1,
          kind: (args as { kind: string }).kind,
          status: 'queued',
          created_at: '2026-07-05T20:00:00Z',
          updated_at: '2026-07-05T20:00:00Z',
        };
        jobs = [job, ...jobs];
        return job;
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

    // Enqueue round-trip on the Jobs surface: command hits the fake core,
    // the list refreshes.
    await userEvent.click(canvas.getByRole('button', { name: 'Jobs' }));
    await userEvent.click(canvas.getByRole('button', { name: /enqueue test job/i }));
    await waitFor(() => expect(canvas.getByText('noop')).toBeInTheDocument());
    await expect(canvas.getByText('queued')).toBeInTheDocument();

    // Lifecycle round-trip (#117/#111): cancel the queued job, then retry it.
    await userEvent.click(canvas.getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(canvas.getByText('cancelled')).toBeInTheDocument());
    await userEvent.click(canvas.getByRole('button', { name: 'Retry' }));
    await waitFor(() => expect(canvas.getByText('done')).toBeInTheDocument());
  },
};

export const IngestFlowEndToEnd: Story = {
  // #104 / US-0016: Connect → Preflight → Recover against the fake core.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    const breadcrumb = () => within(canvas.getByRole('navigation', { name: 'Breadcrumb' }));

    // Workspace → Connect. The ingest flow routes without joining the rail.
    await userEvent.click(canvas.getByRole('button', { name: 'Connect a target' }));
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
    await waitFor(() => expect(canvas.getByText(/12 nodes/)).toBeInTheDocument());
  },
};

export const ManifestDirectoryUsesAddSystem: Story = {
  // The Connect step's explicit source wins over string inference: a manifest
  // target that is a *directory* (no cartograph.system.toml suffix) must
  // still dispatch add_system, never ingest_path (#104 review).
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());

    await userEvent.click(canvas.getByRole('button', { name: 'Connect a target' }));
    await userEvent.click(canvas.getByRole('radio', { name: 'System manifest' }));
    await userEvent.type(canvas.getByRole('textbox'), '/fake/system-checkout');
    await userEvent.click(canvas.getByRole('button', { name: /preflight/i }));
    // Manifest detection defers to recovery rather than inventing a report.
    await expect(canvas.getByText('Detection deferred')).toBeInTheDocument();

    await userEvent.click(canvas.getByRole('button', { name: /run full recovery/i }));
    // add_system's summary lists every declared repo with its identity —
    // proof the manifest loader ran, not the single-repo local path.
    await waitFor(() =>
      expect(canvas.getByTestId('system-repos')).toHaveTextContent(
        'acme/shop@a1b2c3d4e5f6, local/infra@workdir',
      ),
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
        '1 nodes · 0 edges',
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

    // Enqueue on the Jobs surface, then clear the graph from Workspace: the
    // durable job spine must survive a graph clear.
    await userEvent.click(canvas.getByRole('button', { name: 'Jobs' }));
    await userEvent.click(canvas.getByRole('button', { name: /enqueue test job/i }));
    await waitFor(() => expect(canvas.getByText('noop')).toBeInTheDocument());

    await userEvent.click(canvas.getByRole('button', { name: 'Workspace' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Clear graph' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Confirm clear' }));
    await waitFor(() => expect(canvas.getByTestId('graph-node-count')).toHaveTextContent('0'));
    await expect(canvas.getByTestId('graph-edge-count')).toHaveTextContent('0');

    await userEvent.click(canvas.getByRole('button', { name: 'Jobs' }));
    await expect(canvas.getByText('noop')).toBeInTheDocument();
  },
};
