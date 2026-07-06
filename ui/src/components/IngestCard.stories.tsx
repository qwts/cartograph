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
  args: { summary: { job_id: 3, files: 12, nodes: 84, edges: 141 } },
};

// AC-0001: a cloned GitHub repo is listed with its commit SHA.
export const WithClonedRepo: Story = {
  args: {
    summary: {
      job_id: 4,
      files: 3,
      nodes: 20,
      edges: 30,
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
