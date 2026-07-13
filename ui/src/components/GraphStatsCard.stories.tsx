import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { GraphStatsCard } from './GraphStatsCard';

const meta = {
  title: 'Shell/GraphStatsCard',
  component: GraphStatsCard,
  args: { canClear: true, clearing: false, error: null, onClear: fn() },
} satisfies Meta<typeof GraphStatsCard>;

export default meta;
type Story = StoryObj<typeof meta>;

export const NoBackend: Story = {
  args: { stats: null, canClear: false },
};

export const Populated: Story = {
  args: { stats: { nodes: 1284, edges: 4021 } },
  play: async ({ args, canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Clear graph' }));
    await expect(canvas.getByRole('alert')).toHaveTextContent(
      'Clear all graph facts? Job history will be kept.',
    );
    await userEvent.click(canvas.getByRole('button', { name: 'Keep graph' }));
    await expect(canvas.queryByRole('alert')).not.toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Clear graph' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Confirm clear' }));
    await expect(args.onClear).toHaveBeenCalledOnce();
  },
};

export const Empty: Story = {
  args: { stats: { nodes: 0, edges: 0 }, canClear: false },
};

export const ClearFailed: Story = {
  args: { stats: { nodes: 12, edges: 18 }, error: 'storage: database is locked' },
};
