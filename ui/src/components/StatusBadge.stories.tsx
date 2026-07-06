import type { Meta, StoryObj } from '@storybook/react-vite';
import { StatusBadge } from './StatusBadge';

const meta = {
  title: 'Shell/StatusBadge',
  component: StatusBadge,
} satisfies Meta<typeof StatusBadge>;

export default meta;
type Story = StoryObj<typeof meta>;

export const CoreUp: Story = {
  args: { backend: 'up', version: '0.0.1' },
};

export const BrowserPreview: Story = {
  args: { backend: 'browser', version: null },
};

export const Connecting: Story = {
  args: { backend: 'unknown', version: null },
};
