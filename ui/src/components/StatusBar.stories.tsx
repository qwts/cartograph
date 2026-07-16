import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { StatusBar } from './StatusBar';

const meta = {
  title: 'Shell/StatusBar',
  component: StatusBar,
  args: {
    status: 'Ready',
    busy: false,
    egress: 'Local-only · 0 bytes egress',
  },
} satisfies Meta<typeof StatusBar>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Idle: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('status')).toHaveTextContent('Ready');
    await expect(canvas.getByText('Local-only · 0 bytes egress')).toBeInTheDocument();
    await expect(canvasElement.querySelector('.spinning')).not.toBeInTheDocument();
  },
};

export const Ingesting: Story = {
  args: { status: 'Ingesting…', busy: true },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('status')).toHaveTextContent('Ingesting…');
    await expect(canvasElement.querySelector('.spinning')).toBeInTheDocument();
    // No onOpenRecovery: nothing to jump back to, so it's not a button.
    await expect(canvas.queryByRole('button')).not.toBeInTheDocument();
  },
};

export const IngestingIsClickableBackToRecover: Story = {
  // AC-0094: while a real ingest is running, "Ingesting…" is the way back
  // to the Recovering screen after navigating away or backgrounding it.
  args: { status: 'Ingesting…', busy: true, onOpenRecovery: fn() },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const link = canvas.getByRole('button', { name: /Ingesting…/ });
    await userEvent.click(link);
    await expect(args.onOpenRecovery).toHaveBeenCalledTimes(1);
  },
};
