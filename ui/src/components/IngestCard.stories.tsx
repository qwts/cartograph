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

export const Failed: Story = {
  args: { error: 'io: No such file or directory (os error 2)' },
};

export const NoBackend: Story = {
  args: { canIngest: false },
};
