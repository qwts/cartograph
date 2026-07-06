import type { Meta, StoryObj } from '@storybook/react-vite';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { expect, userEvent, waitFor, within } from 'storybook/test';
import App from './App';
import { useAppStore } from './store';

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

const FAKE_ENDPOINT = {
  id: 'ep:GET:/users',
  label: 'Endpoint',
  props: {
    method: 'GET',
    path: '/users',
    prov: {
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
    },
  },
};

const FAKE_REPO = {
  id: 'repo:local',
  label: 'Repo',
  props: { root: '/fake/repo' },
};

function installFakeCore() {
  let jobs: MockJob[] = [];
  mockIPC((cmd, args) => {
    switch (cmd) {
      case 'ping':
        return { app: 'cartograph', version: '0.0.1' };
      case 'graph_stats':
        return { nodes: 42, edges: 99 };
      case 'list_jobs':
        return jobs;
      case 'list_nodes': {
        const label = (args as { label: string }).label;
        if (label === 'Endpoint') return [FAKE_ENDPOINT];
        if (label === 'Repo') return [FAKE_REPO];
        return [];
      }
      case 'read_evidence':
        return { text: FAKE_SOURCE, window_start: 0, truncated: false };
      case 'export_flows':
        return '# Flow dossier\n\n## GET /users — Verified (score 1.00)\n';
      case 'export_topology':
        return 'flowchart LR\n    res_aws_sqs_queue_orders["aws_sqs_queue.orders"]\n';
      case 'ingest_path':
        return { job_id: 1, files: 2, nodes: 12, edges: 18 };
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
      backend: 'unknown',
      version: null,
      stats: null,
      jobs: [],
      endpoints: [],
      topology: null,
      flows: null,
      ingestBusy: false,
      ingestSummary: null,
      ingestError: null,
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
    // Boot: ping resolves and the badge reports the core version.
    await waitFor(() => expect(canvas.getByText('core v0.0.1')).toBeInTheDocument());
    await expect(canvas.getByText('42')).toBeInTheDocument();
    await expect(canvas.getByText('99')).toBeInTheDocument();

    // Enqueue round-trip: command hits the fake core, list refreshes.
    await userEvent.click(canvas.getByRole('button', { name: /enqueue test job/i }));
    await waitFor(() => expect(canvas.getByText('#1 noop')).toBeInTheDocument());
    await expect(canvas.getByText('queued')).toBeInTheDocument();
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
    await expect(canvas.getByText(/t0\.adapter-ts/)).toBeInTheDocument();

    // Close returns to the dashboard.
    await userEvent.click(canvas.getByRole('button', { name: /close/i }));
    await waitFor(() =>
      expect(canvasElement.querySelector('.evidence-panel')).not.toBeInTheDocument(),
    );
  },
};
