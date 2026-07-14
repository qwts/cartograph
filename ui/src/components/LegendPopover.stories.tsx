import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { LegendPopover } from './LegendPopover';

const meta = {
  title: 'Shell/LegendPopover',
  component: LegendPopover,
  args: { open: true, onClose: fn() },
} satisfies Meta<typeof LegendPopover>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Open: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const dialog = canvas.getByRole('dialog', { name: 'Tier, shape, and edge legend' });
    // The non-color-alone key: every tier appears as code + label + method.
    for (const code of ['T0/T1', 'T2', 'T3', 'GAP']) {
      await expect(within(dialog).getByText(code)).toBeInTheDocument();
    }
    await expect(within(dialog).getByText(/Octagon/)).toBeInTheDocument();
    await userEvent.click(within(dialog).getByRole('button', { name: 'Close legend' }));
    await expect(args.onClose).toHaveBeenCalled();
  },
};
