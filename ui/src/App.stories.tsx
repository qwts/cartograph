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
    useAppStore.setState({ backend: 'unknown', version: null, stats: null, jobs: [] });
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
