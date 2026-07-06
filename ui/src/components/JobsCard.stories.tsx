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

// Regression for #41: an unbreakable ingest path wraps inside its row
// instead of widening it (one wide row used to blow out the whole grid
// track and scroll the shell sideways).
export const LongIngestPath: Story = {
  args: {
    jobs: [
      {
        id: 19,
        kind: 'ingest:/private/tmp/claude-503/-Users-chris-Code-cartograph/0de32f46-7d74-4fe2-a33c-43b3999d518f/scratchpad/mt-m3-fixture',
        status: 'done',
        created_at: '2026-07-06T05:00:00Z',
        updated_at: '2026-07-06T05:00:02Z',
      },
    ],
    canEnqueue: true,
  },
  play: async ({ canvasElement }) => {
    const row = canvasElement.querySelector('.jobs-list li')!;
    const card = canvasElement.querySelector('.card')!;
    await expect(row.getBoundingClientRect().width).toBeLessThanOrEqual(
      card.getBoundingClientRect().width,
    );
    // The path wrapped rather than overflowed.
    await expect(row.scrollWidth).toBeLessThanOrEqual(row.clientWidth + 1);
  },
};
