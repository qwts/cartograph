import type { AdapterInventory, CloudDisclosure, TierSettings } from '../store';

export interface SettingsSurfaceProps {
  tiers: TierSettings[];
  /** Status-bar egress line, repeated in the local-core banner. */
  egressLabel: string;
  /** Per-tier consent disclosures; a missing one blocks consent (fail closed). */
  disclosures: Partial<Record<string, CloudDisclosure>>;
  /** Installed + planned adapters (#163) — the registry Preflight uses. */
  adapters: AdapterInventory | null;
  error: string | null;
  /** Disabled controls when there is no live backend to persist into. */
  canEdit: boolean;
  onToggleTier: (tier: string, enabled: boolean) => void;
  onProviderChange: (tier: string, provider: 'local' | 'cloud') => void;
  onGrantConsent: (tier: string) => void;
  onRevokeConsent: (tier: string) => void;
}

/** Request-adapter lane (epic #147): a prefilled issue names the adapter. */
function requestAdapterUrl(language: string): string {
  const title = encodeURIComponent(`Adapter request: ${language}`);
  return `https://github.com/qwts/cartograph/issues/new?title=${title}&labels=adapter-request`;
}

/** Copy per configurable tier (handoff §Settings). */
const TIER_COPY: Record<string, { name: string; description: string }> = {
  T1: {
    name: 'Dynamic observation',
    description: 'Execution-derived evidence: traces, test runs, state exports. Local instrumentation only.',
  },
  T2: {
    name: 'Semantic',
    description: 'Local embeddings + similarity over symbols and channels. Cloud is opt-in per this tier.',
  },
  T3: {
    name: 'Agentic',
    description: 'Multi-step reasoning over assembled context. Propose-only — never overwrites T0/T1.',
  },
};

/** An LLM tier can point at a cloud provider; T1 is local instrumentation. */
function hasProviderChoice(tier: string): boolean {
  return tier === 'T2' || tier === 'T3';
}

function ConsentPanel({
  tier,
  disclosure,
  canEdit,
  onGrant,
}: {
  tier: TierSettings;
  disclosure: CloudDisclosure | undefined;
  canEdit: boolean;
  onGrant: () => void;
}) {
  if (tier.consented) {
    return (
      <div className="consent-panel consented" data-testid={`consent-${tier.tier}`}>
        <p className="consent-status">
          <span className="material-symbols-outlined" aria-hidden="true">
            verified_user
          </span>
          Standing cloud consent granted{tier.consented_at ? ` · ${tier.consented_at}` : ''}. Every
          call still shows its exact payload for a per-action grant.
        </p>
      </div>
    );
  }
  if (!disclosure) {
    // Fail closed: nothing to disclose means nothing to consent to.
    return (
      <div className="consent-panel" data-testid={`consent-${tier.tier}`}>
        <p className="muted">
          Consent disclosure unavailable — cloud consent cannot be recorded until the core
          provides it.
        </p>
      </div>
    );
  }
  return (
    <div className="consent-panel" data-testid={`consent-${tier.tier}`}>
      <h4>
        <span className="material-symbols-outlined" aria-hidden="true">
          cloud_upload
        </span>
        Cloud egress consent — {tier.tier}
      </h4>
      <dl className="consent-meta">
        <div>
          <dt>Provider / model</dt>
          <dd>
            {disclosure.provider} · <code>{disclosure.model}</code>
          </dd>
        </div>
        <div>
          <dt>Endpoint</dt>
          <dd>
            <code>{disclosure.endpoint}</code>
          </dd>
        </div>
        <div>
          <dt>Estimated cost</dt>
          <dd>
            ${disclosure.input_usd_per_mtok}/M input · ${disclosure.output_usd_per_mtok}/M output
            tokens
          </dd>
        </div>
      </dl>
      <ul className="consent-notes">
        {disclosure.notes.map((note) => (
          <li key={note}>{note}</li>
        ))}
      </ul>
      <p className="muted">
        Default stays local-only. No cloud call is possible until granted, and each call still
        requires a per-payload grant showing the exact spans leaving the device.
      </p>
      <button type="button" disabled={!canEdit} onClick={onGrant}>
        Grant revocable consent
      </button>
    </div>
  );
}

/** Settings surface (handoff §Settings, screenshots 08–09): recovery tiers,
 *  providers, and the fail-closed cloud egress consent flow. T0 renders
 *  locked-on — it is not configuration, it is the product's floor. */
