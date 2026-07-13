import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { IngestCard } from './IngestCard';

const meta = {
  title: 'Shell/IngestCard',
  component: IngestCard,
  args: { onIngest: fn(), busy: false, summary: null, error: null, canIngest: true },
} satisfies Meta<typeof IngestCard>;

export default meta;
type Story = StoryObj<typeof meta>;

const EMPTY_LAYERS = {
  ts: { files: 0, nodes: 0, edges: 0 },
  tf: { files: 0, nodes: 0, edges: 0 },
};

export const Idle: Story = {
  play: async ({ args, canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.type(canvas.getByRole('textbox'), '/tmp/some-repo');
    await userEvent.click(canvas.getByRole('button', { name: /ingest/i }));
    await expect(args.onIngest).toHaveBeenCalledWith('/tmp/some-repo');
  },
};

export const Busy: Story = {
  args: { busy: true },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('button', { name: /ingesting/i })).toBeDisabled();
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
        tf: { files: 4, nodes: 34, edges: 51 },
      },
    },
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
        tf: { files: 0, nodes: 0, edges: 0 },
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
