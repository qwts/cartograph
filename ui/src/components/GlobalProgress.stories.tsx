import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, within } from 'storybook/test';
import { GlobalProgress } from './GlobalProgress';

const meta = {
  title: 'Shell/GlobalProgress',
  component: GlobalProgress,
  args: { active: true },
} satisfies Meta<typeof GlobalProgress>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Running: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByRole('progressbar', { name: 'Background work running' }),
    ).toBeInTheDocument();
  },
};

export const Idle: Story = {
  args: { active: false },
  play: async ({ canvasElement }) => {
    await expect(canvasElement.querySelector('.global-progress')).not.toBeInTheDocument();
  },
};
