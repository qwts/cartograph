import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, waitFor, within } from 'storybook/test';
import { assignBands, buildAtlasScene, clusterKeyFor } from '../atlasLayout';
import type { AtlasSnapshot, GraphNode, Provenance, Tier } from '../store';
import { AtlasCanvas, focusAtlasGraph, nodeShapeClass } from './AtlasCanvas';

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
  args: { snapshot: atlasFixture, onSelect: fn(), onSelectEdge: fn(), onLayerChange: fn() },
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

export const FocusModeRootsAndBacksOut: Story = {
  // AC-0086 (#160): Enter/CTA roots the selection to its ego graph, each
  // level stacks, Esc backs out exactly one level, ending at the full graph.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'POST /orders' }));
    await userEvent.click(canvas.getByRole('button', { name: /Focus on POST \/orders/ }));
    // Ego graph: the endpoint plus its direct connections (channel, gap,
    // Checkout) — the SQS Resource is two hops away and disappears from
    // the projection and the entity index.
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('4 nodes · 3 edges'));
    const path = canvas.getByRole('navigation', { name: 'Focus path' });
    await expect(within(path).getByText('▸ POST /orders')).toBeInTheDocument();
    await expect(canvas.queryByRole('button', { name: 'orders' })).not.toBeInTheDocument();

    // A second level roots the channel within the first projection.
    await userEvent.click(canvas.getByRole('button', { name: 'orders queue' }));
    await userEvent.click(canvas.getByRole('button', { name: /Focus on orders queue/ }));
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('2 nodes · 1 edges'));

    // Esc pops one level at a time — never straight to the full graph.
    await userEvent.keyboard('{Escape}');
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('4 nodes · 3 edges'));
    await userEvent.keyboard('{Escape}');
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('5 nodes · 4 edges'));
  },
};

export const FocusIsKeyboardOperableAndAnnounced: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'POST /orders' }));
    await expect(
      canvas.getByText(/POST \/orders selected — press Enter to focus/),
    ).toBeInTheDocument();

    // Selection hands the keyboard to the canvas (#190 review): the next
    // Enter focuses immediately — no manual tabbing to the canvas needed.
    await expect(document.activeElement?.className).toContain('atlas-cytoscape');
    await userEvent.keyboard('{Enter}');
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('4 nodes · 3 edges'));
    await expect(
      canvas.getByText(/Focused on POST \/orders — 4 nodes at level 1\. Esc backs out/),
    ).toBeInTheDocument();

    // The breadcrumb also backs out without a keyboard.
    await userEvent.click(canvas.getByRole('button', { name: 'Full graph' }));
    await waitFor(() => expect(canvas.getByRole('status')).toHaveTextContent('5 nodes · 4 edges'));

    // The projection is a pure function: same inputs, same ego graph,
    // regardless of input order; an unknown root changes nothing.
    const ego = focusAtlasGraph(atlasFixture, atlasNodes[2].id);
    const reversed = focusAtlasGraph(
      { nodes: [...atlasFixture.nodes].reverse(), edges: [...atlasFixture.edges].reverse() },
      atlasNodes[2].id,
    );
    await expect(new Set(reversed.nodes.map((n) => n.id))).toEqual(
      new Set(ego.nodes.map((n) => n.id)),
    );
    await expect(ego.nodes.map((n) => n.id).sort()).toEqual(
      [atlasNodes[1].id, atlasNodes[2].id, atlasNodes[3].id, atlasNodes[4].id].sort(),
    );
    await expect(focusAtlasGraph(atlasFixture, 'missing:node')).toEqual(atlasFixture);
  },
};

export const ShapeEncodesKindNeverColorAlone: Story = {
  // #106: octagon dashed red = Gap (no longer colliding with the gateway
  // diamond), diamond = channel/gateway, rectangle = everything else.
  play: async ({ canvasElement }) => {
    await expect(nodeShapeClass(atlasNodes[4])).toBe('atlas-gap');
    await expect(nodeShapeClass(atlasNodes[1])).toBe('kind-channel');
    await expect(
      nodeShapeClass({ id: 'gw:api', label: 'Gateway', props: {} }),
    ).toBe('kind-channel');
    await expect(nodeShapeClass(atlasNodes[2])).toBe('kind-box');
    // The canvas itself rendered (shape classes feed Cytoscape styles).
    await expect(
      within(canvasElement).getByTestId('atlas-canvas'),
    ).toBeInTheDocument();
  },
};

