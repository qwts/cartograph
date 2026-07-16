import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { CommandPalette } from './CommandPalette';

const meta = {
  title: 'Shell/CommandPalette',
  component: CommandPalette,
  args: {
    open: true,
    onClose: fn(),
    onNavigate: fn(),
  },
} satisfies Meta<typeof CommandPalette>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Open: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const dialog = canvas.getByRole('dialog', { name: 'Command palette' });
    // Eight surfaces plus the Help action (#154).
    await expect(within(dialog).getAllByRole('option')).toHaveLength(9);
    await expect(within(dialog).getByText('Help')).toBeInTheDocument();

    // Click navigates and closes.
    await userEvent.click(within(dialog).getByRole('option', { name: /Gaps & Drift/ }));
    await expect(args.onNavigate).toHaveBeenCalledWith('gaps');
    await expect(args.onClose).toHaveBeenCalled();
  },
};

export const KeyboardDriven: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const list = canvas.getByRole('listbox', { name: 'Surfaces' });
    list.focus();
    // Arrow down twice from Workspace → Flows, Enter selects.
    await userEvent.keyboard('{ArrowDown}{ArrowDown}{Enter}');
    await expect(args.onNavigate).toHaveBeenCalledWith('flows');
  },
};

export const EscapeCloses: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    canvas.getByRole('listbox', { name: 'Surfaces' }).focus();
    await userEvent.keyboard('{Escape}');
    await expect(args.onClose).toHaveBeenCalled();
    await expect(args.onNavigate).not.toHaveBeenCalled();
  },
};

export const Closed: Story = {
  args: { open: false },
  play: async ({ canvasElement }) => {
    await expect(canvasElement.querySelector('.cmdk')).not.toBeInTheDocument();
  },
};
