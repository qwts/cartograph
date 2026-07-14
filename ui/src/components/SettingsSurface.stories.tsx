import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { SettingsSurface } from './SettingsSurface';
import type { CloudDisclosure, TierSettings } from '../store';

const DEFAULTS: TierSettings[] = [
  {
    tier: 'T1',
    enabled: false,
    provider: 'local',
    consented: false,
    consent_disclosure: null,
    consented_at: null,
  },
  {
    tier: 'T2',
    enabled: true,
    provider: 'local',
    consented: false,
    consent_disclosure: null,
    consented_at: null,
  },
  {
    tier: 'T3',
    enabled: false,
    provider: 'local',
    consented: false,
    consent_disclosure: null,
    consented_at: null,
  },
];

const T2_DISCLOSURE: CloudDisclosure = {
  provider: 'Anthropic',
  model: 'claude-haiku-4-5-20251001',
  endpoint: 'https://api.anthropic.com/v1/messages',
  input_usd_per_mtok: 1,
  output_usd_per_mtok: 5,
  notes: ['payload is the exact redacted span set shown by the egress firewall'],
};

const meta = {
  title: 'Surfaces/SettingsSurface',
  component: SettingsSurface,
  args: {
    tiers: DEFAULTS,
    egressLabel: 'Local-only · 0 bytes egress',
    disclosures: { T2: T2_DISCLOSURE },
    error: null,
    canEdit: true,
    onToggleTier: fn(),
    onProviderChange: fn(),
    onGrantConsent: fn(),
    onRevokeConsent: fn(),
  },
} satisfies Meta<typeof SettingsSurface>;

export default meta;
type Story = StoryObj<typeof meta>;

export const LocalOnlyDefaults: Story = {
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // The banner states the on-device default with the live egress line.
    await expect(canvas.getByText('Local core · on-device by default')).toBeInTheDocument();
    await expect(canvas.getByText('Local-only · 0 bytes egress')).toBeInTheDocument();

    // T0 is locked on — a statement, not a control.
    await expect(canvas.getByText('always on')).toBeInTheDocument();
    await expect(canvas.getByText(/never invokes an LLM/)).toBeInTheDocument();
    await expect(
      canvas.queryByRole('switch', { name: 'Deterministic enabled' }),
    ).not.toBeInTheDocument();

    // Toggles per configurable tier, reflecting persisted state.
    await expect(canvas.getByRole('switch', { name: 'Semantic enabled' })).toHaveAttribute(
      'aria-checked',
      'true',
    );
    await userEvent.click(canvas.getByRole('switch', { name: 'Dynamic observation enabled' }));
    await expect(args.onToggleTier).toHaveBeenCalledWith('T1', true);

    // No consent panel while everything is local.
    await expect(canvas.queryByText(/Cloud egress consent/)).not.toBeInTheDocument();

    // Choosing cloud is an explicit act on an enabled LLM tier.
    const t2Group = within(canvas.getByRole('radiogroup', { name: 'T2 provider' }));
    await userEvent.click(t2Group.getByRole('radio', { name: 'Cloud (opt-in)' }));
    await expect(args.onProviderChange).toHaveBeenCalledWith('T2', 'cloud');
  },
};

export const CloudSelectedShowsFullDisclosure: Story = {
  args: {
    tiers: DEFAULTS.map((tier) =>
      tier.tier === 'T2' ? { ...tier, provider: 'cloud' as const } : tier,
    ),
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    // Everything the user consents to is visible before the button.
    await expect(canvas.getByText('Cloud egress consent — T2')).toBeInTheDocument();
    await expect(canvas.getByText('claude-haiku-4-5-20251001')).toBeInTheDocument();
    await expect(canvas.getByText('https://api.anthropic.com/v1/messages')).toBeInTheDocument();
    await expect(canvas.getByText(/\$1\/M input · \$5\/M output/)).toBeInTheDocument();
    await expect(
      canvas.getByText(/exact redacted span set shown by the egress firewall/),
    ).toBeInTheDocument();
    await expect(canvas.getByText(/No cloud call is possible until granted/)).toBeInTheDocument();

    await userEvent.click(canvas.getByRole('button', { name: 'Grant revocable consent' }));
    await expect(args.onGrantConsent).toHaveBeenCalledWith('T2');
  },
};

export const MissingDisclosureFailsClosed: Story = {
  args: {
    tiers: DEFAULTS.map((tier) =>
      tier.tier === 'T2' ? { ...tier, provider: 'cloud' as const } : tier,
    ),
    disclosures: {},
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    // No disclosure ⇒ no consent affordance at all.
    await expect(
      canvas.getByText(/cloud consent cannot be recorded until the core provides it/),
    ).toBeInTheDocument();
    await expect(
      canvas.queryByRole('button', { name: 'Grant revocable consent' }),
    ).not.toBeInTheDocument();
  },
};

export const ConsentedTierIsRevocable: Story = {
  args: {
    tiers: DEFAULTS.map((tier) =>
      tier.tier === 'T2'
        ? {
            ...tier,
            provider: 'cloud' as const,
            consented: true,
            consent_disclosure: JSON.stringify(T2_DISCLOSURE),
            consented_at: '2026-07-14T20:00:00Z',
          }
        : tier,
    ),
    egressLabel: 'Cloud enabled (T2) · 0 bytes egress',
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Cloud enabled (T2) · 0 bytes egress')).toBeInTheDocument();
    await expect(canvas.getByText(/Standing cloud consent granted/)).toBeInTheDocument();
    // Granted consent still promises the per-payload gate.
    await expect(
      canvas.getByText(/Every call still shows its exact payload/),
    ).toBeInTheDocument();
    await userEvent.click(canvas.getByRole('button', { name: 'Revoke consent' }));
    await expect(args.onRevokeConsent).toHaveBeenCalledWith('T2');
  },
};

export const NoBackend: Story = {
  args: { tiers: [], canEdit: false },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByText(/connect a backend to manage it\. Everything runs local-only/),
    ).toBeInTheDocument();
    // T0's floor statement renders even with no core.
    await expect(canvas.getByText('always on')).toBeInTheDocument();
  },
};
