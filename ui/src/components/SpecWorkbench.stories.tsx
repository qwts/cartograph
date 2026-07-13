import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import type { SpecArtifact, SpecAssertion, SpecBundle } from '../store';
import { SpecWorkbench } from './SpecWorkbench';

const CONFIRMED: SpecAssertion = {
  id: 'node:cap:orders',
  subject_id: 'cap:orders',
  subject_kind: 'Capability',
  summary: 'Capability: Place orders',
  provenance: {
    tier: 'Deterministic',
    confidence_tier: 'Confirmed',
    evidence: [{
      repo: 'acme/shop',
      path: 'src/orders.ts',
      byte_start: 18,
      byte_end: 72,
      commit_sha: 'abc123',
    }],
    extractor_id: 't0.capability',
    content_hash: 'a'.repeat(64),
  },
};

const INFERRED: SpecAssertion = {
  id: 'node:adr:queue',
  subject_id: 'adr:queue',
  subject_kind: 'ADR',
  summary: 'ADR: asynchronous fulfillment',
  provenance: {
    tier: 'Semantic',
    confidence_tier: 'InferredStrong',
    evidence: [{
      repo: 'acme/shop',
      path: 'src/fulfillment.ts',
      byte_start: 90,
      byte_end: 148,
      commit_sha: 'abc123',
    }, {
      repo: 'acme/infra',
      path: 'orders.tf',
      byte_start: 10,
      byte_end: 88,
      commit_sha: 'def456',
    }],
    extractor_id: 't2.semantic',
    content_hash: 'b'.repeat(64),
  },
};

const GAP: SpecAssertion = {
  id: 'node:gap:channel',
  subject_id: 'gap:channel',
  subject_kind: 'Gap',
  summary: 'Gap: runtime-computed channel identity',
  provenance: {
    tier: 'Deterministic',
    confidence_tier: 'Gap',
    evidence: [{
      repo: 'acme/shop',
      path: 'src/publish.ts',
      byte_start: 44,
      byte_end: 80,
      commit_sha: 'abc123',
    }],
    extractor_id: 't0.events',
    content_hash: 'c'.repeat(64),
  },
};

function artifact(
  id: string,
  fileName: string,
  title: string,
  assertions: SpecAssertion[] = [],
): SpecArtifact {
  return {
    id,
    file_name: fileName,
    title,
    format: 'markdown',
    content: `# ${title}\n\n## Assertions and inline provenance\n`,
    assertions,
  };
}

const BUNDLE: SpecBundle = {
  mode: 'best-effort',
  artifacts: [
    artifact('user-stories', 'user_stories.md', 'User stories', [CONFIRMED, INFERRED]),
    artifact('us-tm', 'US-TM.md', 'US traceability matrix'),
    artifact('flow-dossiers', 'flow_dossiers.md', 'Flow dossiers', [CONFIRMED]),
    artifact('topology', 'topology.md', 'Resource topology', [CONFIRMED]),
    artifact('data-model', 'data_model.md', 'Data model', [CONFIRMED]),
    artifact('adrs', 'adrs.md', 'Architecture decisions', [INFERRED]),
    artifact('gap-register', 'gap_register.md', 'Gap register', [GAP]),
    artifact('drift-register', 'drift_register.md', 'Drift register'),
  ],
  assertion_count: 8,
  gap_count: 1,
  drift_count: 0,
};

const meta = {
  title: 'Spec/SpecWorkbench',
  component: SpecWorkbench,
  args: {
    bundle: BUNDLE,
    mode: 'best-effort',
    decisions: [],
    busy: false,
    error: null,
    canCurate: true,
    onModeChange: fn(),
    onCurate: fn(),
    onCopyArtifact: fn(),
    onExportBundle: fn(),
  },
} satisfies Meta<typeof SpecWorkbench>;

export default meta;
type Story = StoryObj<typeof meta>;

export const FullArtifactSetAndInlineProvenance: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const nav = canvas.getByRole('navigation', { name: 'Official spec artifacts' });
    await expect(within(nav).getAllByRole('button')).toHaveLength(8);
    await expect(canvas.getByText('8 artifacts')).toBeInTheDocument();
    await expect(canvas.getByText('Capability: Place orders')).toBeInTheDocument();
    await expect(canvas.getByText('ADR: asynchronous fulfillment')).toBeInTheDocument();
    await expect(canvas.getByText('t2.semantic')).toBeInTheDocument();
    await expect(canvas.getByText('b'.repeat(64))).toBeInTheDocument();
    await expect(canvas.getByText('acme/infra/orders.tf')).toBeInTheDocument();
    await userEvent.click(within(nav).getByRole('button', { name: /Gap register/ }));
    await expect(canvas.getByText('Gap: runtime-computed channel identity')).toBeInTheDocument();
    await expect(canvas.queryByText('None recorded — treated as unresolved.')).not.toBeInTheDocument();
  },
};

export const AcceptRejectAndAnnotate: Story = {
  play: async ({ args, canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole('button', { name: 'Accept' }));
    await expect(args.onCurate).toHaveBeenCalledWith(INFERRED, 'accepted', undefined);
    const annotation = canvas.getByLabelText('Annotation');
    await userEvent.type(annotation, 'Matched to the queue declaration');
    await userEvent.click(canvas.getByRole('button', { name: 'Annotate' }));
    await expect(args.onCurate).toHaveBeenLastCalledWith(
      INFERRED,
      'annotated',
      'Matched to the queue declaration',
    );
    await userEvent.click(canvas.getByRole('button', { name: 'Reject' }));
    await expect(args.onCurate).toHaveBeenLastCalledWith(
      INFERRED,
      'rejected',
      'Matched to the queue declaration',
    );
  },
};

export const VerifiedOnlyExport: Story = {
  args: {
    bundle: { ...BUNDLE, mode: 'verified-only' },
    mode: 'verified-only',
  },
  play: async ({ args, canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('button', { name: 'verified-only' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    await expect(canvas.getByText('1 gaps')).toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'best-effort' }));
    await expect(args.onModeChange).toHaveBeenCalledWith('best-effort');
    await userEvent.click(canvas.getByRole('button', { name: 'Export bundle' }));
    await expect(args.onExportBundle).toHaveBeenCalledWith(args.bundle);
  },
};

export const WithPersistedDecision: Story = {
  args: {
    decisions: [{
      assertion: {
        subject_id: INFERRED.subject_id,
        summary: INFERRED.summary,
        provenance: INFERRED.provenance,
      },
      decision: 'annotated',
      note: 'Matched to the queue declaration',
      updated_at: '2026-07-13T18:00:00Z',
    }],
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getAllByText('Annotated').length).toBeGreaterThan(0);
    await expect(canvas.getAllByText('Matched to the queue declaration').length).toBeGreaterThan(0);
  },
};

export const Empty: Story = {
  args: { bundle: null, canCurate: false },
  play: async ({ canvasElement }) => {
    await expect(within(canvasElement).getByText(/No compiled spec is available/)).toBeInTheDocument();
  },
};
