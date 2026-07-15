import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import type { Flow, FlowHop, Tier } from '../store';
import { FlowsCard, hopKind, projectedDossier, statusBadge } from './FlowsCard';

function hop(
  label: string,
  src: string,
  dst: string,
  srcName: string,
  dstName: string,
  confidence: Tier,
  options: Partial<FlowHop> = {},
): FlowHop {
  const tier =
    confidence === 'Confirmed'
      ? 'Deterministic'
      : confidence === 'InferredStrong'
        ? 'Semantic'
        : confidence === 'InferredWeak'
          ? 'Agentic'
          : 'Deterministic';
  return {
    label,
    src,
    dst,
    src_name: srcName,
    dst_name: dstName,
    tier,
    confidence,
    evidence: 'src/app.ts bytes 10..42',
    provenance: {
      tier,
      confidence_tier: confidence,
      evidence: [{
        repo: 'local/shop',
        path: 'src/app.ts',
        byte_start: 10,
        byte_end: 42,
        commit_sha: 'abc123',
      }],
      extractor_id: 'story.flow',
      content_hash: `${src}:${label}:${dst}`,
    },
    gap_reason: null,
    attempted_tiers: [],
    ...options,
  };
}

const VERIFIED: Flow = {
  trigger: 'ep:POST:/orders',
  trigger_kind: 'Endpoint',
  trigger_name: 'POST /orders',
  hops: [
    hop('HANDLES', 'ep:POST:/orders', 'sym:orders#create', 'POST /orders', 'createOrder', 'Confirmed'),
    hop('CALLS', 'sym:orders#create', 'sym:orders#persist', 'createOrder', 'persistOrder', 'Confirmed'),
  ],
  status: 'Verified',
  score: 1.0,
  depth_limited: false,
};

const PARTIAL: Flow = {
  trigger: 'ep:POST:/notify',
  trigger_kind: 'Endpoint',
  trigger_name: 'POST /notify',
  hops: [
    hop('HANDLES', 'ep:POST:/notify', 'sym:notify#send', 'POST /notify', 'sendNotification', 'Confirmed'),
    hop('CALLS', 'sym:notify#send', 'sym:notify#guess', 'sendNotification', 'guessedTarget', 'InferredWeak'),
    hop(
      'PUBLISHES',
      'sym:notify#send',
      'gap:channel:notify',
      'sendNotification',
      'GAP: runtime-computed channel identity',
      'Gap',
      {
        evidence: 'src/notify.ts bytes 90..128',
        gap_reason: 'runtime-computed channel identity',
        attempted_tiers: ['T0', 'T1', 'T2', 'T3'],
      },
    ),
  ],
  status: 'Partial',
  score: 0.43,
  depth_limited: false,
};

const FLOWS = [VERIFIED, PARTIAL];
const SAMPLE = projectedDossier(FLOWS, 'best-effort');
const BRANCHED: Flow = {
  trigger: 'ep:POST:/branch',
  trigger_kind: 'Endpoint',
  trigger_name: 'POST /branch',
  hops: [
    hop('HANDLES', 'ep:POST:/branch', 'sym:branch#handle', 'POST /branch', 'branchHandler', 'Confirmed'),
    hop('CALLS', 'sym:branch#handle', 'sym:branch#helper', 'branchHandler', 'helper', 'Confirmed'),
    hop('PUBLISHES', 'sym:branch#handle', 'channel:orders', 'branchHandler', 'orders.created', 'Confirmed'),
  ],
  status: 'Verified',
  score: 1,
  depth_limited: false,
};
const UNKNOWN_CONFIDENCE: Flow = {
  ...VERIFIED,
  trigger: 'ep:GET:/unknown',
  trigger_name: 'GET /unknown',
  hops: [
    {
      ...VERIFIED.hops[0],
      src: 'ep:GET:/unknown',
      src_name: 'GET /unknown',
      confidence: 'Unrecognized',
    },
  ],
  status: 'Partial',
  score: 0,
};

