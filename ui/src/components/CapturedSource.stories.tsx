import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { CapturedSource } from './CapturedSource';
import type { CapturedDescription } from '../primarySourceStore';

const captureDescription: CapturedDescription = {
  fact: { kind: 'node', id: 'rule:local/src_example@rules.ts#exit' },
  receipt_id: 'ts-primary-v1:fixture', emitted_fact_digest: 'fact-digest',
  source_id: 'src_example', repo_key: 'local/src_example',
  scope: 'primary_source_only', input_closure: 'input_closure_not_established',
  ranges: [{ index: 0, path: 'rules.ts', byte_start: 30, byte_end: 43 }],
};

const meta = {
  title: 'Atlas/CapturedSource', component: CapturedSource,
  args: { description: captureDescription, text: null, loading: false, error: null, onRead: fn() },
} satisfies Meta<typeof CapturedSource>;
export default meta;
type Story = StoryObj<typeof meta>;

export const SelectsStoredRange: Story = {
  // AC-0154: a stored range index and receipt bind inspection, not caller offsets.
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/Full input coverage and business interpretation are not established/)).toBeVisible();
    await userEvent.selectOptions(canvas.getByLabelText('Captured cited range'), '0');
    await expect(args.onRead).toHaveBeenCalledWith(0);
  },
};

export const OriginalSource: Story = {
  args: { text: { receipt_id: captureDescription.receipt_id, range_index: 0, text: 'return false;', path: 'rules.ts', byte_start: 30, byte_end: 43 } },
  play: async ({ canvasElement }) => {
    await expect(within(canvasElement).getByTestId('captured-source-text')).toHaveTextContent('return false;');
  },
};

export const DifferentReceiptNeverRenders: Story = {
  // AC-0154: even a stale parent payload cannot display another receipt's text.
  args: { text: { receipt_id: 'another-receipt', range_index: 0, text: 'WRONG SOURCE', path: 'rules.ts', byte_start: 30, byte_end: 43 } },
  play: async ({ canvasElement }) => {
    await expect(within(canvasElement).queryByText('WRONG SOURCE')).not.toBeInTheDocument();
  },
};

export const ForgottenSource: Story = {
  args: { error: 'Captured source is unavailable. Receipt and review history is preserved.' },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('status')).toHaveTextContent('Captured source is unavailable');
    await expect(canvas.queryByTestId('captured-source-text')).not.toBeInTheDocument();
  },
};