export function SettingsSurface({
  tiers,
  egressLabel,
  disclosures,
  adapters,
  error,
  canEdit,
  onToggleTier,
  onProviderChange,
  onGrantConsent,
  onRevokeConsent,
}: SettingsSurfaceProps) {
  return (
    <section className="settings-surface" aria-label="Settings">
      <header className="ingest-hero">
        <h2>Settings</h2>
        <p className="muted">
          Recovery tiers, providers, and egress. Cloud fails closed — nothing leaves the device
          unless a tier is explicitly opted in here.
        </p>
      </header>

      <p className="reassure-strip settings-banner">
        <span className="material-symbols-outlined" aria-hidden="true">
          verified_user
        </span>
        <span>
          <strong>Local core · on-device by default</strong>
          <code className="settings-egress">{egressLabel}</code>
        </span>
      </p>

      {error && <p className="error-text">{error}</p>}

      <h3 className="settings-section-title">Recovery tiers</h3>
      <ul className="tier-cards">
        <li className="tier-card">
          <span className="tier-code">T0</span>
          <div className="tier-main">
            <h4>Deterministic</h4>
            <p className="muted">
              Static parse, call graph, literal channel identity. Always local — never invokes an
              LLM.
            </p>
          </div>
          <span className="tier-locked">
            <span className="material-symbols-outlined" aria-hidden="true">
              lock
            </span>
            always on
          </span>
        </li>
        {tiers.map((tier) => {
          const copy = TIER_COPY[tier.tier] ?? { name: tier.tier, description: '' };
          return (
            <li key={tier.tier} className="tier-card">
              <span className="tier-code">{tier.tier}</span>
              <div className="tier-main">
                <h4>{copy.name}</h4>
                <p className="muted">{copy.description}</p>
                {hasProviderChoice(tier.tier) && tier.enabled && (
                  <div className="provider-row">
                    <span className="provider-label">Provider</span>
                    <div className="source-picker" role="radiogroup" aria-label={`${tier.tier} provider`}>
                      <button
                        type="button"
                        role="radio"
                        aria-checked={tier.provider === 'local'}
                        className={`source-option${tier.provider === 'local' ? ' active' : ''}`}
                        disabled={!canEdit}
                        onClick={() => onProviderChange(tier.tier, 'local')}
                      >
                        Local (Ollama)
                      </button>
                      <button
                        type="button"
                        role="radio"
                        aria-checked={tier.provider === 'cloud'}
                        className={`source-option${tier.provider === 'cloud' ? ' active' : ''}`}
                        disabled={!canEdit}
                        onClick={() => onProviderChange(tier.tier, 'cloud')}
                      >
                        Cloud (opt-in)
                      </button>
                    </div>
                    {tier.provider === 'cloud' && tier.consented && (
                      <button
                        type="button"
                        className="secondary-button revoke-button"
                        disabled={!canEdit}
                        onClick={() => onRevokeConsent(tier.tier)}
                      >
                        Revoke consent
                      </button>
                    )}
                  </div>
                )}
                {hasProviderChoice(tier.tier) && tier.enabled && tier.provider === 'cloud' && (
                  <ConsentPanel
                    tier={tier}
                    disclosure={disclosures[tier.tier]}
                    canEdit={canEdit}
                    onGrant={() => onGrantConsent(tier.tier)}
                  />
                )}
              </div>
              <button
                type="button"
                role="switch"
                aria-checked={tier.enabled}
                aria-label={`${copy.name} enabled`}
                className={`tier-toggle${tier.enabled ? ' on' : ''}`}
                disabled={!canEdit}
                onClick={() => onToggleTier(tier.tier, !tier.enabled)}
              >
                <span className="tier-toggle-knob" />
              </button>
            </li>
          );
        })}
      </ul>
      {tiers.length === 0 && (
        <p className="muted">
          Tier configuration lives in the core — connect a backend to manage it. Everything runs
          local-only until then.
        </p>
      )}

      <h3 className="settings-section-title">Adapters</h3>
      <p className="muted adapter-explainer">
        An adapter is the per-language (or per-format) extractor that turns source into Confirmed
        facts — one per language, not per tool or toolchain version: ESLint, Babel, or React are
        framework markers Preflight detects, not adapters, and a JDK bump never needs a new
        adapter — version-specific constructs degrade to explicit Unsupported findings, never
        guesses.
      </p>
      {adapters ? (
        <>
          <ul className="adapter-list" aria-label="Installed adapters">
            {adapters.installed.map((adapter) => (
              <li key={adapter.id} className="adapter-row">
                <div className="adapter-head">
                  <strong>{adapter.language}</strong>
                  <code>{adapter.id}</code>
                </div>
                <p className="muted">
                  {adapter.covers}. Files: {adapter.extensions.map((ext) => `.${ext}`).join(' ')}
                </p>
              </li>
            ))}
          </ul>
          <p className="muted">
            This list is the same registry Preflight consults (detector{' '}
            <code>{adapters.detector}</code>) — coverage and inventory cannot disagree.
          </p>
          <h4 className="settings-subsection-title">Known adapter types, not yet installed</h4>
          <ul className="adapter-list planned" aria-label="Planned adapters">
            {adapters.planned.map((planned) => (
              <li key={planned.language} className="adapter-row">
                <div className="adapter-head">
                  <strong>{planned.language}</strong>
                  <a
                    href={requestAdapterUrl(planned.language)}
                    target="_blank"
                    rel="noreferrer"
                    className="adapter-request-link"
                  >
                    Request this adapter
                  </a>
                </div>
                <p className="muted">
                  Detected via {planned.extensions.map((ext) => `.${ext}`).join(' ')} — Preflight
                  names it as uncovered until an adapter ships.
                </p>
              </li>
            ))}
          </ul>
        </>
      ) : (
        <p className="muted">
          Adapter inventory lives in the core — connect a backend to list installed adapters.
        </p>
      )}
    </section>
  );
}