const meta = {
  title: 'Atlas/FlowInspector',
  component: FlowsCard,
  args: { flows: FLOWS, dossier: SAMPLE, onSelectHop: fn(), onOpenResolution: fn() },
} satisfies Meta<typeof FlowsCard>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Empty: Story = {
  args: { flows: [], dossier: '# Flow dossier\n' },
  play: async ({ canvasElement }) => {
    await expect(within(canvasElement).getByText(/no flows traced yet/i)).toBeInTheDocument();
  },
};

export const SequenceAndTriggerSelection: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    // #107 header: flow id, status badge with named gap count, trigger
    // summary and score in prose.
    await expect(canvas.getByRole('heading', { level: 2 })).toHaveTextContent(
      'F-0001 · POST /orders',
    );
    await expect(canvas.getByText('VERIFIED')).toHaveClass('tier-confirmed');
    await expect(canvas.getByText(/Flow score 1\.00/)).toBeInTheDocument();
    await expect(canvas.getByRole('status')).toHaveTextContent('2 of 2 hops shown');
    await expect(canvas.getByLabelText('Flow sequence')).toHaveTextContent('createOrder');

    await userEvent.selectOptions(canvas.getByLabelText('Trigger source'), PARTIAL.trigger);
    await expect(canvas.getByRole('heading', { level: 2 })).toHaveTextContent(
      'F-0002 · POST /notify',
    );
    await expect(canvas.getByText('PARTIAL (1 gap)')).toHaveClass('tier-gap');
    await expect(canvas.getByText(/Flow score 0\.43/)).toBeInTheDocument();
    await expect(canvas.getByRole('status')).toHaveTextContent('3 of 3 hops shown');
    await expect(canvas.getByLabelText('Flow sequence')).toHaveTextContent('sendNotification');
    await expect(statusBadge(PARTIAL)).toBe('PARTIAL (1 gap)');
  },
};

export const ExplicitGap: Story = {
  args: { flows: [PARTIAL], dossier: projectedDossier([PARTIAL], 'best-effort') },
  play: async ({ canvasElement }) => {
    const sequence = within(canvasElement).getByLabelText('Flow sequence');
    const gap = within(sequence)
      .getByRole('button', { name: /Unresolved hop/ });
    await expect(gap).toHaveClass('unresolved');
    await expect(gap).toHaveTextContent('runtime-computed channel identity');
    await expect(gap).toHaveTextContent('T0 → T1 → T2 → T3');
    await expect(within(gap).getByText('GAP', { selector: '.tier-badge' })).toBeInTheDocument();
    await expect(hopKind(PARTIAL.hops[2])).toBe('GAP');
  },
};

export const BranchedTraceUsesRecordedEndpoints: Story = {
  args: { flows: [BRANCHED], dossier: projectedDossier([BRANCHED], 'best-effort') },
  play: async ({ canvasElement }) => {
    // Every card and dossier arrow uses the hop's recorded src/dst ids —
    // sequence is never inferred from array position.
    const sequence = within(canvasElement).getByLabelText('Flow sequence');
    const publishes = within(sequence).getByRole('button', { name: /^PUBLISHES:/ });
    await expect(publishes).toHaveTextContent('branchHandler → orders.created');
    await expect(hopKind(BRANCHED.hops[2])).toBe('CHANNEL');

    const dossier = projectedDossier([BRANCHED], 'best-effort');
    await expect(dossier).toContain('p1->>p2: CALLS [Confirmed]');
    await expect(dossier).toContain('p1->>p3: PUBLISHES [Confirmed]');
    await expect(dossier).not.toContain('p2->>p3: PUBLISHES [Confirmed]');
  },
};

