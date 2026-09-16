import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, userEvent, within } from 'storybook/test';
import { TaskEvidence } from './TaskEvidence';
import { mixedTaskBasis } from '../taskBasisFixtures';

const meta = {
  title: 'Evidence/TaskEvidence', component: TaskEvidence,
  args: { basis: mixedTaskBasis },
} satisfies Meta<typeof TaskEvidence>;
export default meta;
type Story = StoryObj<typeof meta>;

export const MixedOriginsAndSelectionLimits: Story = {
  // AC-0179: supplied origins, omissions and unread tails have distinct labels.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/1 retained parser span\(s\) · 1 working-tree span\(s\), unverified/)).toBeInTheDocument();
    await expect(canvas.getByText(/Evidence request limit reached/)).toHaveTextContent('Further candidates beyond the selection window were not inspected');
    await userEvent.click(canvas.getByText('Evidence origins (2)'));
    await expect(canvas.getByText('src_registered')).toBeVisible();
    await expect(canvas.getByText('receipt:original')).toBeVisible();
    await expect(canvas.getByText(/Working-tree input — unverified/)).toBeVisible();
    await userEvent.click(canvas.getByText('Selection details'));
    await expect(canvas.getByText('Evidence requests: 64 / 64.')).toBeVisible();
    await expect(canvas.getByText('Request 3: Legacy working-tree read unavailable.')).toBeVisible();
    await userEvent.click(canvas.getByText('Saved basis metadata'));
    await expect(canvas.getByText(/"input_closure": "input_closure_not_established"/)).toBeVisible();
  },
};

export const UnreadWindowIsNotAnOmission: Story = {
  // AC-0179: a successful early stop does not label unvisited evidence unreadable.
  args: { basis: { ...mixedTaskBasis, selection: { ...mixedTaskBasis.selection,
    stop_reason: 'candidate_limit', acquisition_attempts: 10, supplied_evidence: 10, supplied_candidates: 8,
    omissions: [], metadata_preselected_not_read: 54, unread_tail: true,
  } } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/Candidate limit reached/)).toHaveTextContent('0 omission(s); 54 preselected item(s) not read');
    await expect(canvas.queryByText(/Legacy working-tree read unavailable/)).not.toBeInTheDocument();
  },
};

export const LegacyOriginsAreNotRetrospectivelyCaptured: Story = {
  // AC-0179: absent v2 metadata preserves explicit legacy uncertainty.
  args: { basis: undefined },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/legacy working-tree spans have no parser-input receipt/)).toBeInTheDocument();
    await expect(canvas.queryByText(/retained parser span/)).not.toBeInTheDocument();
  },
};
