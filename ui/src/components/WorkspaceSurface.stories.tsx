import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { WorkspaceSurface } from './WorkspaceSurface';
import type { FindingsSummary, SpecBundle, SystemRepo, TierDistribution } from '../store';

/** The handoff's image-trail outcome (screenshot 01). */
const FINDINGS: FindingsSummary = {
  gaps: 3,
  unsupported: 2,
  no_evidence: 1,
  drift: 1,
  open_findings: 6,
  graph_facts: 134,
};

const DISTRIBUTION: TierDistribution = {
  confirmed: 98,
  inferredStrong: 22,
  inferredWeak: 11,
  gap: 3,
  unattributed: 0,
  total: 134,
};

const BUNDLE = {
  mode: 'best-effort',
  assertion_count: 12,
  gap_count: 3,
  drift_count: 1,
  security_count: 0,
  artifacts: [
    'user_stories.md',
    'rule-evidence.md',
    'flow_dossiers.md',
    'US-TM.md',
    'topology.md',
    'gap_register.md',
    'adrs.md',
  ].map((fileName, index) => ({
    id: `artifact-${index}`,
    file_name: fileName,
    title: fileName,
    format: 'markdown' as const,
    content: `# ${fileName}`,
    assertions: [],
  })),
} satisfies SpecBundle;

const meta = {
  title: 'Surfaces/WorkspaceSurface',
  component: WorkspaceSurface,
  args: {
    summary: {
      job_id: 3,
      files: 14,
      nodes: 80,
      edges: 54,
      layers: {
        ts: { files: 14, nodes: 80, edges: 54 },
        python: { files: 0, nodes: 0, edges: 0 },
        go: { files: 0, nodes: 0, edges: 0 },
        tf: { files: 0, nodes: 0, edges: 0 },
        java: { files: 0, nodes: 0, edges: 0 },            kotlin: { files: 0, nodes: 0, edges: 0 },
        webext: { files: 0, nodes: 0, edges: 0 },
        tools: { files: 0, nodes: 0, edges: 0 },
      },
      repo: 'collidingscopes/image-trail',
      commit_sha: 'a1b9f30fffffffffffffffffffffffffffffffff',
    },
    findings: FINDINGS,
    distribution: DISTRIBUTION,
    bundle: BUNDLE,
    onReingest: fn(),
    onTriageGaps: fn(),
    onProvenance: fn(),
    onOpenArtifact: fn(),
  },
} satisfies Meta<typeof WorkspaceSurface>;

export default meta;
type Story = StoryObj<typeof meta>;

export const PartialRecovery: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // Title row: system, mono commit chip, re-ingest.
    await expect(canvas.getByText('collidingscopes/image-trail')).toBeInTheDocument();
    await expect(canvas.getByText('@ a1b9f30')).toBeInTheDocument();

    // The honest tally: findings are listed explicitly, never guessed. The
    // badge reports the weakest tier present (11 InferredWeak facts here).
    await expect(canvas.getByText('Partial recovery')).toBeInTheDocument();
    await expect(canvas.getByText('InferredWeak overall')).toBeInTheDocument();
    const outcome = within(canvas.getByTestId('outcome-card'));
    await expect(outcome.getByText('6 open findings')).toBeInTheDocument();
    // #164: outcome jargon and artifact authority explain themselves.
    await expect(outcome.getByRole('button', { name: 'What is system gap?' })).toBeInTheDocument();
    await expect(
      canvas.getByRole('button', { name: 'What is recovery authority?' }),
    ).toBeInTheDocument();
    await expect(
      outcome.getByText(/3 gaps and 2 unsupported patterns \(plus 1 no-evidence\)/),
    ).toBeInTheDocument();

    // Provenance health reconciles with the same register summary the
    // outcome quotes — one source of truth.
    await expect(canvas.getByTestId('count-confirmed')).toHaveTextContent('98');
    await expect(canvas.getByText('73% of 134 facts')).toBeInTheDocument();
    await expect(canvas.getByTestId('count-gap')).toHaveTextContent(String(FINDINGS.gaps));
    await expect(canvas.getByTestId('count-unsupported')).toHaveTextContent(
      String(FINDINGS.unsupported),
    );

    // CTAs route to the register surfaces.
    await userEvent.click(canvas.getByRole('button', { name: 'Triage 3 gaps' }));
    await expect(args.onTriageGaps).toHaveBeenCalled();
    await userEvent.click(canvas.getByRole('button', { name: 'Provenance & eval' }));
    await expect(args.onProvenance).toHaveBeenCalled();
    await userEvent.click(canvas.getByRole('button', { name: /re-ingest/i }));
    await expect(args.onReingest).toHaveBeenCalled();
  },
};

