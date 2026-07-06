import type { Meta, StoryObj } from '@storybook/react-vite';
import { GraphStatsCard } from './GraphStatsCard';

const meta = {
  title: 'Shell/GraphStatsCard',
  component: GraphStatsCard,
} satisfies Meta<typeof GraphStatsCard>;

export default meta;
type Story = StoryObj<typeof meta>;

export const NoBackend: Story = {
  args: { stats: null },
};

export const Populated: Story = {
  args: { stats: { nodes: 1284, edges: 4021 } },
};

export const Empty: Story = {
  args: { stats: { nodes: 0, edges: 0 } },
};
