import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { GapsDriftSurface } from './GapsDriftSurface';
import type { Provenance, RegisterFinding, SpecAssertion } from '../store';

function gapProvenance(tier: string): Provenance {
  return {
    tier,
    confidence_tier: 'Gap',
    evidence: [
      {
        repo: 'local/image-trail',
        path: 'src/background.ts',
        byte_start: 100,
        byte_end: 180,
        commit_sha: 'workdir',
      },
    ],
    extractor_id: 't0.adapter-ts',
    content_hash: 'c'.repeat(64),
  };
}

/** The handoff's three example gaps (screenshot 05). */
const GAPS: SpecAssertion[] = [
  {
    id: 'node:gap:executeScript',
    subject_id: 'gap:executeScript',
    subject_kind: 'Gap',
    summary: 'chrome.scripting.executeScript — injected function body not linkable',
    provenance: gapProvenance('Deterministic'),
  },
  {
    id: 'node:gap:sync-host',
    subject_id: 'gap:sync-host',
    subject_kind: 'Gap',
    summary: 'Remote sync target — endpoint host computed from config at runtime',
    provenance: gapProvenance('Dynamic'),
  },
  {
    id: 'node:gap:offscreen',
    subject_id: 'gap:offscreen',
    subject_kind: 'Gap',
    summary: 'Offscreen document lifecycle — teardown path not statically provable',
    provenance: gapProvenance('Semantic'),
  },
];

// Raw registers restate findings in other shapes: a flow-hop assertion
// repeats a gap inside its flow; CONFLICTS edges support a drift node.
// The tally counts neither — the surface must filter them out (#137 review).
const FLOW_HOP_RESTATEMENT: SpecAssertion = {
  id: 'flow:ep:capture:1:CALLS:sym:sync',
  subject_id: 'sym:capture CALLS sym:sync',
  subject_kind: 'FlowHop',
  summary: 'CALLS: capture → sync (unresolved)',
  provenance: gapProvenance('Deterministic'),
};

const DRIFT_SUPPORT_EDGE: SpecAssertion = {
  id: 'edge:adr:sync CONFLICTS ch:events',
  subject_id: 'adr:sync CONFLICTS ch:events',
  subject_kind: 'CONFLICTS',
  summary: 'adr:sync CONFLICTS ch:events',
  provenance: { ...gapProvenance('Deterministic'), confidence_tier: 'InferredStrong' },
};

const DRIFT: SpecAssertion[] = [
  {
    id: 'node:drift:sync-batching',
    subject_id: 'drift:sync-batching',
    subject_kind: 'Drift',
    summary: 'ADR-0002 declares batched sync; observed per-event POST on capture',
    provenance: { ...gapProvenance('Deterministic'), confidence_tier: 'InferredStrong' },
  },
  DRIFT_SUPPORT_EDGE,
];

const REGISTER_FINDINGS: RegisterFinding[] = [
  {
    id: 1,
    kind: 'unsupported',
    detector: 'preflight@1',
    repo: 'local/image-trail',
    path: 'src/wasm/filter.wasm',
    line: 1,
    message: 'WebAssembly image filter module — no WASM adapter',
    created_at: '2026-07-14T10:00:00Z',
  },
  {
    id: 2,
    kind: 'unsupported',
    detector: 'preflight@1',
    repo: 'local/image-trail',
    path: 'src/legacy.js',
    line: 120,
    message: 'Inline eval() — dynamic code construction',
    created_at: '2026-07-14T10:00:00Z',
  },
  {
    id: 3,
    kind: 'no-evidence',
    detector: 'preflight@1',
    repo: 'local/image-trail',
    path: 'README.md',
    line: 1,
    message: 'Retention policy for captured frames — no evidence in code or config',
    created_at: '2026-07-14T10:00:00Z',
  },
];

const meta = {
  title: 'Surfaces/GapsDriftSurface',
  component: GapsDriftSurface,
  args: {
    summary: {
      gaps: 3,
      unsupported: 2,
      no_evidence: 1,
      drift: 1,
      open_findings: 6,
      graph_facts: 134,
    },
    gaps: [...GAPS, FLOW_HOP_RESTATEMENT],
    drift: DRIFT,
    registerFindings: REGISTER_FINDINGS,
    onOpenGap: fn(),
  },
} satisfies Meta<typeof GapsDriftSurface>;

