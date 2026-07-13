import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { EgressConsentDialog, type EgressPreview } from './EgressConsentDialog';

const preview: EgressPreview = {
  provider_id: 'cloud:gpt',
  locality: 'Cloud',
  tier: 'Agentic',
  action_id: 'agent:gap:orders',
  payload: {
    system: 'Propose one InferredWeak link and return JSON only.',
    prompt: '{"gap_id":"gap:orders","edge_label":"PUBLISHES"}',
    spans: [
      {
        id: 'source',
        repo: 'acme/shop',
        path: 'src/orders.ts',
        byte_start: 120,
        byte_end: 164,
        commit_sha: 'abc123',
        text: 'publish(queueName, { token: "[REDACTED]" })',
      },
      {
        id: 'target',
        repo: 'acme/infra',
        path: 'orders.tf',
        byte_start: 20,
        byte_end: 88,
        commit_sha: 'def456',
        text: 'resource "aws_sqs_queue" "orders" {}',
      },
    ],
  },
  payload_hash: 'a'.repeat(64),
  redaction_count: 1,
};

const meta = {
  title: 'Privacy/EgressConsentDialog',
  component: EgressConsentDialog,
  args: {
    preview,
    onCancel: fn(),
    onConsent: fn(),
  },
} satisfies Meta<typeof EgressConsentDialog>;

export default meta;
type Story = StoryObj<typeof meta>;

export const ExactSpanPayload: Story = {
  play: async ({ args, canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('dialog')).toHaveAccessibleName('Review exact model payload');
    await expect(canvas.getByText('cloud:gpt')).toBeInTheDocument();
    await expect(canvas.getByText('agent:gap:orders')).toBeInTheDocument();
    await expect(canvas.getByText(/acme\/shop\/src\/orders\.ts:120-164 @abc123/)).toBeInTheDocument();
    await expect(canvas.getByText(/acme\/infra\/orders\.tf:20-88 @def456/)).toBeInTheDocument();
    await expect(canvas.getByText(/token: "\[REDACTED\]"/)).toBeInTheDocument();
    await expect(canvas.queryByText(/super-secret/i)).not.toBeInTheDocument();
    await expect(canvas.getByText('a'.repeat(64))).toBeInTheDocument();

    await userEvent.click(canvas.getByRole('button', { name: 'Allow this action once' }));
    await expect(args.onConsent).toHaveBeenCalledOnce();
    await expect(args.onConsent).toHaveBeenCalledWith(preview);
  },
};

export const SemanticSpanPayload: Story = {
  args: {
    preview: {
      ...preview,
      tier: 'Semantic',
      action_id: 'semantic:gap:orders',
      redaction_count: 0,
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('T2 · InferredStrong')).toBeInTheDocument();
    await expect(canvas.queryByText('T3 · InferredWeak')).not.toBeInTheDocument();
    await expect(canvas.getByText('Semantic')).toBeInTheDocument();
    await expect(canvas.getByText('semantic:gap:orders')).toBeInTheDocument();
  },
};
