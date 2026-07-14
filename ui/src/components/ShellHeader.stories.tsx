import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { ShellHeader } from './ShellHeader';

const meta = {
  title: 'Shell/ShellHeader',
  component: ShellHeader,
  args: {
    system: 'image-trail',
    surface: 'Atlas',
    scope: { kind: 'system' as const, label: 'Whole system' },
    onShowLegend: fn(),
  },
} satisfies Meta<typeof ShellHeader>;

export default meta;
type Story = StoryObj<typeof meta>;

export const WholeSystem: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('image-trail')).toBeInTheDocument();
    await expect(canvas.getByText('Atlas')).toBeInTheDocument();
    await expect(canvas.getByText('Whole system')).toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: /legend/i }));
    await expect(args.onShowLegend).toHaveBeenCalled();
  },
};

export const EvidenceTrailScope: Story = {
  args: { scope: { kind: 'trail', label: 'Single evidence trail' } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Single evidence trail')).toBeInTheDocument();
  },
};

export const NoSystemYet: Story = {
  args: { system: null, surface: 'Workspace' },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('No system')).toBeInTheDocument();
  },
};