export default meta;
type Story = StoryObj<typeof meta>;

export const ThreeLanesNeverConflate: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // Header tally quotes the shared register summary — the same numbers
    // the lanes below add up to.
    await expect(canvas.getByText('6 open findings')).toBeInTheDocument();
    await expect(canvas.getByText('3 gaps')).toBeInTheDocument();
    await expect(canvas.getByText('2 unsupported')).toBeInTheDocument();

    // Lane headers reconcile with the tally — the raw register's flow-hop
    // restatement (a 4th assertion) is filtered out, not double-counted.
    await expect(canvas.getByText('System gaps · 3')).toBeInTheDocument();
    await expect(canvas.queryByText(/capture → sync \(unresolved\)/)).not.toBeInTheDocument();
    await expect(canvas.getByText('Unsupported patterns · 2')).toBeInTheDocument();
    await expect(canvas.getByText('No evidence found · 1')).toBeInTheDocument();

    // Gaps get ids, a next-tier tail, and open their resolution seam.
    await expect(canvas.getByText('G-01')).toBeInTheDocument();
    await expect(canvas.getByText('T1 next')).toBeInTheDocument();
    await userEvent.click(
      canvas.getByText(/executeScript — injected function body not linkable/),
    );
    await expect(args.onOpenGap).toHaveBeenCalledWith(GAPS[0]);

    // Unsupported rows are explicitly tool limitations, not gaps — static
    // rows with a file tail, and the lane says so.
    await expect(canvas.getByText(/a tool limitation, not a System Gap/)).toBeInTheDocument();
    await expect(canvas.getByText('src/wasm/filter.wasm:1')).toBeInTheDocument();
    // No-evidence absence is stated, not implied.
    await expect(
      canvas.getByText(/Retention policy for captured frames/),
    ).toBeInTheDocument();
  },
};

export const EscalationTierGrouping: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('tab', { name: 'By escalation tier' }));

    // Gaps group under the next ladder rung derived from the tier that
    // established them: Deterministic→T1, Dynamic→T2, Semantic→T3 —
    // one gap under each.
    await expect(canvas.getByText('T1', { selector: 'code' })).toBeInTheDocument();
    await expect(canvas.getAllByText(/next escalation · 1 open/)).toHaveLength(3);
    const t1Section = canvas.getByText('T1', { selector: 'code' })
      .closest('div')?.parentElement as HTMLElement;
    await expect(
      within(t1Section).getByText(/executeScript/),
    ).toBeInTheDocument();

    // The integrity rule is stated on the surface.
    await expect(
      canvas.getByText(/T2\/T3 escalations propose only — they never overwrite T0\/T1/),
    ).toBeInTheDocument();

    // Rows stay actionable in this projection too.
    await userEvent.click(canvas.getByText(/endpoint host computed from config/));
    await expect(args.onOpenGap).toHaveBeenCalledWith(GAPS[1]);
  },
};

export const DriftTab: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('tab', { name: 'Drift' }));
    // The headline counts drift nodes only — the CONFLICTS support edge in
    // the raw register is the same finding, not a second one.
    await expect(canvas.getByText('ADR / code drift · 1')).toBeInTheDocument();
    await expect(
      canvas.getByText(/ADR-0002 declares batched sync; observed per-event POST/),
    ).toBeInTheDocument();
    await expect(canvas.getByText('drift:sync-batching')).toBeInTheDocument();
    await expect(
      canvas.queryByText('adr:sync CONFLICTS ch:events'),
    ).not.toBeInTheDocument();
  },
};

export const CleanRegister: Story = {
  args: {
    summary: {
      gaps: 0,
      unsupported: 0,
      no_evidence: 0,
      drift: 0,
      open_findings: 0,
      graph_facts: 134,
    },
    gaps: [],
    drift: [],
    registerFindings: [],
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    // Empty lanes state their emptiness — absence is a claim, not a blank.
    await expect(canvas.getByText('No unresolved facts.')).toBeInTheDocument();
    await expect(canvas.getByText('None detected.')).toBeInTheDocument();
    await expect(canvas.getByText('None recorded.')).toBeInTheDocument();
    await userEvent.click(canvas.getByRole('tab', { name: 'Drift' }));
    await expect(canvas.getByText('No ADR/code conflicts recovered.')).toBeInTheDocument();
  },
};
