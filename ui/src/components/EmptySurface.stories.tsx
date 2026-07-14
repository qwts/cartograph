import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, within } from 'storybook/test';
import { EmptySurface } from './EmptySurface';

const meta = {
  title: 'Shell/EmptySurface',
  component: EmptySurface,
  args: {
    icon: 'report',
    title: 'Gaps & Drift',
    description: 'The open-findings register lands with #109.',
  },
} satisfies Meta<typeof EmptySurface>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Interim: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('heading', { name: 'Gaps & Drift' })).toBeInTheDocument();
    await expect(canvas.getByText(/lands with #109/)).toBeInTheDocument();
  },
};
