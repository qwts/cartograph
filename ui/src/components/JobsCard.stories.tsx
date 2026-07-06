import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { JobsCard } from './JobsCard';

const meta = {
  title: 'Shell/JobsCard',
  component: JobsCard,
  args: { onEnqueue: fn() },
} satisfies Meta<typeof JobsCard>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Empty: Story = {
  args: { jobs: [], canEnqueue: true },
  play: async ({ args, canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: /enqueue test job/i }));
    await expect(args.onEnqueue).toHaveBeenCalledWith('noop');
  },
};

export const WithJobs: Story = {
  args: {
    jobs: [
      { id: 2, kind: 'ingest', status: 'running', created_at: '2026-07-05T20:00:00Z', updated_at: '2026-07-05T20:00:05Z' },
      { id: 1, kind: 'noop', status: 'done', created_at: '2026-07-05T19:59:00Z', updated_at: '2026-07-05T19:59:01Z' },
    ],
    canEnqueue: true,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('#2 ingest')).toBeInTheDocument();
    await expect(canvas.getByText('done')).toBeInTheDocument();
  },
};

export const NoBackend: Story = {
  args: { jobs: [], canEnqueue: false },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('button', { name: /enqueue test job/i })).toBeDisabled();
  },
};