export const ArtifactBadgeSemantics: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // Two independent axes on generated artifacts: generation + authority.
    await expect(canvas.getAllByText('Artifact generated')).toHaveLength(6);
    await expect(canvas.getAllByText('Recovery: partial')).toHaveLength(5);

    // AC-0125: generation of source observations does not establish behavior.
    const rules = canvas.getByRole('button', { name: /Source rule evidence/ });
    await expect(within(rules).getByText('Artifact generated')).toBeInTheDocument();
    await expect(within(rules).getByText('Interpretation: incomplete')).toBeInTheDocument();
    await expect(within(rules).queryByText(/Recovery:/)).not.toBeInTheDocument();
    await userEvent.click(rules);
    await expect(args.onOpenArtifact).toHaveBeenCalled();

    // The gap register shows exactly ONE completion-style badge, never two.
    const register = canvas
      .getByText('Gap register')
      .closest('.artifact-card') as HTMLElement;
    await expect(within(register).getByText('6 open findings')).toBeInTheDocument();
    await expect(within(register).queryByText('Artifact generated')).not.toBeInTheDocument();
    await expect(within(register).queryByText(/Recovery:/)).not.toBeInTheDocument();

    await userEvent.click(canvas.getByText('Flow dossiers'));
    await expect(args.onOpenArtifact).toHaveBeenCalled();
  },
};

