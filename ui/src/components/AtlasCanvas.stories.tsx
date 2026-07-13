import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, waitFor, within } from 'storybook/test';
import type { AtlasSnapshot, GraphNode, Provenance, Tier } from '../store';
import { AtlasCanvas } from './AtlasCanvas';

function prov(confidence_tier: Tier, path: string): Provenance {
  return {
    tier:
      confidence_tier === 'Confirmed'
        ? 'Deterministic'
        : confidence_tier === 'InferredStrong'
          ? 'Semantic'
          : confidence_tier === 'InferredWeak'
            ? 'Agentic'
            : 'Deterministic',
    confidence_tier,
    evidence: [
      {
        repo: 'local/shop',
        path,
        byte_start: 0,
        byte_end: 12,
        commit_sha: 'abc123',
      },
    ],
    extractor_id: 'atlas.story',
    content_hash: `${confidence_tier}-${path}`,
  };
}

const atlasNodes: GraphNode[] = [
  {
    id: 'res:local/shop@aws_sqs_queue.orders',
    label: 'Resource',
    props: { type: 'aws_sqs_queue', logical_id: 'orders', prov: prov('Confirmed', 'infra.tf') },
  },
  {
    id: 'chan:sqs-queue:orders',
    label: 'Channel',
    props: { identity: 'orders queue', prov: prov('InferredStrong', 'publisher.ts') },
  },
  {
    id: 'ep:local/shop@POST:/orders',
    label: 'Endpoint',
    props: { method: 'POST', path: '/orders', prov: prov('Confirmed', 'api.ts') },
  },
  {
    id: 'component:local/shop@Checkout',
    label: 'Component',
    props: { name: 'Checkout', prov: prov('InferredWeak', 'Checkout.tsx') },
  },
  {
    id: 'gap:local/shop@orders-target',
    label: 'Gap',
    props: { reason: 'unresolved call target', prov: prov('Gap', 'api.ts') },
  },
];

const atlasFixture: AtlasSnapshot = {
  nodes: atlasNodes,
  edges: [
    {
      src: atlasNodes[0].id,
      dst: atlasNodes[1].id,
      label: 'BACKS',
      props: { prov: prov('Confirmed', 'state.json') },
    },
    {
      src: atlasNodes[2].id,
      dst: atlasNodes[1].id,
      label: 'PUBLISHES',
      props: { prov: prov('InferredStrong', 'publisher.ts') },
    },
    {
      src: atlasNodes[3].id,
      dst: atlasNodes[2].id,
      label: 'FETCHES',
      props: { prov: prov('InferredWeak', 'Checkout.tsx') },
    },
    {
      src: atlasNodes[2].id,
      dst: atlasNodes[4].id,
      label: 'CALLS',
      props: { prov: prov('Gap', 'api.ts') },
    },
  ],
};

const meta = {
  title: 'Atlas/AtlasCanvas',
  component: AtlasCanvas,
  args: { snapshot: atlasFixture, onSelect: fn() },
} satisfies Meta<typeof AtlasCanvas>;

export default meta;
type Story = StoryObj<typeof meta>;

export const LayerFilters: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('status')).toHaveTextContent('5 nodes · 4 edges');

    await userEvent.click(canvas.getByRole('button', { name: 'Infrastructure' }));
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('1 nodes · 0 edges'));
    await expect(canvas.getByRole('button', { name: 'orders' })).toBeInTheDocument();
    await expect(canvas.queryByRole('button', { name: 'orders queue' })).not.toBeInTheDocument();

    await userEvent.click(canvas.getByRole('button', { name: 'Events' }));
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('3 nodes · 2 edges'));
    await expect(canvas.getByRole('button', { name: 'orders queue' })).toBeInTheDocument();
  },
};

export const ConfidenceOverlay: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    for (const label of ['Confirmed', 'Inferred strong', 'Inferred weak', 'Gap']) {
      await expect(canvas.getByText(label)).toBeInTheDocument();
    }
    const toggle = canvas.getByRole('button', { name: 'Confidence overlay' });
    await expect(toggle).toHaveAttribute('aria-pressed', 'true');
    await userEvent.click(toggle);
    await expect(toggle).toHaveAttribute('aria-pressed', 'false');
  },
};

export const NodeSelection: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'POST /orders' }));
    await expect(args.onSelect).toHaveBeenCalledWith(atlasNodes[2]);
  },
};

/** Manual MT-M9-01 scale fixture; excluded from the per-PR browser suite. */
export const TenThousandNodeScale: Story = {
  tags: ['!test'],
  args: {
    snapshot: {
      nodes: Array.from({ length: 10_000 }, (_, index) => ({
        id: `res:scale@aws_lambda_function.worker_${index}`,
        label: 'Resource',
        props: {
          type: 'aws_lambda_function',
          logical_id: `worker_${index}`,
          prov: prov('Confirmed', `modules/worker_${index}.tf`),
        },
      })),
      edges: [],
    },
  },
};
