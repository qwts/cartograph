import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { JobsSurface } from './JobsSurface';
import type { Job } from '../store';

function job(overrides: Partial<Job> & Pick<Job, 'id' | 'kind' | 'status'>): Job {
  return {
    created_at: '2026-07-14T10:00:00Z',
    updated_at: '2026-07-14T10:05:00Z',
    ...overrides,
  };
}

const ALL_STATES: Job[] = [
  job({ id: 5, kind: 'ingest:/repo', status: 'running', stage: 'extract', progress: 40 }),
  job({ id: 4, kind: 'ingest:/queued', status: 'queued' }),
  job({
    id: 3,
    kind: 'ingest:/repo',
    status: 'done',
    progress: 100,
    artifacts: ['graph:local/repo@workdir'],
  }),
  job({ id: 2, kind: 'ingest:/gone', status: 'failed', error: 'io: no such directory' }),
  job({ id: 1, kind: 'ingest:/big', status: 'interrupted' }),
];

const meta = {
  title: 'Surfaces/JobsSurface',
  component: JobsSurface,
  args: {
    jobs: ALL_STATES,
    canClear: true,
    onClearFinished: fn(),
    onCancel: fn(),
    onRetry: fn(),
  },
} satisfies Meta<typeof JobsSurface>;

export default meta;
type Story = StoryObj<typeof meta>;

export const EveryLifecycleState: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // Running: stage + accessible progress.
    await expect(canvas.getByText('extract')).toBeInTheDocument();
    await expect(canvas.getByRole('progressbar', { name: 'Job 5 progress' })).toHaveAttribute(
      'aria-valuenow',
      '40',
    );
    // Failure detail and artifact links are visible, never hidden.
    await expect(canvas.getByText('io: no such directory')).toBeInTheDocument();
    await expect(canvas.getByText('graph:local/repo@workdir')).toBeInTheDocument();

    // Lifecycle verbs per status: cancel / retry / resume.
    const cancels = canvas.getAllByRole('button', { name: 'Cancel' });
    await expect(cancels).toHaveLength(2); // running + queued
    await userEvent.click(cancels[0]);
    await expect(args.onCancel).toHaveBeenCalledWith(5);

    await userEvent.click(canvas.getByRole('button', { name: 'Retry' }));
    await expect(args.onRetry).toHaveBeenCalledWith(2);
    await userEvent.click(canvas.getByRole('button', { name: 'Resume' }));
    await expect(args.onRetry).toHaveBeenCalledWith(1);
  },
};

export const LifecycleVerbsOnly: Story = {
  // AC-0077: the production surface manages existing work — no
  // job-creation control ships; Clear finished is the only header action.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.queryByRole('button', { name: /enqueue/i })).not.toBeInTheDocument();
    await expect(canvas.getByRole('button', { name: 'Clear finished' })).toBeEnabled();
    const verbs = canvas
      .getAllByRole('button')
      .map((b) => b.textContent)
      .filter((label) => label !== 'Clear finished');
    await expect(new Set(verbs)).toEqual(new Set(['Cancel', 'Retry', 'Resume']));
  },
};

export const ClearFinishedConfirms: Story = {
  // AC-0076: clearing is confirm-gated, counts only terminal jobs, and
  // states that resumable work is kept.
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Clear finished' }));
    // 2 of the 5 fixture jobs are terminal (done + failed); the queued,
    // running, and interrupted rows never count toward the clear.
    const alert = within(canvas.getByRole('alert'));
    await expect(
      alert.getByText(/Remove 2 finished jobs\? Queued, running, and resumable work is kept\./),
    ).toBeInTheDocument();
    // Declining changes nothing.
    await userEvent.click(alert.getByRole('button', { name: 'Keep history' }));
    await expect(args.onClearFinished).not.toHaveBeenCalled();
    // Confirming fires exactly once.
    await userEvent.click(canvas.getByRole('button', { name: 'Clear finished' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Confirm clear' }));
    await expect(args.onClearFinished).toHaveBeenCalledTimes(1);
  },
};

export const NothingToClear: Story = {
  // With no terminal jobs the clear control is disabled, not hidden.
  args: {
    jobs: [
      job({ id: 2, kind: 'ingest:/repo', status: 'running', stage: 'extract', progress: 10 }),
      job({ id: 1, kind: 'ingest:/big', status: 'interrupted' }),
    ],
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('button', { name: 'Clear finished' })).toBeDisabled();
  },
};

export const Empty: Story = {
  args: { jobs: [] },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('No jobs yet.')).toBeInTheDocument();
    await expect(canvas.getByRole('button', { name: 'Clear finished' })).toBeDisabled();
  },
};

export const PreV2CoreDegradesGracefully: Story = {
  // A core without #117 sends no stage/progress/error/artifacts — rows
  // still render with status and timestamps.
  args: { jobs: [job({ id: 1, kind: 'noop', status: 'done' })] },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('noop')).toBeInTheDocument();
    await expect(canvas.getByText('done')).toBeInTheDocument();
    await expect(canvas.queryByRole('progressbar')).not.toBeInTheDocument();
  },
};