export const VerifiedOnlyProjection: Story = {
  args: { flows: [PARTIAL], dossier: projectedDossier([PARTIAL], 'best-effort') },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    // Best-effort names its weak hops instead of hiding the difference.
    await expect(canvas.getByRole('note')).toHaveTextContent(
      /Best-effort: includes 1 InferredWeak hop/,
    );
    // The note's tone must actually resolve (#144 review): a typo'd CSS
    // token invalidates the declaration and silently falls back to the
    // confirmed-green base, marking inferred content as confirmed.
    const bestEffortBorder = getComputedStyle(canvas.getByRole('note')).borderColor;

    await userEvent.click(canvas.getByRole('button', { name: 'verified-only' }));
    await expect(
      getComputedStyle(canvas.getByRole('note')).borderColor,
    ).not.toBe(bestEffortBorder);
    // #107: the excluded hop stays visible as an annotated, non-interactive
    // card; the count and the note make the projection difference explicit,
    // and the Gap node is retained (R-INT-4).
    await expect(canvas.getByRole('status')).toHaveTextContent('2 of 3 hops shown');
    await expect(canvas.getByRole('note')).toHaveTextContent(
      /Verified-only: InferredWeak hops are excluded \(1 hidden\), but the Gap node is retained/,
    );
    const excludedNote = canvas.getByText('Excluded in verified-only — InferredWeak');
    // --tier-inferred-weak resolved, not fallen back to inherited text color.
    await expect(getComputedStyle(excludedNote).color).toBe('rgb(242, 201, 76)');
    const excluded = excludedNote.closest('.flow-hop-card');
    await expect(excluded).toHaveClass('excluded');
    await expect(excluded).toHaveAttribute('aria-disabled', 'true');
    await expect(excluded?.tagName).not.toBe('BUTTON');
    await expect(
      canvas.getByRole('button', { name: /Unresolved hop/ }),
    ).toBeInTheDocument();

    await userEvent.click(canvas.getByText(/Mermaid \+ provenance dossier/));
    const dossier = canvas.getByTestId('flows-dossier');
    await expect(dossier).not.toHaveTextContent('guessedTarget');
    await expect(dossier).toHaveTextContent('PUBLISHES [Gap]');
  },
};

export const GapHopOpensResolutionStrategy: Story = {
  args: { flows: [PARTIAL], dossier: projectedDossier([PARTIAL], 'best-effort') },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // Gap hop → Resolution Strategy with the Gap node id; a confirmed hop →
    // evidence drawer with the hop itself (#107).
    await userEvent.click(canvas.getByRole('button', { name: /Unresolved hop/ }));
    await expect(args.onOpenResolution).toHaveBeenCalledWith('gap:channel:notify');
    await expect(args.onSelectHop).not.toHaveBeenCalled();

    await userEvent.click(
      canvas.getByRole('button', { name: 'HANDLES: POST /notify to sendNotification' }),
    );
    await expect(args.onSelectHop).toHaveBeenCalledWith(PARTIAL.hops[0]);
  },
};

export const FitAndZoomNeverScrollHorizontally: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const row = canvas.getByLabelText('Flow sequence');
    const noOverflow = () => row.scrollWidth <= row.clientWidth + 1;
    await expect(noOverflow()).toBe(true);

    await userEvent.click(canvas.getByRole('button', { name: 'Zoom in' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Zoom in' }));
    await expect(noOverflow()).toBe(true);

    await userEvent.click(canvas.getByRole('button', { name: 'Fit hops to view' }));
    await expect(noOverflow()).toBe(true);
    await userEvent.click(canvas.getByRole('button', { name: 'Zoom out' }));
    await expect(noOverflow()).toBe(true);
  },
};

export const UnknownConfidenceFailsClosed: Story = {
  args: { flows: [UNKNOWN_CONFIDENCE], dossier: null },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const sequence = canvas.getByLabelText('Flow sequence');
    const card = within(sequence).getByRole('button', { name: /Unresolved hop/ });
    await expect(card).toHaveClass('unresolved');
    await expect(card).toHaveTextContent('confidence metadata missing or unrecognized');
    // Fail-closed is not escalatable: the destination is not a Gap node, so
    // the click shows evidence instead of a Resolution Strategy.
    await userEvent.click(card);
    await expect(args.onOpenResolution).not.toHaveBeenCalled();
    await expect(args.onSelectHop).toHaveBeenCalled();
  },
};