export const EdgeChipsOpenEdgeEvidence: Story = {
  // #106: every edge carries a mono tier+relation chip and is clickable —
  // the chip names the producing tier, GAP edges say so.
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const relations = within(canvas.getByLabelText('Visible relations'));
    await expect(relations.getByText('T0 BACKS')).toBeInTheDocument();
    await expect(relations.getByText('T2 PUBLISHES')).toBeInTheDocument();
    await expect(relations.getByText('T3 FETCHES')).toBeInTheDocument();
    await expect(relations.getByText('GAP CALLS')).toBeInTheDocument();

    await userEvent.click(relations.getByText('GAP CALLS'));
    await expect(args.onSelectEdge).toHaveBeenCalledWith(atlasFixture.edges[3]);
  },
};

export const LayerDrivesScopeChip: Story = {
  // #106: the layer filter reports its label so the header scope chip can
  // read `Atlas · <layer>`.
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Events' }));
    await expect(args.onLayerChange).toHaveBeenCalledWith('Events');
    await userEvent.click(canvas.getByRole('button', { name: 'All layers' }));
    await expect(args.onLayerChange).toHaveBeenCalledWith('All layers');
  },
};

export const BandedLayoutIsDeterministic: Story = {
  // #159/AC-0081: bands come from kind (Gaps inherit from a neighbor),
  // cluster keys are id-derived, and positions are identical for identical
  // snapshots regardless of input order — layout never jitters.
  play: async () => {
    const bands = assignBands(atlasFixture);
    await expect(bands.get(atlasNodes[0].id)).toBe('infra'); // Resource
    await expect(bands.get(atlasNodes[1].id)).toBe('events'); // Channel
    await expect(bands.get(atlasNodes[2].id)).toBe('server'); // Endpoint
    await expect(bands.get(atlasNodes[3].id)).toBe('client'); // Component
    // The Gap anchors to its calling Endpoint's band, never floats alone.
    await expect(bands.get(atlasNodes[4].id)).toBe('server');

    await expect(clusterKeyFor(atlasNodes[0])).toBe('local/shop · aws_sqs_queue');
    await expect(clusterKeyFor(atlasNodes[1])).toBe('sqs-queue');
    // Routed screen ids carry a leading slash — the first real segment
    // names the cluster, and the root route stays legible (#173 review).
    await expect(
      clusterKeyFor({ id: 'screen:local/shop@/users/[id]', label: 'Screen', props: {} }),
    ).toBe('local/shop · users');
    await expect(
      clusterKeyFor({ id: 'screen:local/shop@/', label: 'Screen', props: {} }),
    ).toBe('local/shop · /');

    const shuffled: AtlasSnapshot = {
      nodes: [...atlasFixture.nodes].reverse(),
      edges: [...atlasFixture.edges].reverse(),
    };
    const a = buildAtlasScene(atlasFixture, new Set());
    const b = buildAtlasScene(shuffled, new Set());
    const positions = (scene: typeof a) =>
      scene.nodes.map((item) => `${item.id}@${item.position.x},${item.position.y}`).sort();
    await expect(positions(b)).toEqual(positions(a));
    await expect(a.bands.map((band) => band.label)).toEqual([
      'Infrastructure',
      'Server',
      'Events',
      'Client',
    ]);
  },
};

const SCALE_NODES: GraphNode[] = [
  ...Array.from({ length: 150 }, (_, index) => ({
    id: `sym:local/shop@src/app_${index}.ts#fn${index}`,
    label: 'Symbol' as const,
    props: { name: `fn${index}`, prov: prov('Confirmed', `src/app_${index}.ts`) },
  })),
  ...Array.from({ length: 90 }, (_, index) => ({
    id: `res:local/shop@aws_lambda_function.worker_${index}`,
    label: 'Resource' as const,
    props: { logical_id: `worker_${index}`, prov: prov('Confirmed', 'infra.tf') },
  })),
];

export const ClustersCollapseAtScale: Story = {
  // #159/AC-0081: past the threshold the initial view is collapsed clusters
  // that expand on demand — reading never requires manual arrangement.
  args: {
    snapshot: { nodes: SCALE_NODES, edges: [] },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('status')).toHaveTextContent(
      '240 nodes · 0 edges · 2 of 2 clusters collapsed',
    );

    // The cluster index expands one cluster in place.
    const index = within(canvas.getByLabelText('Collapsed clusters'));
    await userEvent.click(index.getByRole('button', { name: /Infrastructure · local\/shop · aws_lambda_function · 90/ }));
    await waitFor(() =>
      expect(canvas.getByRole('status')).toHaveTextContent('1 of 2 clusters collapsed'),
    );

    // Expand all, then collapse back — no manual arrangement anywhere.
    await userEvent.click(canvas.getByRole('button', { name: 'Expand all clusters' }));
    await waitFor(() =>
      expect(canvas.getByRole('status')).toHaveTextContent('0 of 2 clusters collapsed'),
    );
    await userEvent.click(canvas.getByRole('button', { name: 'Collapse clusters' }));
    await waitFor(() =>
      expect(canvas.getByRole('status')).toHaveTextContent('2 of 2 clusters collapsed'),
    );
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
