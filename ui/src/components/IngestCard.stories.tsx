import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { IngestCard } from './IngestCard';

const meta = {
  title: 'Shell/IngestCard',
  component: IngestCard,
  args: { onConnect: fn(), busy: false, summary: null, error: null, canIngest: true },
} satisfies Meta<typeof IngestCard>;

export default meta;
type Story = StoryObj<typeof meta>;

const EMPTY_LAYERS = {
  ts: { files: 0, nodes: 0, edges: 0 },
  python: { files: 0, nodes: 0, edges: 0 },
  go: { files: 0, nodes: 0, edges: 0 },
  tf: { files: 0, nodes: 0, edges: 0 },
  webext: { files: 0, nodes: 0, edges: 0 },
};

export const Idle: Story = {
  play: async ({ args, canvasElement }) => {
    const canvas = within(canvasElement);
    // The card is the entry into the Connect → Preflight → Recover flow.
    await userEvent.click(canvas.getByRole('button', { name: 'Connect a target' }));
    await expect(args.onConnect).toHaveBeenCalled();
  },
};

export const Busy: Story = {
  args: { busy: true },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('button', { name: /recovering/i })).toBeDisabled();
  },
};

export const WithSummary: Story = {
  args: {
    summary: {
      job_id: 3,
      files: 12,
      nodes: 84,
      edges: 141,
      layers: {
        ts: { files: 8, nodes: 50, edges: 90 },
        python: { files: 0, nodes: 0, edges: 0 },
        go: { files: 0, nodes: 0, edges: 0 },
        tf: { files: 4, nodes: 34, edges: 51 },
        webext: { files: 0, nodes: 0, edges: 0 },
      },
    },
  },
};

// AC-0039/AC-0040: unchanged parses are visibly reused on re-ingest.
export const DeltaReingest: Story = {
  args: {
    summary: {
      job_id: 7,
      files: 12,
      nodes: 84,
      edges: 141,
      layers: {
        ts: { files: 8, nodes: 50, edges: 90 },
        python: { files: 0, nodes: 0, edges: 0 },
        go: { files: 0, nodes: 0, edges: 0 },
        tf: { files: 4, nodes: 34, edges: 51 },
        webext: { files: 0, nodes: 0, edges: 0 },
      },
      delta: { recomputed_files: 1, reused_files: 11, deleted_files: 0 },
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('delta-summary').textContent).toBe(
      'Delta: 1 recomputed · 11 reused · 0 removed',
    );
  },
};

// AC-0049: a Pulumi/TS ingest makes its lack of Terraform facts explicit.
export const PulumiWithoutTerraform: Story = {
  args: {
    summary: {
      job_id: 6,
      files: 7,
      nodes: 31,
      edges: 22,
      layers: {
        ts: { files: 7, nodes: 31, edges: 22 },
        python: { files: 0, nodes: 0, edges: 0 },
        go: { files: 0, nodes: 0, edges: 0 },
        tf: { files: 0, nodes: 0, edges: 0 },
        webext: { files: 0, nodes: 0, edges: 0 },
      },
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('ts-layer-summary').textContent).toBe(
      '7 files · 31 nodes · 22 edges',
    );
    await expect(canvas.getByTestId('tf-layer-summary').textContent).toBe(
      '0 files · 0 nodes · 0 edges',
    );
  },
};

// AC-0053: Python and TypeScript are reported separately, so successful
// Python recovery cannot be mistaken for TypeScript coverage.
export const PythonAndTypeScript: Story = {
  args: {
    summary: {
      job_id: 8,
      files: 5,
      nodes: 44,
      edges: 39,
      layers: {
        ts: { files: 3, nodes: 24, edges: 20 },
        python: { files: 2, nodes: 20, edges: 19 },
        go: { files: 0, nodes: 0, edges: 0 },
        tf: { files: 0, nodes: 0, edges: 0 },
        webext: { files: 0, nodes: 0, edges: 0 },
      },
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('python-layer-summary').textContent).toBe(
      '2 files · 20 nodes · 19 edges',
    );
    await expect(canvas.getByTestId('ts-layer-summary').textContent).toBe(
      '3 files · 24 nodes · 20 edges',
    );
  },
};

// AC-0054: Go is visible as its own deterministic language layer alongside
// Python rather than being folded into a generic server count.
export const GoAndPython: Story = {
  args: {
    summary: {
      job_id: 9,
      files: 5,
      nodes: 42,
      edges: 37,
      layers: {
        ts: { files: 0, nodes: 0, edges: 0 },
        python: { files: 2, nodes: 20, edges: 19 },
        go: { files: 3, nodes: 22, edges: 18 },
        tf: { files: 0, nodes: 0, edges: 0 },
        webext: { files: 0, nodes: 0, edges: 0 },
      },
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('go-layer-summary').textContent).toBe(
      '3 files · 22 nodes · 18 edges',
    );
    await expect(canvas.getByTestId('python-layer-summary').textContent).toBe(
      '2 files · 20 nodes · 19 edges',
    );
  },
};

// AC-0002: a system add lists every declared repo with its identity.
export const WithSystemManifest: Story = {
  args: {
    summary: {
      job_id: 5,
      files: 5,
      nodes: 40,
      edges: 60,
      layers: EMPTY_LAYERS,
      repos: ['acme/shop@a1b2c3d4e5f6', 'local/infra@workdir'],
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('system-repos').textContent).toBe(
      'acme/shop@a1b2c3d4e5f6, local/infra@workdir',
    );
  },
};

// AC-0001: a cloned GitHub repo is listed with its commit SHA.
export const WithClonedRepo: Story = {
  args: {
    summary: {
      job_id: 4,
      files: 3,
      nodes: 20,
      edges: 30,
      layers: EMPTY_LAYERS,
      repo: 'acme/shop',
      commit_sha: 'a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2',
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const listed = canvas.getByTestId('cloned-repo');
    await expect(listed.textContent).toBe('acme/shop @ a1b2c3d4e5f6');
  },
};

export const Failed: Story = {
  args: { error: 'io: No such file or directory (os error 2)' },
};

export const NoBackend: Story = {
  args: { canIngest: false },
};
