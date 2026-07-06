import type { Meta, StoryObj } from '@storybook/react-vite';
import { TierBadge } from './TierBadge';

const meta = {
  title: 'Atlas/TierBadge',
  component: TierBadge,
} satisfies Meta<typeof TierBadge>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Confirmed: Story = { args: { tier: 'Confirmed' } };
export const InferredStrong: Story = { args: { tier: 'InferredStrong' } };
export const InferredWeak: Story = { args: { tier: 'InferredWeak' } };
export const Gap: Story = { args: { tier: 'Gap' } };
