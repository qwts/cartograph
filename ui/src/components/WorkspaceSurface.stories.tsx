import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { WorkspaceSurface } from './WorkspaceSurface';
import type { FindingsSummary, SpecBundle, TierDistribution } from '../store';

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

    // The honest tally: findings are listed explicitly, never guessed.
    await expect(canvas.getByText('Partial recovery')).toBeInTheDocument();
    await expect(canvas.getByText('InferredStrong overall')).toBeInTheDocument();
    const outcome = within(canvas.getByTestId('outcome-card'));
    await expect(outcome.getByText('6 open findings')).toBeInTheDocument();
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
    await expect(canvas.getAllByText('Artifact generated')).toHaveLength(5);
    await expect(canvas.getAllByText('Recovery: partial')).toHaveLength(5);

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
    await expect(canvas.getByRole('button', { name: 'Triage 0 gaps' })).toBeInTheDocument();
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

export const NoRecoveryYet: Story = {
  args: { summary: null, findings: null, bundle: null },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('No recovery yet')).toBeInTheDocument();
    await expect(canvas.queryByTestId('prov-health')).not.toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Connect a target' }));
    await expect(args.onReingest).toHaveBeenCalled();
  },
};

export const MissingArtifactIsVisible: Story = {
  args: {
    bundle: {
      ...BUNDLE,
      artifacts: BUNDLE.artifacts.filter((artifact) => artifact.file_name !== 'topology.md'),
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    // A missing artifact is stated, not implied by absence.
    const topology = canvas
      .getByText('Topology / resource map')
      .closest('.artifact-card') as HTMLElement;
    await expect(within(topology).getByText('Not generated')).toBeInTheDocument();
  },
};
