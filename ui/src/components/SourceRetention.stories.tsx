import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { SourceRetention } from './SourceRetention';

const sources = [
  { source_id: 'src_first', repo_key: 'local/src_first', display_name: 'service' },
  { source_id: 'src_second', repo_key: 'local/src_second', display_name: 'service' },
];
const meta = {
  title: 'Surfaces/SourceRetention', component: SourceRetention,
  args: { sources, preview: null, busy: false, error: null, message: null, onPreview: fn(), onDismiss: fn(), onForget: fn() },
} satisfies Meta<typeof SourceRetention>;
export default meta;
type Story = StoryObj<typeof meta>;

export const PreviewBeforeForgetting: Story = {
  // AC-0154: equal names stay distinguishable; preview cannot perform deletion.
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Preview retained source for local/src_second' }));
    await expect(args.onPreview).toHaveBeenCalledWith('src_second');
    await expect(args.onForget).not.toHaveBeenCalled();
    await expect(canvas.queryByRole('button', { name: 'Forget retained source' })).not.toBeInTheDocument();
  },
};

export const ExactSourceConfirmation: Story = {
  args: { preview: { ...sources[0], captures: 2, files: 4, bytes: 1500, receipts: 6, current_references: 2, historical_references: 4, fingerprint: 'preview-first' } },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const confirmation = within(canvas.getByRole('group', { name: 'Confirm source forgetting' }));
    await expect(confirmation.getByText('local/src_first')).toBeVisible();
    await expect(confirmation.getByText(/2 current and 4 historical/)).toBeVisible();
    await userEvent.click(confirmation.getByRole('button', { name: 'Forget retained source' }));
    await expect(args.onForget).toHaveBeenCalledOnce();
  },
};

export const ChangedPreviewRequiresRefresh: Story = {
  args: { error: 'Source changed. Refresh the preview before trying again.' },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('alert')).toHaveTextContent('Refresh the preview');
    await expect(canvas.queryByRole('button', { name: 'Forget retained source' })).not.toBeInTheDocument();
  },
};

// AC-0177/AC-0179: preview counts actual staged evidence without upgrading it.
export const StagedEvidenceReferencesSurviveForgetting: Story = {
  args: { preview: { ...sources[0], captures: 1, files: 1, bytes: 120,
    receipts: 2, current_references: 1, historical_references: 1,
    staged_references: 3, fingerprint: 'staged-preview' } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/3 staged evidence references/)).toBeInTheDocument();
    await expect(canvas.getByText(/proposal history remains/)).toBeInTheDocument();
  },
};

// AC-0187/0188: investigation references participate before raw source is read.
export const InvestigationReferencesSurviveForgetting: Story = {
  args: { preview: { ...sources[0], captures: 1, files: 1, bytes: 120,
    receipts: 2, current_references: 1, historical_references: 1,
    investigation_references: 2, fingerprint: 'investigation-preview' } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/2 investigation references/)).toBeInTheDocument();
    await expect(canvas.getByText(/Findings and their citation history remain/)).toBeInTheDocument();
  },
};
