import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { PreflightSurface } from './PreflightSurface';
import type { PreflightReport } from '../store';

/** Mirrors `ingest::preflight` output for the handoff's image-trail example. */
const REPORT: PreflightReport = {
  languages: [
    { language: 'TypeScript/JavaScript', files: 14, adapter: 't0.adapter-ts' },
    { language: 'Rust', files: 3, adapter: null },
  ],
  frameworks: ['Chrome Extension MV3', 'React'],
  potential_gaps: [
    {
      kind: 'dynamic-injection',
      path: 'src/background.ts',
      line: 41,
      message: 'Dynamically injected function bodies (executeScript)',
      detector: 'preflight@1',
    },
    {
      kind: 'computed-import',
      path: 'src/sync.ts',
      line: 9,
      message: 'Runtime-computed sync host',
      detector: 'preflight@1',
    },
  ],
  unsupported: [
    {
      kind: 'inline-eval',
      path: 'src/legacy.js',
      line: 120,
      message: 'Inline eval()',
      detector: 'preflight@1',
    },
    {
      kind: 'wasm-module',
      path: 'src/filters.wasm',
      line: 1,
      message: 'WASM module',
      detector: 'preflight@1',
    },
  ],
  detector: 'preflight@1',
};

const meta = {
  title: 'Ingest/PreflightSurface',
  component: PreflightSurface,
  args: {
    source: 'local',
    target: '/repos/image-trail',
    report: REPORT,
    busy: false,
    error: null,
    canRecover: true,
    onBack: fn(),
    onRunRecovery: fn(),
  },
} satisfies Meta<typeof PreflightSurface>;

export default meta;
type Story = StoryObj<typeof meta>;

export const ThreeWayClassification: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // The egress contract is in the header.
    await expect(canvas.getByText('0 bytes egress')).toBeInTheDocument();

    // Detection results.
    await expect(canvas.getByText(/TypeScript\/JavaScript/)).toBeInTheDocument();
    await expect(canvas.getByText('t0.adapter-ts')).toBeInTheDocument();
    await expect(canvas.getByText('Chrome Extension MV3')).toBeInTheDocument();
    // An uncovered language is labeled, not hidden.
    await expect(
      canvas.getByText(/no adapter — surfaces under Unsupported patterns/),
    ).toBeInTheDocument();

    // The two classification cards exist separately and never conflate:
    // gaps are predicted System Gaps with Resolution Strategies…
    await expect(canvas.getByText('Potential system gaps')).toBeInTheDocument();
    await expect(
      canvas.getByText(/predicted System Gaps — each gets an explicit Gap with a Resolution/),
    ).toBeInTheDocument();
    await expect(
      canvas.getByText('Dynamically injected function bodies (executeScript)', { exact: false }),
    ).toBeInTheDocument();
    // …while unsupported items are tool limitations, explicitly NOT Gaps.
    await expect(canvas.getByText('Unsupported patterns')).toBeInTheDocument();
    await expect(
      canvas.getByText(/a tool limitation, not a System Gap/),
    ).toBeInTheDocument();
    await expect(canvas.getByText('WASM module', { exact: false })).toBeInTheDocument();

    // Structure only is visible but deferred to #120.
    await expect(canvas.getByRole('button', { name: 'Structure only' })).toBeDisabled();
    await userEvent.click(canvas.getByRole('button', { name: /run full recovery/i }));
    await expect(args.onRunRecovery).toHaveBeenCalled();
  },
};

export const CleanTree: Story = {
  args: {
    report: {
      languages: [{ language: 'Python', files: 6, adapter: 't0.adapter-python' }],
      frameworks: [],
      potential_gaps: [],
      unsupported: [],
      detector: 'preflight@1',
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    // Both classification cards render their honest empty state.
    await expect(canvas.getAllByText('None detected.')).toHaveLength(2);
    await expect(canvas.getByText('No framework markers detected.')).toBeInTheDocument();
  },
};

export const RemoteTargetDefersDetection: Story = {
  args: { source: 'github', target: 'github.com/acme/shop', report: null },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Detection deferred')).toBeInTheDocument();
    await expect(
      canvas.getByText(/detected against its local clone at the start of recovery/),
    ).toBeInTheDocument();
    // Deferred detection still allows recovery.
    await expect(canvas.getByRole('button', { name: /run full recovery/i })).toBeEnabled();
  },
};

export const DetectionFailed: Story = {
  args: { report: null, error: 'io: No such file or directory (os error 2)' },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByText('io: No such file or directory (os error 2)'),
    ).toBeInTheDocument();
  },
};

export const Detecting: Story = {
  args: { report: null, busy: true },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('status')).toHaveTextContent('Detecting…');
    await expect(canvas.getByRole('button', { name: /run full recovery/i })).toBeDisabled();
  },
};