export const FullyConfirmedRecovery: Story = {
  args: {
    findings: { ...FINDINGS, gaps: 0, unsupported: 0, no_evidence: 0, open_findings: 0 },
    distribution: {
      confirmed: 134,
      inferredStrong: 0,
      inferredWeak: 0,
      gap: 0,
      unattributed: 0,
      total: 134,
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Full recovery')).toBeInTheDocument();
    await expect(canvas.getByText('Confirmed overall')).toBeInTheDocument();
    await expect(canvas.getAllByText('Recovery: authoritative')).toHaveLength(5);
    // AC-0125: even a confirmed graph cannot upgrade local source observations
    // to a complete behavioral interpretation.
    const rules = within(canvas.getByRole('button', { name: /Source rule evidence/ }));
    await expect(rules.getByText('Interpretation: incomplete')).toBeInTheDocument();
    await expect(rules.queryByText('Recovery: authoritative')).not.toBeInTheDocument();
    await expect(canvas.getByRole('button', { name: 'Triage 0 gaps' })).toBeInTheDocument();
  },
};

export const ConfirmedWithGapsIsNotInferred: Story = {
  // AC-0057 (#245): a fully deterministic recovery with open gaps is
  // partial, but the tier badge states what the recovered facts carry —
  // Confirmed, never an Inferred tier no fact holds (R-INT-2/R-INT-4).
  args: {
    findings: {
      gaps: 203,
      unsupported: 0,
      no_evidence: 0,
      drift: 0,
      open_findings: 203,
      graph_facts: 1265,
    },
    distribution: {
      confirmed: 1062,
      inferredStrong: 0,
      inferredWeak: 0,
      gap: 203,
      unattributed: 0,
      total: 1265,
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Partial recovery')).toBeInTheDocument();
    await expect(canvas.getByText('Confirmed overall')).toBeInTheDocument();
    await expect(canvas.queryByText(/Inferred\w+ overall/)).not.toBeInTheDocument();
  },
};

export const UnsupportedOnlyIsNotAuthoritative: Story = {
  // Review fix on #136: unsupported/no-evidence findings without gaps must
  // still keep artifacts from claiming authoritative recovery.
  args: {
    findings: {
      gaps: 0,
      unsupported: 2,
      no_evidence: 0,
      drift: 0,
      open_findings: 2,
      graph_facts: 134,
    },
    distribution: {
      confirmed: 134,
      inferredStrong: 0,
      inferredWeak: 0,
      gap: 0,
      unattributed: 0,
      total: 134,
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getAllByText('Recovery: partial')).toHaveLength(5);
    await expect(canvas.queryByText('Recovery: authoritative')).not.toBeInTheDocument();
  },
};

export const ManifestRecoveryShowsRepoIdentities: Story = {
  // Review fix on #136: a manifest recovery lists exact per-repo identities,
  // never a false mutable-workdir chip.
  args: {
    summary: {
      job_id: 5,
      files: 5,
      nodes: 40,
      edges: 60,
      layers: {
        ts: { files: 3, nodes: 25, edges: 38 },
        python: { files: 0, nodes: 0, edges: 0 },
        go: { files: 0, nodes: 0, edges: 0 },
        tf: { files: 2, nodes: 15, edges: 22 },
        java: { files: 0, nodes: 0, edges: 0 },            kotlin: { files: 0, nodes: 0, edges: 0 },
        webext: { files: 0, nodes: 0, edges: 0 },
        tools: { files: 0, nodes: 0, edges: 0 },
      },
      repos: ['acme/shop@a1b2c3d4e5f6', 'local/infra@workdir'],
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('2 repos as one system')).toBeInTheDocument();
    await expect(
      canvas.getByText('acme/shop@a1b2c3d4e5f6 · local/infra@workdir'),
    ).toBeInTheDocument();
    await expect(canvas.queryByText('@ workdir')).not.toBeInTheDocument();
  },
};

export const StackedSystemContentsAreStated: Story = {
  // AC-0085 (#162): two stacked ingests are never silent — the Workspace
  // names every repo in the system and states the merge behavior.
  args: {
    systemContents: [
      { repo: 'acme/shop', commit: 'a1b2c3d4e5f6' },
      { repo: 'local/infra', commit: 'workdir' },
    ],
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const contents = canvas.getByTestId('system-contents');
    await expect(contents).toHaveTextContent(
      'System contents: acme/shop @ a1b2c3d · local/infra @ workdir',
    );
    await expect(contents).toHaveTextContent(
      'new ingests merge into this system; Clear system starts over',
    );
  },
};

export const NoRecoveryYet: Story = {
  args: { summary: null, findings: null, bundle: null },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('No recovery yet')).toBeInTheDocument();
    await expect(canvas.queryByTestId('prov-health')).not.toBeInTheDocument();

    // #161: the empty landing flows with the window — a fluid, centered
    // column (auto inline margins under a wide cap), never a fixed block
    // pinned to the left half of a stretched window.
    const landing = canvasElement.querySelector('.workspace-landing') as HTMLElement;
    const style = getComputedStyle(landing);
    await expect(style.maxWidth).toContain('1280px');
    // margin-inline: auto — equal margins on both sides at any width.
    await expect(style.marginLeft).toBe(style.marginRight);

    await userEvent.click(canvas.getByRole('button', { name: 'Connect a target' }));
    await expect(args.onReingest).toHaveBeenCalled();
  },
};

export const RegisteredRepositoryDisplayNames: Story = {
  // AC-0144: operational labels use exact repo membership, never the first
  // registration. A host without a display label still shows the repository key.
  args: {
    summary: { ...meta.args.summary, repo: 'local/src_22222222222222222222222222222222' },
    systemContents: [
      { repo: 'local/src_11111111111111111111111111111111', display_name: 'Other project', commit: 'workdir' },
      { repo: 'local/src_22222222222222222222222222222222', display_name: 'Billing service', commit: 'workdir' },
      { repo: 'acme/infra', commit: 'a1b2c3d4e5f6' },
    ],
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('heading', { name: 'Billing service' })).toBeInTheDocument();
    await expect(canvas.getByTestId('system-contents')).toHaveTextContent(
      'Other project @ workdir · Billing service @ workdir · acme/infra @ a1b2c3d',
    );
    await expect(canvas.queryByText(/local\/src_/)).not.toBeInTheDocument();
  },
};

