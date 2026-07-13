import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, userEvent, within } from 'storybook/test';
import type { Flow, FlowHop, Tier } from '../store';
import { FlowsCard, flowElements, projectedDossier } from './FlowsCard';

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
  args: { flows: FLOWS, dossier: SAMPLE },
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
    await expect(canvas.getByRole('status')).toHaveTextContent('2 of 2 hops shown');
    await expect(canvas.getByText('Verified')).toHaveClass('tier-confirmed');
    await expect(canvas.getByText('score 1.00')).toBeInTheDocument();
    await expect(canvas.getByLabelText('Flow sequence')).toHaveTextContent('createOrder');
    await userEvent.selectOptions(canvas.getByLabelText('Trigger source'), PARTIAL.trigger);
    await expect(canvas.getByRole('status')).toHaveTextContent('3 of 3 hops shown');
    await expect(canvas.getByText('Partial')).toHaveClass('tier-gap');
    await expect(canvas.getByText('score 0.43')).toBeInTheDocument();
    await expect(canvas.getByLabelText('Flow sequence')).toHaveTextContent('sendNotification');
    await expect(canvas.getByTestId('flow-inspector-canvas')).toBeInTheDocument();
  },
};

export const ExplicitGap: Story = {
  args: { flows: [PARTIAL], dossier: projectedDossier([PARTIAL], 'best-effort') },
  play: async ({ canvasElement }) => {
    const sequence = within(canvasElement).getByLabelText('Flow sequence');
    const gap = within(sequence).getByText(/Unresolved hop/).closest('li');
    await expect(gap).toHaveClass('unresolved');
    await expect(gap).toHaveTextContent('runtime-computed channel identity');
    await expect(gap).toHaveTextContent('T0 → T1 → T2 → T3');
    await expect(gap).toHaveTextContent('T0 · Gap');
  },
};

export const BranchedTraceUsesRecordedEndpoints: Story = {
  args: { flows: [BRANCHED], dossier: projectedDossier([BRANCHED], 'best-effort') },
  play: async ({ canvasElement }) => {
    const graph = flowElements(BRANCHED);
    const handler = graph.nodes.find(
      (node) => node.data.kind === 'entity' && node.data.entityId === 'sym:branch#handle',
    );
    const channel = graph.nodes.find(
      (node) => node.data.kind === 'entity' && node.data.entityId === 'channel:orders',
    );
    const publishes = graph.edges.find((edge) => edge.label?.toString().startsWith('PUBLISHES'));
    await expect(publishes).toMatchObject({ source: handler?.id, target: channel?.id });

    const dossier = projectedDossier([BRANCHED], 'best-effort');
    await expect(dossier).toContain('p1->>p2: CALLS [Confirmed]');
    await expect(dossier).toContain('p1->>p3: PUBLISHES [Confirmed]');
    await expect(dossier).not.toContain('p2->>p3: PUBLISHES [Confirmed]');
    await expect(within(canvasElement).getByLabelText('Flow sequence')).toHaveTextContent(
      'branchHandler → orders.created',
    );
  },
};

export const VerifiedOnlyProjection: Story = {
  args: { flows: [PARTIAL], dossier: projectedDossier([PARTIAL], 'best-effort') },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByLabelText('Flow sequence')).toHaveTextContent('guessedTarget');
    await userEvent.click(canvas.getByRole('button', { name: 'verified-only' }));
    await expect(canvas.getByRole('status')).toHaveTextContent('2 of 3 hops shown');
    await expect(canvas.getByLabelText('Flow sequence')).not.toHaveTextContent('guessedTarget');
    await expect(canvas.getByLabelText('Flow sequence')).toHaveTextContent('runtime-computed channel identity');
    await userEvent.click(canvas.getByText(/Mermaid \+ provenance dossier/));
    const dossier = canvas.getByTestId('flows-dossier');
    await expect(dossier).not.toHaveTextContent('guessedTarget');
    await expect(dossier).toHaveTextContent('PUBLISHES [Gap]');
  },
};

export const UnknownConfidenceFailsClosed: Story = {
  args: { flows: [UNKNOWN_CONFIDENCE], dossier: null },
  play: async ({ canvasElement }) => {
    const sequence = within(canvasElement).getByLabelText('Flow sequence');
    await expect(sequence).toHaveTextContent('T0 · Gap');
    await expect(sequence).toHaveTextContent('confidence metadata missing or unrecognized');
    await expect(sequence.querySelector('li')).toHaveClass('unresolved');
  },
};
