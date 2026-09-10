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

const FOUND_ADR: SpecAssertion = {
  ...CONFIRMED,
  id: 'node:adr:found:no-sync',
  subject_id: 'adr:found:no-sync',
  subject_kind: 'ADR',
  summary: 'ADR: No synchronous order calls',
  provenance: {
    ...CONFIRMED.provenance,
    evidence: [{
      repo: 'acme/shop',
      path: 'docs/adr/ADR-0007-no-sync.md',
      byte_start: 0,
      byte_end: 180,
      commit_sha: 'abc123',
    }],
    extractor_id: 't0.adr-markdown',
    content_hash: 'd'.repeat(64),
  },
};

const DRIFT: SpecAssertion = {
  ...CONFIRMED,
  id: 'node:drift:no-sync',
  subject_id: 'drift:no-sync',
  subject_kind: 'Drift',
  summary: 'Drift: No synchronous order calls forbids CALLS',
  provenance: {
    ...CONFIRMED.provenance,
    extractor_id: 't0.adr-drift',
    content_hash: 'e'.repeat(64),
  },
};

const UNAUTHENTICATED: SpecAssertion = {
  ...CONFIRMED,
  id: 'node:finding:security:admin',
  subject_id: 'finding:security:admin',
  subject_kind: 'Finding',
  summary: 'Finding: Unauthenticated endpoint: GET /admin',
  provenance: {
    ...CONFIRMED.provenance,
    extractor_id: 't0.security-projection',
    content_hash: 'f'.repeat(64),
  },
};

const OVER_BROAD_GRANT: SpecAssertion = {
  ...INFERRED,
  id: 'node:finding:security:grant',
  subject_id: 'finding:security:grant',
  subject_kind: 'Finding',
  summary: 'Finding: Over-broad IAM grant: admin policy → orders/*',
  provenance: {
    ...INFERRED.provenance,
    confidence_tier: 'InferredWeak',
    extractor_id: 't2.security-projection',
    content_hash: '1'.repeat(64),
  },
};

const SOURCE_RULE: SpecAssertion = {
  ...CONFIRMED,
  id: 'node:rule:guarded-return',
  subject_id: 'rule:guarded-return',
  subject_kind: 'BusinessRule',
  summary: 'Guarded local exit observation; behavioral interpretation not established',
  provenance: {
    ...CONFIRMED.provenance,
    extractor_id: 't0.ts',
    content_hash: '2'.repeat(64),
  },
};

