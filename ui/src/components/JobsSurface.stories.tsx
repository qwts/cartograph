import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { JobsSurface } from './JobsSurface';
import type { Job } from '../store';

function job(overrides: Partial<Job> & Pick<Job, 'id' | 'kind' | 'status'>): Job {
  return {
    execution_tracking: 'recorded',
    created_at: '2026-07-14T10:00:00Z',
    updated_at: '2026-07-14T10:05:00Z',
    ...overrides,
  };
}

const ALL_STATES: Job[] = [
  job({ id: 5, kind: 'ingest-source-v1:src_live', status: 'running', stage: 'extract', progress: 40 }),
  job({ id: 4, kind: 'ingest-source-v1:src_queued', status: 'queued' }),
  job({
    id: 3,
    kind: 'ingest:/repo',
    status: 'done',
    progress: 100,
    artifacts: ['graph:local/repo@workdir'],
  }),
  job({ id: 2, kind: 'ingest-source-v1:src_22222222222222222222222222222222', status: 'failed', error: 'io: no such directory' }),
  job({ id: 1, kind: 'ingest-source-v1:src_33333333333333333333333333333333', status: 'interrupted' }),
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
    // Running: friendly stage label (shared with Recover, #209) + accessible
    // progress — never the raw internal stage string.
    await expect(
      canvas.getByText('Parsing source — building the import & call graph'),
    ).toBeInTheDocument();
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
      job({ id: 2, kind: 'ingest-source-v1:src_live', status: 'running', stage: 'extract', progress: 10 }),
      job({ id: 1, kind: 'ingest-source-v1:src_33333333333333333333333333333333', status: 'interrupted' }),
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

export const ViewLiveOnRecoveryJobs: Story = {
  // AC-0094: only recovery-flow jobs (ingest/add-repo/add-system) that are
  // still running/queued get a way back to the Recovering screen; other
  // kinds (and terminal recovery jobs) don't offer a live view that no
  // longer exists. Running jobs also surface the live detail ping.
  args: {
    jobs: [
      job({
        id: 6,
        kind: 'ingest-source-v1:src_live',
        status: 'running',
        stage: 'extract',
        progress: 40,
        detail: 'Reading application code — src/api/routes.ts',
      }),
      job({ id: 4, kind: 'add-system:/repo/cartograph.system.toml', status: 'queued' }),
      job({ id: 3, kind: 'plugin-gate:t0.plugin-fixture', status: 'running', progress: 10 }),
      job({ id: 2, kind: 'ingest:/repo', status: 'done', progress: 100 }),
    ],
    onViewLive: fn(),
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByText('Reading application code — src/api/routes.ts'),
    ).toBeInTheDocument();
    const viewLive = canvas.getAllByRole('button', { name: 'View live' });
    await expect(viewLive).toHaveLength(2); // running ingest + queued add-system only
    await userEvent.click(viewLive[0]);
    await expect(args.onViewLive).toHaveBeenCalledWith(6);
  },
};

export const PreV2CoreDegradesGracefully: Story = {
  // A core without #117 sends no stage/progress/error/artifacts — rows
  // still render with status and timestamps.
  args: { jobs: [job({ id: 1, kind: 'noop', status: 'done', execution_tracking: undefined })] },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('noop')).toBeInTheDocument();
    await expect(canvas.getByText('done')).toBeInTheDocument();
    await expect(canvas.queryByRole('progressbar')).not.toBeInTheDocument();
  },
};

export const RegisteredAndHistoricalRecoveryJobs: Story = {
  // AC-0145: registered-source jobs retain recovery actions; historical path
  // kinds remain visible and require a fresh ingestion before retry.
  args: {
    jobs: [
      job({ id: 8, kind: 'ingest-source-v1:src_88888888888888888888888888888888', status: 'running' }),
      job({ id: 7, kind: 'ingest-source-v1:src_77777777777777777777777777777777', status: 'interrupted' }),
      job({ id: 6, kind: 'ingest:/historical', status: 'interrupted', execution_tracking: 'legacy_unknown' }),
      job({ id: 5, kind: 'ingest:/live-history', status: 'queued', execution_tracking: undefined }),
    ],
    onViewLive: fn(),
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const live = canvas.getAllByRole('button', { name: 'View live' });
    await expect(live).toHaveLength(1);
    await userEvent.click(live[0]);
    await expect(args.onViewLive).toHaveBeenCalledWith(8);
    const resume = canvas.getAllByRole('button', { name: 'Resume' });
    await expect(resume[0]).toBeEnabled();
    await userEvent.click(resume[0]);
    await expect(args.onRetry).toHaveBeenCalledWith(7);
    await expect(resume[1]).toBeDisabled();
    await expect(canvas.getByText(/ingest:\/historical/)).toBeInTheDocument();
    await expect(canvas.getAllByText(/Execution ownership unknown/)).toHaveLength(2);
    await expect(args.onRetry).not.toHaveBeenCalledWith(6);
  },
};

export const LegacyOwnershipIsHistory: Story = {
  // AC-0162: a supported kind does not make a legacy row retryable or live;
  // missing metadata from an older core receives the same honest treatment.
  args: {
    jobs: [
      job({ id: 91, kind: 'ingest-source-v1:src_running', status: 'running', execution_tracking: 'legacy_unknown', stage: 'extract', progress: 45, detail: 'STALE LIVE DETAIL' }),
      job({ id: 92, kind: 'add-repo:owner/project', status: 'failed', execution_tracking: undefined, error: 'stored failure' }),
      job({ id: 93, kind: 'plugin-gate:fixture', status: 'interrupted', execution_tracking: 'legacy_unknown' }),
    ],
    onViewLive: fn(),
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('running')).toBeVisible();
    await expect(canvas.getByText('stored failure')).toBeVisible();
    await expect(canvas.getByText(/Stored progress: 45%/)).toBeVisible();
    await expect(canvas.queryByText('STALE LIVE DETAIL')).not.toBeInTheDocument();
    await expect(canvas.queryByRole('progressbar')).not.toBeInTheDocument();
    await expect(canvasElement.querySelector('.spinning')).not.toBeInTheDocument();
    await expect(canvas.queryByRole('button', { name: 'View live' })).not.toBeInTheDocument();
    await expect(canvas.getByRole('button', { name: 'Retry' })).toBeDisabled();
    await expect(canvas.getByRole('button', { name: 'Resume' })).toBeDisabled();
    await expect(canvas.getAllByText(/Start a fresh operation from its source/)).toHaveLength(3);
    await userEvent.click(canvas.getByRole('button', { name: 'Cancel' }));
    await expect(args.onCancel).toHaveBeenCalledWith(91);
    await expect(args.onRetry).not.toHaveBeenCalled();
  },
};

export const RejectedActionRemainsVisible: Story = {
  // AC-0162: an ownership rejection does not invent a new status or remove
  // retry controls; the user can wait for the owner to finish and try again.
  args: {
    jobs: [job({ id: 94, kind: 'ingest-source-v1:src_recorded', status: 'cancelled' })],
    actionError: 'Retry for job #94 was not confirmed: execution is busy. Wait for the current worker to stop, then refresh Jobs and try again.',
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('alert')).toHaveTextContent('execution is busy');
    await expect(canvas.getByText('cancelled')).toBeVisible();
    await userEvent.click(canvas.getByRole('button', { name: 'Retry' }));
    await expect(args.onRetry).toHaveBeenCalledWith(94);
  },
};