const SAME_NAMED_SOURCES: SystemRepo[] = [
  { repo: 'local/src_11111111111111111111111111111111', display_name: 'mirror', commit: 'workdir' },
  { repo: 'local/src_22222222222222222222222222222222', display_name: ' mirror ', commit: 'workdir' },
  { repo: 'acme/shop', display_name: 'shop', commit: 'a1b2c3d4e5f6' },
  { repo: 'other/shop', display_name: 'shop', commit: 'b2c3d4e5f6a1' },
  { repo: 'acme/billing', display_name: 'Billing service', commit: 'workdir' },
  { repo: 'legacy/mirror', commit: 'workdir' },
  { repo: 'legacy/blank', display_name: '  ', commit: 'workdir' },
];

export const SameNamedRepositoriesRemainDistinct: Story = {
  // AC-0144: equal local basenames and GitHub repository names retain their
  // human labels and exact keys. Unique names and missing labels stay concise.
  args: {
    summary: { ...meta.args.summary, repo: 'local/src_22222222222222222222222222222222' },
    systemContents: SAME_NAMED_SOURCES,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('heading', {
      name: 'mirror (local/src_22222222222222222222222222222222)',
    })).toBeInTheDocument();
    await expect(canvas.getByTestId('system-contents')).toHaveTextContent(
      'System contents: mirror (local/src_11111111111111111111111111111111) @ workdir · '
      + 'mirror (local/src_22222222222222222222222222222222) @ workdir · '
      + 'shop (acme/shop) @ a1b2c3d · shop (other/shop) @ b2c3d4e · '
      + 'Billing service @ workdir · legacy/mirror @ workdir · legacy/blank @ workdir',
    );
  },
};

export const ManifestLabelsUseExactRepositoryMembership: Story = {
  // AC-0144: revision labels share the same disambiguation. A same-basename
  // repository absent from current membership never borrows another's label.
  args: {
    summary: {
      ...meta.args.summary,
      repo: undefined,
      commit_sha: undefined,
      repos: ['acme/shop@a1b2c3d4e5f6', 'other/shop@b2c3d4e5f6a1', 'unknown/shop@workdir'],
    },
    systemContents: SAME_NAMED_SOURCES,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole('heading', { name: '3 repos as one system' })).toBeInTheDocument();
    await expect(canvas.getByText(
      'shop (acme/shop)@a1b2c3d4e5f6 · shop (other/shop)@b2c3d4e5f6a1 · unknown/shop@workdir',
    )).toBeInTheDocument();
  },
};

export const MissingArtifactIsVisible: Story = {
  args: {
    bundle: {
      ...BUNDLE,
      artifacts: BUNDLE.artifacts.filter(
        (artifact) => !['topology.md', 'rule-evidence.md'].includes(artifact.file_name),
      ),
    },
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // A missing artifact is stated, not implied by absence.
    const topology = canvas
      .getByText('Topology / resource map')
      .closest('.artifact-card') as HTMLElement;
    await expect(within(topology).getByText('Not generated')).toBeInTheDocument();
    // AC-0125: older/missing bundles retain an explicit missing card and the
    // established navigation into the Spec Workbench.
    const rules = canvas.getByRole('button', { name: /Source rule evidence/ });
    await expect(within(rules).getByText('Not generated')).toBeInTheDocument();
    await expect(within(rules).queryByText('Artifact generated')).not.toBeInTheDocument();
    await userEvent.click(rules);
    await expect(args.onOpenArtifact).toHaveBeenCalled();
  },
};