const RULE_DEPENDENCY: SpecAssertion = {
  ...GAP,
  id: 'edge:rule:guarded-return DEPENDS_ON gap:consumer',
  subject_id: 'rule:guarded-return DEPENDS_ON gap:consumer',
  subject_kind: 'DEPENDS_ON',
  summary: 'Source-rule DEPENDS_ON relationship',
  provenance: {
    ...GAP.provenance,
    extractor_id: 't0.ts',
    content_hash: '3'.repeat(64),
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

const SOURCE_RULE_ARTIFACT: SpecArtifact = {
  ...artifact('rule-evidence', 'rule-evidence.md', 'Source rule evidence', [
    SOURCE_RULE,
    RULE_DEPENDENCY,
  ]),
  content: '# Source rule evidence\n\nGuarded local exit observations.\n\nComplete execution predicate: not established.\n\nConsumer effect: not established.\n\nLocal effect: Return(false).\n',
};

const BUNDLE: SpecBundle = {
  mode: 'best-effort',
  artifacts: [
    artifact('user-stories', 'user_stories.md', 'User stories', [CONFIRMED, INFERRED]),
    SOURCE_RULE_ARTIFACT,
    artifact('us-tm', 'US-TM.md', 'US traceability matrix'),
    artifact('flow-dossiers', 'flow_dossiers.md', 'Flow dossiers', [CONFIRMED]),
    artifact('topology', 'topology.md', 'Resource topology', [CONFIRMED]),
    artifact('data-model', 'data_model.md', 'Data model', [CONFIRMED]),
    artifact('adrs', 'adrs.md', 'Architecture decisions', [INFERRED]),
    artifact('gap-register', 'gap_register.md', 'Gap register', [GAP]),
    artifact('drift-register', 'drift_register.md', 'Drift register'),
    {
      ...artifact(
        'security-view',
        'security.md',
        'Security findings',
        [UNAUTHENTICATED, OVER_BROAD_GRANT],
      ),
      content: '# Security findings\n\n| Finding | Type | Subject | Resource scope | Actions | US / AC | Confidence |\n|---|---|---|---|---|---|---|\n| Unauthenticated endpoint: GET /admin | unauthenticated_endpoint | ep:admin | GET /admin | — | US-0015 / AC-0041 | Confirmed |\n| Over-broad IAM grant | over_broad_grant | res:admin GRANTS res:orders | arn:aws:s3:::orders/* | s3:Get* | US-0015 / AC-0042 | InferredWeak |\n',
    },
  ],
  assertion_count: 11,
  gap_count: 1,
  drift_count: 0,
  security_count: 2,
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
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const nav = canvas.getByRole('navigation', { name: 'Official spec artifacts' });
    await expect(within(nav).getAllByRole('button')).toHaveLength(10);
    await expect(canvas.getByText('10 artifacts')).toBeInTheDocument();
    await expect(canvas.getByText('Capability: Place orders')).toBeInTheDocument();
    await expect(canvas.getByText('ADR: asynchronous fulfillment')).toBeInTheDocument();
    await expect(canvas.getByText('t2.semantic')).toBeInTheDocument();
    await expect(canvas.getByText('b'.repeat(64))).toBeInTheDocument();
    await expect(canvas.getByText('acme/infra/orders.tf')).toBeInTheDocument();
    await userEvent.click(within(nav).getByRole('button', { name: /Gap register/ }));
    await expect(canvas.getByText('Gap: runtime-computed channel identity')).toBeInTheDocument();
    await expect(canvas.queryByText('None recorded — treated as unresolved.')).not.toBeInTheDocument();

    // AC-0125: the new artifact is navigable and copyable with its source-only
    // qualifier, actual assertion units, and locked T0 observation intact.
    const rulesLink = within(nav).getByRole('button', { name: /Source rule evidence/ });
    await expect(within(rulesLink).getByText('2 assertions')).toBeInTheDocument();
    await userEvent.click(rulesLink);
    await expect(rulesLink).toHaveAttribute('aria-current', 'page');
    const detail = within(canvas.getByRole('article', { name: 'Source rule evidence' }));
    await expect(detail.getByText(/Source observations only/)).toHaveTextContent(
      'Complete execution predicates and consumer effects are not established.',
    );
    await expect(detail.getByTestId('spec-artifact-source')).toHaveTextContent(
      'Consumer effect: not established.',
    );
    const observed = within(detail.getByText(SOURCE_RULE.summary).closest('li') as HTMLElement);
    await expect(observed.getByRole('button', { name: 'Accept' })).toBeDisabled();
    await expect(observed.getByText('Confirmed T0 — locked, read-only')).toBeInTheDocument();
    await userEvent.click(detail.getByRole('button', { name: 'Copy artifact' }));
    await expect(args.onCopyArtifact).toHaveBeenCalledWith(SOURCE_RULE_ARTIFACT);
  },
};

export const LocalConstInitializersKeepObservationAuthority: Story = {
  // AC-0170: the stored artifact is displayed and copied with its qualification;
  // initializer syntax does not become a curatable or complete business rule.
  args: {
    bundle: {
      ...BUNDLE,
      artifacts: [{
        ...SOURCE_RULE_ARTIFACT,
        content: '# Source rule evidence\n\nSame-callable branch condition: (allowed)\n\n### Local const initializers\n\nInitializer as written; value at use and business meaning are not established.\n\nBinding: binding:allowed\n\nDeclaration source: fixture:rules.ts bytes 20..65 @ workdir\n\nUse sources: fixture:rules.ts bytes 72..79 @ workdir\n\nStored initializer: item.enabled !== false\n\nStructured expression evidence: Binary StrictNotEqual; runtime value unresolved\n\nConsumer effect: not established.\n',
      }],
      assertion_count: SOURCE_RULE_ARTIFACT.assertions.length,
    },
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const source = canvas.getByTestId('spec-artifact-source');
    await expect(source).toHaveTextContent('Same-callable branch condition: (allowed)');
    await expect(source).toHaveTextContent('Initializer as written; value at use and business meaning are not established.');
    await expect(source).toHaveTextContent('Stored initializer: item.enabled !== false');
    await expect(source).toHaveTextContent('rules.ts bytes 20..65 @ workdir');
    await expect(source).toHaveTextContent('runtime value unresolved');
    await expect(canvas.getByText(/Source observations only/)).toBeInTheDocument();
    const observed = within(canvas.getByText(SOURCE_RULE.summary).closest('li') as HTMLElement);
    await expect(observed.getByRole('button', { name: 'Accept' })).toBeDisabled();
    await userEvent.click(canvas.getByRole('button', { name: 'Copy artifact' }));
    await expect(args.onCopyArtifact).toHaveBeenCalledWith(args.bundle?.artifacts[0]);
  },
};

export const LegacyRuleDefinitionsWereNotCollected: Story = {
  // AC-0170: v1 history remains visible without implying a fresh empty analysis.
  args: {
    bundle: {
      ...BUNDLE,
      artifacts: [{
        ...SOURCE_RULE_ARTIFACT,
        content: '# Source rule evidence\n\n### Local const initializers\n\nInitializer as written; value at use and business meaning are not established.\n\nLocal-definition evidence was not collected in this version 1 observation.\n',
      }],
      assertion_count: SOURCE_RULE_ARTIFACT.assertions.length,
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('spec-artifact-source')).toHaveTextContent('Local-definition evidence was not collected in this version 1 observation.');
    await expect(canvas.getByText(/Source observations only/)).toHaveTextContent('Complete execution predicates and consumer effects are not established.');
    await expect(canvas.getByText(SOURCE_RULE.summary)).toBeInTheDocument();
  },
};

export const SecurityFindings: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const nav = canvas.getByRole('navigation', { name: 'Official spec artifacts' });
    await userEvent.click(within(nav).getByRole('button', { name: /Security findings/ }));
    await expect(canvas.getByText('2 security findings')).toBeInTheDocument();
    await expect(canvas.getByText('Finding: Unauthenticated endpoint: GET /admin')).toBeInTheDocument();
    await expect(
      canvas.getByText('Finding: Over-broad IAM grant: admin policy → orders/*'),
    ).toBeInTheDocument();
    const source = canvas.getByTestId('spec-artifact-source');
    await expect(source).toHaveTextContent('US-0015 / AC-0041');
    await expect(source).toHaveTextContent('US-0015 / AC-0042');
    await expect(source).toHaveTextContent('arn:aws:s3:::orders/*');
  },
};

