import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { ResolutionStrategyModal } from './ResolutionStrategyModal';
import type { AgentProposal, EscalationState, GapStrategyReport } from '../store';

const REPORT: GapStrategyReport = {
  gap_id: 'gap:sync',
  summary: 'Remote sync target — endpoint host computed from config at runtime',
  stop_reason: 'endpoint host computed from config at runtime',
  attempted_tiers: ['T0'],
  required_evidence: ['E1 · local/image-trail:src/capture.ts', 'E2 · local/image-trail:src/background.ts'],
  candidates: 4,
  strategies: [
    {
      id: 'local-slm',
      tier: 'T3',
      provider: 'ollama:qwen3:8b',
      locality: 'local',
      egress_bytes: 0,
      est_usd: null,
      latency: 'seconds to a minute on-device',
      privacy: 'payload never leaves the device',
      export_impact:
        'Accepted proposals enter best-effort exports as InferredWeak with cited evidence; verified-only exports are unaffected (R-INT-5). T0/T1 facts are never modified (R-INT-1).',
      available: true,
      unavailable_reason: null,
    },
    {
      id: 'cloud-opus',
      tier: 'T3',
      provider: 'Anthropic · claude-opus-4-8',
      locality: 'cloud',
      egress_bytes: 2048,
      est_usd: 0.0151,
      latency: 'a few seconds via API',
      privacy: 'redacted payload leaves the device after a per-payload grant',
      export_impact:
        'Accepted proposals enter best-effort exports as InferredWeak with cited evidence; verified-only exports are unaffected (R-INT-5). T0/T1 facts are never modified (R-INT-1).',
      available: false,
      unavailable_reason:
        'T3 is not consented to cloud — enable the provider and grant consent in Settings (cloud fails closed)',
    },
  ],
};

const PROPOSAL: AgentProposal = {
  gap_id: 'gap:sync',
  source_id: 'sym:capture',
  target_id: 'ch:events',
  edge_label: 'PUBLISHES',
  annotation: 'capture() posts frames to the events channel per E1/E3 payload shape.',
  basis_hash: 'b'.repeat(64),
  provenance: {
    tier: 'Agentic',
    confidence_tier: 'InferredWeak',
    evidence: [
      {
        repo: 'local/image-trail',
        path: 'src/capture.ts',
        byte_start: 10,
        byte_end: 60,
        commit_sha: 'workdir',
      },
    ],
    extractor_id: 't3.agent',
    content_hash: 'c'.repeat(64),
  },
};

function state(overrides: Partial<EscalationState> = {}): EscalationState {
  return {
    gapId: 'gap:sync',
    report: REPORT,
    loading: false,
    error: null,
    running: false,
    preview: null,
    proposal: null,
    decided: null,
    ...overrides,
  };
}

const meta = {
  title: 'Overlays/ResolutionStrategyModal',
  component: ResolutionStrategyModal,
  args: {
    state: state(),
    onRun: fn(),
    onConsent: fn(),
    onDismissPreview: fn(),
    onDecide: fn(),
    onClose: fn(),
  },
} satisfies Meta<typeof ResolutionStrategyModal>;

export default meta;
type Story = StoryObj<typeof meta>;

export const StrategyCardsFromProvenance: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // The ladder, stop reason, and integrity rule are stated up front.
    await expect(
      canvas.getByText(/Why deterministic recovery stopped: endpoint host computed/),
    ).toBeInTheDocument();
    await expect(
      canvas.getByText(/T0 established this gap → escalate next\. T2\/T3 never overwrite T0\/T1/),
    ).toBeInTheDocument();
    await expect(canvas.getByText('Required evidence (2)')).toBeInTheDocument();
    await expect(
      canvas.getByText(/4 allowed candidate targets — the model can never invent one/),
    ).toBeInTheDocument();

    // Local card runs directly; egress/cost/privacy are explicit.
    const local = within(canvas.getByTestId('strategy-local-slm'));
    await expect(local.getByText('0 bytes')).toBeInTheDocument();
    await userEvent.click(local.getByRole('button', { name: 'Run locally' }));
    await expect(args.onRun).toHaveBeenCalledWith('local-slm');

    // Cloud card fails closed without consent: reason shown, no run button.
    const cloud = within(canvas.getByTestId('strategy-cloud-opus'));
    await expect(cloud.getByText(/cloud fails closed/)).toBeInTheDocument();
    await expect(cloud.queryByRole('button')).not.toBeInTheDocument();
  },
};

export const CloudGoesThroughExactPayloadReview: Story = {
  args: {
    state: state({
      report: {
        ...REPORT,
        strategies: REPORT.strategies.map((strategy) =>
          strategy.id === 'cloud-opus'
            ? { ...strategy, available: true, unavailable_reason: null }
            : strategy,
        ),
      },
    }),
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const cloud = within(canvas.getByTestId('strategy-cloud-opus'));
    await expect(cloud.getByText('2048 bytes')).toBeInTheDocument();
    await expect(cloud.getByText(/~\$0\.0151 per run/)).toBeInTheDocument();
    // Cloud never runs from this click — it opens the exact-payload review.
    await userEvent.click(cloud.getByRole('button', { name: 'Review exact payload…' }));
    await expect(args.onRun).toHaveBeenCalledWith('cloud-opus');
  },
};

export const ConsentDialogTakesOverForPreview: Story = {
  args: {
    state: state({
      preview: {
        provider_id: 'anthropic:claude-opus-4-8',
        locality: 'Cloud',
        tier: 'Agentic',
        action_id: 'escalate:gap:sync',
        payload: {
          system: 'You are Cartograph’s bounded T3 resolver…',
          prompt: '{"gap_id":"gap:sync"}',
          spans: [
            {
              id: 'E1',
              repo: 'local/image-trail',
              path: 'src/capture.ts',
              byte_start: 10,
              byte_end: 60,
              commit_sha: 'workdir',
              text: 'const handler = capture;',
            },
          ],
        },
        payload_hash: 'a'.repeat(64),
        redaction_count: 1,
      },
    }),
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // The one-action dialog renders the exact payload before any egress.
    await expect(canvas.getByText('Review exact model payload')).toBeInTheDocument();
    await expect(canvas.getByText('const handler = capture;')).toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Keep local' }));
    await expect(args.onDismissPreview).toHaveBeenCalled();
  },
};

export const ProposalNeverAutoJoins: Story = {
  args: { state: state({ proposal: PROPOSAL }) },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('proposal-card')).toBeInTheDocument();
    await expect(canvas.getByText('Inferred (weak)')).toBeInTheDocument();
    await expect(
      canvas.getByText(/never joins the spec until accepted, and never overwrites T0\/T1/),
    ).toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Accept as InferredWeak' }));
    await expect(args.onDecide).toHaveBeenCalledWith('accepted');
  },
};

export const DecisionRecordedState: Story = {
  args: { state: state({ proposal: PROPOSAL, decided: 'rejected' }) },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByTestId('decision-recorded')).toHaveTextContent(
      'Decision recorded: rejected',
    );
    await expect(canvas.queryByRole('button', { name: /accept/i })).not.toBeInTheDocument();
  },
};

export const RunFailureIsExplicit: Story = {
  args: {
    state: state({
      error:
        'no Anthropic API key configured (set ANTHROPIC_API_KEY) — cloud escalation stays closed',
    }),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText(/cloud escalation stays closed/)).toBeInTheDocument();
  },
};
