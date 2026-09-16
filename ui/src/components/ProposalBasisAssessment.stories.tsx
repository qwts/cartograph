import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { ProposalBasisAssessment } from './ProposalBasisAssessment';
import type { BasisAssessmentState } from '../store';

const ASSESSMENT: BasisAssessmentState = {
  proposalId: 'proposal:fixture', requestGeneration: 1, loading: false, error: null,
  result: {
    schema_version: 1, proposal_id: 'proposal:fixture', current_graph_snapshot_id: 'snapshot:observed',
    graph_comparison: 'unchanged', association_comparison: 'changed',
    evidence: [{ evidence_id: 'E1', status: 'unavailable' }, { evidence_id: 'E2', status: 'working_tree_unverified' }],
    unverified_evidence: 1, captured_validation_bytes: 0, max_captured_validation_bytes: 134217728,
  },
};

const meta = {
  title: 'Evidence/ProposalBasisAssessment', component: ProposalBasisAssessment,
  args: { onAssess: fn() },
} satisfies Meta<typeof ProposalBasisAssessment>;
export default meta;
type Story = StoryObj<typeof meta>;

export const ChangedAssociationsAndUnavailableBytes: Story = {
  // AC-0179: equal graph content does not hide changed receipts or missing retained bytes.
  args: { state: ASSESSMENT },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Unchanged at this check')).toBeInTheDocument();
    await expect(canvas.getByText('Changed since preparation')).toBeInTheDocument();
    await expect(canvas.getByText(/Retained bytes unavailable/)).toBeInTheDocument();
    await expect(canvas.getByText(/Working-tree input — unverified/)).toBeInTheDocument();
    await expect(canvas.getByText(/does not change the saved proposal/)).toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Check again' }));
    await expect(args.onAssess).toHaveBeenCalledOnce();
  },
};

export const OperationalFailureIsNotMissingEvidence: Story = {
  // AC-0179: storage failure and byte-budget exhaustion remain separate outcomes.
  args: { state: { ...ASSESSMENT, result: { ...ASSESSMENT.result!,
    graph_comparison: 'operational_failure', association_comparison: 'operational_failure',
    evidence: [{ evidence_id: 'E1', status: 'operational_failure' }, { evidence_id: 'E2', status: 'validation_byte_limit' }],
  } } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getAllByText('Comparison could not be completed')).toHaveLength(2);
    await expect(canvas.getByText(/Retained evidence could not be checked/)).toBeInTheDocument();
    await expect(canvas.getByText(/Retained-byte check reached its limit/)).toBeInTheDocument();
    await expect(canvas.queryByText(/Retained bytes unavailable/)).not.toBeInTheDocument();
  },
};

export const CheckIsExplicitAndFailureCanBeRetried: Story = {
  // AC-0179: failed observation does not become a successful or empty assessment.
  args: { state: { ...ASSESSMENT, result: null, error: 'Current basis could not be assessed. No proposal or review was changed.' } },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('alert')).toHaveTextContent('could not be assessed');
    await expect(canvas.queryByText('Unchanged at this check')).not.toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Check again' }));
    await expect(args.onAssess).toHaveBeenCalledOnce();
  },
};
