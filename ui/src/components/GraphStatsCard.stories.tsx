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
  // AC-0085 (#162): the destructive action speaks in system terms and the
  // confirmation names exactly what is removed and what survives.
  args: {
    stats: { nodes: 1284, edges: 4021 },
    systemContents: [
      { repo: 'acme/shop', commit: 'a1b2c3d4e5f6' },
      { repo: 'local/infra', commit: 'workdir' },
    ],
  },
  play: async ({ args, canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Clear system' }));
    await expect(canvas.getByRole('alert')).toHaveTextContent(
      'Remove every recovered fact for acme/shop, local/infra? Job history and settings are kept.',
    );
    await userEvent.click(canvas.getByRole('button', { name: 'Keep system' }));
    await expect(canvas.queryByRole('alert')).not.toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Clear system' }));
    await userEvent.click(canvas.getByRole('button', { name: 'Confirm clear' }));
    await expect(args.onClear).toHaveBeenCalledOnce();
  },
};

export const PopulatedWithoutContents: Story = {
  // Degrades against a core without system_contents: system phrasing stays,
  // the repo list is simply absent.
  args: { stats: { nodes: 12, edges: 18 } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Clear system' }));
    await expect(canvas.getByRole('alert')).toHaveTextContent(
      'Remove every recovered fact in this system? Job history and settings are kept.',
    );
  },
};

export const Empty: Story = {
  args: { stats: { nodes: 0, edges: 0 }, canClear: false },
};

export const ClearFailed: Story = {
  args: { stats: { nodes: 12, edges: 18 }, error: 'storage: database is locked' },
};
