import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, within } from 'storybook/test';
import { TopologyCard } from './TopologyCard';

const SAMPLE = `flowchart LR
    res_aws_lambda_function_fulfill["aws_lambda_function.fulfill"]
    res_aws_sqs_queue_orders["aws_sqs_queue.orders"]
    res_aws_sqs_queue_orders -->|TRIGGERS| res_aws_lambda_function_fulfill
`;

const meta = {
  title: 'Atlas/TopologyCard',
  component: TopologyCard,
} satisfies Meta<typeof TopologyCard>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Empty: Story = {
  args: { mermaid: 'flowchart LR\n' },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/no resources recovered yet/i)).toBeInTheDocument();
  },
};

export const NoBackend: Story = {
  args: { mermaid: null },
};

export const Populated: Story = {
  args: { mermaid: SAMPLE },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const pre = canvas.getByTestId('topology-mermaid');
    await expect(pre.textContent).toContain('flowchart LR');
    await expect(pre.textContent).toContain('-->|TRIGGERS|');
    await expect(canvas.getByRole('button', { name: /copy mermaid/i })).toBeEnabled();
  },
};
