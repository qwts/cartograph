import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { RecoverSurface } from './RecoverSurface';
import type { Job } from '../store';

const RUNNING: Job = {
  id: 12,
  kind: 'ingest:/repos/image-trail',
  status: 'running',
  stage: 'extract',
  progress: 15,
  created_at: '2026-07-14T10:00:00Z',
  updated_at: '2026-07-14T10:00:05Z',
};

const meta = {
  title: 'Ingest/RecoverSurface',
  component: RecoverSurface,
  args: {
    job: RUNNING,
    busy: true,
    error: null,
    onBack: fn(),
    onBackground: fn(),
  },
} satisfies Meta<typeof RecoverSurface>;

export default meta;
type Story = StoryObj<typeof meta>;

export const StreamingCoreProgress: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // Real job events drive the view: friendly stage label, mono %, bar.
    await expect(canvas.getByTestId('recover-spinner')).toBeInTheDocument();
    await expect(
      canvas.getByText(/reconstruct endpoints, call graphs/i),
    ).toBeInTheDocument();
    await expect(canvas.getByRole('status')).toHaveTextContent(
      'Parsing source — building the import & call graph',
    );
    await expect(canvas.getByText('15%')).toBeInTheDocument();
    await expect(
      canvas.getByRole('progressbar', { name: 'Recovery progress' }),
    ).toHaveAttribute('aria-valuenow', '15');

    // Run in background frees the UI.
    await userEvent.click(canvas.getByRole('button', { name: 'Run in background' }));
    await expect(args.onBackground).toHaveBeenCalled();
  },
};

export const LiveDetailPing: Story = {
  // AC-0094: the current adapter/file, streamed over job://detail and
  // rendered as a secondary line — best-effort, so it's fine to be absent
  // (see StreamingCoreProgress, which has no detail set).
  args: {
    job: { ...RUNNING, detail: 'Reading application code — src/api/routes.ts' },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByText('Reading application code — src/api/routes.ts'),
    ).toBeInTheDocument();
  },
};

export const BeforeFirstJobEvent: Story = {
  // The ingest command is in flight but no job row has landed yet: the
  // spinner shows honestly with no invented percentage.
  args: { job: null },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('status')).toHaveTextContent('Preparing…');
    await expect(canvas.queryByRole('progressbar')).not.toBeInTheDocument();
  },
};

export const UnknownStagePassesThrough: Story = {
  // A newer core with a new stage name still reads (no silent blank).
  args: { job: { ...RUNNING, stage: 'vectorize', progress: 55 } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('status')).toHaveTextContent('vectorize');
  },
};

export const Failed: Story = {
  args: { job: null, busy: false, error: 'clone failed: repository not found' },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Recovery failed')).toBeInTheDocument();
    await expect(canvas.getByText('clone failed: repository not found')).toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Back' }));
    await expect(args.onBack).toHaveBeenCalled();
  },
};
