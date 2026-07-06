import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import type { GraphNode } from '../store';
import { EndpointsCard } from './EndpointsCard';

export function endpointFixture(method: string, path: string): GraphNode {
  return {
    id: `ep:${method}:${path}`,
    label: 'Endpoint',
    props: {
      method,
      path,
      prov: {
        tier: 'Deterministic',
        confidence_tier: 'Confirmed',
        evidence: [
          { repo: 'local', path: 'src/app.ts', byte_start: 64, byte_end: 92, commit_sha: 'workdir' },
        ],
        extractor_id: 't0.adapter-ts',
        content_hash: 'a'.repeat(64),
      },
    },
  };
}

const meta = {
  title: 'Atlas/EndpointsCard',
  component: EndpointsCard,
  args: { onSelect: fn() },
  // endpointFixture is a shared helper, not a story.
  excludeStories: ['endpointFixture'],
} satisfies Meta<typeof EndpointsCard>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Empty: Story = {
  args: { endpoints: [] },
};

export const Populated: Story = {
  args: {
    endpoints: [endpointFixture('GET', '/users'), endpointFixture('POST', '/users')],
  },
  play: async ({ args, canvasElement }) => {
    const canvas = within(canvasElement);
    // Confidence tier is visible on every row (R-INT-2).
    await expect(canvas.getAllByText('Confirmed')).toHaveLength(2);
    await userEvent.click(canvas.getByText('GET'));
    await expect(args.onSelect).toHaveBeenCalledWith(
      expect.objectContaining({ id: 'ep:GET:/users' }),
    );
  },
};

// Regression for #41: a long route wraps in full — never clipped, never
// ellipsized (the route is the content) — and the tier badge stays inside
// its row (the badge is the R-INT-2 signal).
export const LongPath: Story = {
  args: {
    endpoints: [
      endpointFixture(
        'POST',
        '/api/v2/tenants/:tenantId/workspaces/:workspaceId/members/:memberId/notifications',
      ),
    ],
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const badge = canvas.getByText('Confirmed');
    const row = badge.closest('.endpoint-row')!;
    const badgeBox = badge.getBoundingClientRect();
    const rowBox = row.getBoundingClientRect();
    await expect(badgeBox.right).toBeLessThanOrEqual(rowBox.right + 1);
    await expect(badgeBox.left).toBeGreaterThanOrEqual(rowBox.left - 1);
    // The full route is rendered and not horizontally clipped: it wraps.
    const path = row.querySelector('.endpoint-path')!;
    await expect(path.scrollWidth).toBeLessThanOrEqual(path.clientWidth + 1);
    await expect(path.textContent).toBe(
      '/api/v2/tenants/:tenantId/workspaces/:workspaceId/members/:memberId/notifications',
    );
  },
};
