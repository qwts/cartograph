import type { Meta, StoryObj } from '@storybook/react-vite';
import { expect, fn, userEvent, within } from 'storybook/test';
import { SettingsSurface } from './SettingsSurface';
import type { AdapterInventory, CloudDisclosure, TierSettings } from '../store';

/** Mirrors `ingest::preflight::INSTALLED_ADAPTERS`/`PLANNED_ADAPTERS`. */
const INVENTORY: AdapterInventory = {
  installed: [
    {
      id: 't0.adapter-ts',
      language: 'TypeScript',
      extensions: ['ts', 'tsx'],
      covers: 'imports, call graph, endpoints, chrome messaging, IndexedDB',
    },
    {
      id: 't0.webextension',
      language: 'WebExtension',
      extensions: [],
      covers: 'manifest.json contexts, commands, permissions as grants',
    },
    {
      id: 't0.adapter-java',
      language: 'Java',
      extensions: ['java'],
      covers: 'types, methods, call graph, Spring Web endpoint annotations',
    },
    {
      id: 't0.iac-terraform',
      language: 'Terraform',
      extensions: ['tf'],
      covers: 'resource DAG, AWS capability edges',
    },
  ],
  planned: [
    { language: 'JavaScript', extensions: ['js', 'jsx', 'mjs', 'cjs'] },
    { language: 'C', extensions: ['c', 'h'] },
    { language: 'C++', extensions: ['cc', 'cpp', 'cxx', 'hpp', 'hh'] },
    { language: 'Kotlin', extensions: ['kt', 'kts'] },
    { language: 'Swift', extensions: ['swift'] },
    { language: 'Objective-C', extensions: ['m', 'mm'] },
  ],
  detector: 'preflight@1',
};

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
    adapters: INVENTORY,
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

export const AdapterInventoryExplainsAndRecommends: Story = {
  // AC-0087 (#163): installed adapters with coverage, the word "adapter"
  // explained in place, and planned types wired to the request lane.
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText('Adapters')).toBeInTheDocument();
    // Plain-language explanation distinguishes adapters from frameworks
    // and from toolchain versions.
    await expect(
      canvas.getByText(/framework markers Preflight detects, not adapters/),
    ).toBeInTheDocument();
    await expect(canvas.getByText(/a JDK bump never needs a new adapter/)).toBeInTheDocument();

    const installed = canvas.getByRole('list', { name: 'Installed adapters' });
    await expect(within(installed).getAllByRole('listitem')).toHaveLength(4);
    await expect(within(installed).getByText('TypeScript')).toBeInTheDocument();
    await expect(within(installed).getByText('t0.webextension')).toBeInTheDocument();
    await expect(within(installed).getByText('t0.adapter-java')).toBeInTheDocument();
    await expect(within(installed).getByText(/\.ts \.tsx/)).toBeInTheDocument();
    // Provably the Preflight registry: the shared detector id is stated.
    await expect(canvas.getByText('preflight@1')).toBeInTheDocument();

    const planned = canvas.getByRole('list', { name: 'Planned adapters' });
    await expect(within(planned).getAllByRole('listitem')).toHaveLength(6);
    for (const language of ['JavaScript', 'C', 'C++', 'Kotlin', 'Swift', 'Objective-C']) {
      await expect(within(planned).getByText(language)).toBeInTheDocument();
    }
    const links = within(planned).getAllByRole('link', { name: 'Request this adapter' });
    await expect(links).toHaveLength(6);
    await expect(links[3]).toHaveAttribute(
      'href',
      expect.stringContaining('title=Adapter%20request%3A%20Kotlin'),
    );
  },
};

export const DiscoveredPluginsToggle: Story = {
  // #198 (AC-0068 slice): discovered plugins list with scope, content hash,
  // and a per-project fail-closed toggle — off until explicitly enabled.
  args: {
    plugins: [
      {
        id: 't0.adapter-ruby',
        path: '/repo/.cartograph/adapters/t0.adapter-ruby.wasm',
        content_hash: 'abcdef0123456789abcdef',
        scope: 'project',
        shadowed_user_copy: true,
        enabled: false,
      },
      {
        id: 't0.adapter-swift',
        path: '/home/u/adapters/t0.adapter-swift.wasm',
        content_hash: '9876543210fedcba987654',
        scope: 'user',
        shadowed_user_copy: false,
        enabled: true,
      },
    ],
    onTogglePlugin: fn(),
  },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const list = within(canvas.getByRole('list', { name: 'Discovered plugins' }));
    await expect(list.getAllByRole('listitem')).toHaveLength(2);
    // Fail-closed default is stated, plus discovery provenance.
    await expect(canvas.getByText(/off until you enable it here/)).toBeInTheDocument();
    await expect(list.getByText(/Project copy \(shadows a user-level copy\)/)).toBeInTheDocument();
    await expect(list.getByText('abcdef012345')).toBeInTheDocument();

    const ruby = list.getByRole('switch', { name: 't0.adapter-ruby enabled for this project' });
    await expect(ruby).toHaveAttribute('aria-checked', 'false');
    await userEvent.click(ruby);
    await expect(args.onTogglePlugin).toHaveBeenCalledWith('t0.adapter-ruby', true);

    const swift = list.getByRole('switch', { name: 't0.adapter-swift enabled for this project' });
    await expect(swift).toHaveAttribute('aria-checked', 'true');
  },
};

export const NoBackend: Story = {
  args: { tiers: [], canEdit: false, adapters: null },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByText(/connect a backend to manage it\. Everything runs local-only/),
    ).toBeInTheDocument();
    // T0's floor statement renders even with no core.
    await expect(canvas.getByText('always on')).toBeInTheDocument();
    // No fabricated inventory without a core to report it.
    await expect(
      canvas.getByText(/connect a backend to list installed adapters/),
    ).toBeInTheDocument();
  },
};
