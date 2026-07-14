import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { NavRail } from './NavRail';

const meta = {
  title: 'Shell/NavRail',
  component: NavRail,
  args: {
    active: 'workspace',
    onNavigate: fn(),
    onOpenPalette: fn(),
  },
} satisfies Meta<typeof NavRail>;

export default meta;
type Story = StoryObj<typeof meta>;

export const AllSurfaces: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // Every icon-only control is reachable by accessible name (handoff #4).
    for (const label of [
      'Workspace',
      'Atlas',
      'Flows',
      'Spec Workbench',
      'Gaps & Drift',
      'Provenance & Eval',
      'Jobs',
      'Settings',
      'Command palette',
    ]) {
      await expect(canvas.getByRole('button', { name: label })).toBeInTheDocument();
    }
    // The active surface is exposed as the current page.
    await expect(canvas.getByRole('button', { name: 'Workspace' })).toHaveAttribute(
      'aria-current',
      'page',
    );

    await userEvent.click(canvas.getByRole('button', { name: 'Atlas' }));
    await expect(args.onNavigate).toHaveBeenCalledWith('atlas');
    await userEvent.click(canvas.getByRole('button', { name: 'Command palette' }));
    await expect(args.onOpenPalette).toHaveBeenCalled();
  },
};

export const AtlasActive: Story = {
  args: { active: 'atlas' },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('button', { name: 'Atlas' })).toHaveAttribute(
      'aria-current',
      'page',
    );
    await expect(canvas.getByRole('button', { name: 'Workspace' })).not.toHaveAttribute(
      'aria-current',
    );
  },
};