export const AcceptRejectAndAnnotate: Story = {
  play: async ({ args, canvasElement }) => {
    // Scope to the inferred block: the confirmed block above it renders the
    // same three controls locked (#108), so unscoped queries are ambiguous.
    const block = within(canvasElement)
      .getByText('ADR: asynchronous fulfillment')
      .closest('li') as HTMLElement;
    const canvas = within(block);
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

export const ConfirmedBlocksAreLockedNotHidden: Story = {
  // #108: T0 lock semantics — Confirmed blocks show the curation controls
  // truly disabled (real in the a11y tree) with an inline explanation;
  // inferred blocks stay curatable; Gap blocks offer no curation at all.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('note')).toHaveTextContent(
      /Confirmed \(T0\/T1\) assertions are read-only/,
    );

    const locked = within(
      canvas.getByText('Capability: Place orders').closest('li') as HTMLElement,
    );
    for (const name of ['Accept', 'Reject', 'Annotate']) {
      const control = locked.getByRole('button', { name });
      await expect(control).toBeDisabled();
      await expect(control).toHaveAttribute('aria-disabled', 'true');
    }
    await expect(locked.getByText('Confirmed T0 — locked, read-only')).toBeInTheDocument();

    const curatable = within(
      canvas.getByText('ADR: asynchronous fulfillment').closest('li') as HTMLElement,
    );
    await expect(curatable.getByRole('button', { name: 'Accept' })).toBeEnabled();
    await expect(curatable.queryByText(/locked, read-only/)).not.toBeInTheDocument();

    const nav = canvas.getByRole('navigation', { name: 'Official spec artifacts' });
    // Per-doc chips name what the assertions actually are (#145 review):
    // recovered stories are one-per-US, but the dossier's assertions are
    // hops — never relabeled as flows. Registers read as alerts.
    await expect(within(nav).getByText('2 US')).toBeInTheDocument();
    await expect(within(nav).getByText('1 hops')).toBeInTheDocument();
    const gapChip = within(nav).getByRole('button', { name: /Gap register/ });
    await expect(gapChip.querySelector('.spec-doc-chip')).toHaveClass('alert');

    await userEvent.click(within(nav).getByRole('button', { name: /Gap register/ }));
    const gapBlock = within(
      canvas.getByText('Gap: runtime-computed channel identity').closest('li') as HTMLElement,
    );
    await expect(gapBlock.queryByRole('button')).not.toBeInTheDocument();
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

export const FoundRecoveredAndDrift: Story = {
  args: {
    bundle: {
      ...BUNDLE,
      drift_count: 1,
      artifacts: BUNDLE.artifacts.map((item) => {
        if (item.id === 'adrs') {
          return {
            ...artifact('adrs', 'adrs.md', 'Architecture decisions', [FOUND_ADR, INFERRED]),
            content: '# Found and recovered ADRs\n\n## No synchronous order calls\n\n**Origin:** found\n\n## Recovered: asynchronous fulfillment\n\n**Origin:** recovered\n**Status:** Recovered / Inferred\n',
          };
        }
        if (item.id === 'drift-register') {
          return {
            ...artifact('drift-register', 'drift_register.md', 'Drift register', [DRIFT]),
            content: '# Drift register\n\n| Finding | ADR | Offending edge | Flow triggers | Confidence |\n|---|---|---|---|---|\n| No synchronous order calls | adr:found:no-sync | sym:handler CALLS sym:remote | ep:orders | Confirmed |\n',
          };
        }
        return item;
      }),
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const nav = canvas.getByRole('navigation', { name: 'Official spec artifacts' });
    await userEvent.click(within(nav).getByRole('button', { name: /Architecture decisions/ }));
    await expect(canvas.getByText('ADR: No synchronous order calls')).toBeInTheDocument();
    await expect(canvas.getByText('ADR: asynchronous fulfillment')).toBeInTheDocument();
    await expect(canvas.getByText('t0.adr-markdown')).toBeInTheDocument();
    await userEvent.click(within(nav).getByRole('button', { name: /Drift register/ }));
    await expect(canvas.getByTestId('spec-artifact-source')).toHaveTextContent(
      'sym:handler CALLS sym:remote',
    );
    await expect(canvas.getByTestId('spec-artifact-source')).toHaveTextContent('ep:orders');
  },
};

export const Empty: Story = {
  args: { bundle: null, canCurate: false },
  play: async ({ canvasElement }) => {
    await expect(within(canvasElement).getByText(/No compiled spec is available/)).toBeInTheDocument();
  },
};
