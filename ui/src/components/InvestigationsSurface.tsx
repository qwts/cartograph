import { useRef, useState } from 'react';
import type { InvestigationState } from '../investigationStore';
import type { GraphNode } from '../store';
import type { InvestigationProviderMode, InvestigationScope, InvestigationStatus, SpecialistId } from '../investigationTypes';
import { InvestigationActivity } from './InvestigationActivity';
import { InvestigationFindings } from './InvestigationFindings';
import { EgressConsentDialog } from './EgressConsentDialog';

export const INVESTIGATION_STATUS: Record<InvestigationStatus, string> = {
  queued: 'Queued', preparing: 'Preparing context', running: 'Running', awaiting_consent: 'Awaiting cloud consent',
  completed: 'Execution completed', failed: 'Failed', cancelled: 'Cancelled',
  interrupted: 'Interrupted', outcome_unknown: 'Model outcome unknown',
};

export interface InvestigationsSurfaceProps {
  state: InvestigationState;
  anchors: GraphNode[];
  canStart: boolean;
}

/** Props/callbacks only: the coordinator and dedicated store own task lifecycle. */
export function InvestigationsSurface({ state, anchors, canStart }: InvestigationsSurfaceProps) {
  const [specialistId, setSpecialistId] = useState<SpecialistId>('domain-analyst@2');
  const [providerMode, setProviderMode] = useState<InvestigationProviderMode>('local');
  const [question, setQuestion] = useState('');
  const [scopeType, setScopeType] = useState<'all' | 'neighborhood'>('all');
  const [anchor, setAnchor] = useState('');
  const [hops, setHops] = useState(1);
  const [parent, setParent] = useState<{ conversation_id: string; parent_id: string } | null>(null);
  const questionRef = useRef<HTMLTextAreaElement>(null);
  const provider = state.catalog?.providers.find((item) => item.mode === providerMode);
  const summary = state.detail ?? state.history.find((item) => item.investigation_id === state.selectedId);
  const questionBytes = new TextEncoder().encode(question).length;
  const locked = state.starting || state.pendingStart !== null;
  const scope: InvestigationScope = scopeType === 'all' ? { type: 'all' } : { type: 'neighborhood', anchor, hops };
  const startEnabled = canStart && !locked && Boolean(provider?.available) &&
    Boolean(state.catalog?.specialists.some((item) => item.id === specialistId)) && question.trim().length > 0 &&
    questionBytes <= 2048 && (scopeType === 'all' || anchor.trim().length > 0);
  const refresh = () => { void state.loadCatalog(); void state.loadHistory(); };

  return <section className="investigations-surface" aria-label="Investigations">
    <header className="investigation-heading"><div><h2>Investigations</h2>
      <p className="muted">Ask a specialist to inspect recovered context and evidence. Findings remain T3 / InferredWeak.</p></div>
      <button type="button" className="secondary-button" onClick={refresh} disabled={state.catalogLoading || state.historyLoading}>Refresh investigations</button>
    </header>
    <form className="investigation-compose" aria-label="Start an investigation" onSubmit={(event) => {
      event.preventDefault();
      if (startEnabled && state.catalog) void state.start({ specialist_id: specialistId, question,
        scope, provider_mode: providerMode, limit_profile: state.catalog.limits.profile, ...parent });
    }}>
      <fieldset disabled={locked}><legend>Specialist</legend>
        <div className="investigation-specialists">{state.catalog?.specialists.map((specialist) =>
          <label key={specialist.id} className={`investigation-specialist${specialistId === specialist.id ? ' selected' : ''}`}>
            <input type="radio" name="specialist" checked={specialistId === specialist.id} onChange={() => setSpecialistId(specialist.id)} />
            <span><strong>{specialist.name}</strong><span className="muted">{specialist.purpose}</span>
              <code>{specialist.id} · T3 / InferredWeak</code></span>
          </label>)}</div>
      </fieldset>
      {state.catalogLoading && <p role="status">Loading specialists…</p>}
      {state.catalogError && <p className="error-text" role="alert">{state.catalogError}</p>}
      {!state.catalog && !state.catalogLoading && <p className="muted">The app core supplies available specialists and provider limits.</p>}
      <div className="investigation-form-row">
        <label>Scope<select value={scopeType} disabled={locked} onChange={(event) => setScopeType(event.target.value as 'all' | 'neighborhood')}>
          <option value="all">Recovered system</option><option value="neighborhood">Fact neighborhood</option>
        </select></label>
        <label>Provider<select value={providerMode} disabled={locked} onChange={(event) => setProviderMode(event.target.value as InvestigationProviderMode)}>
          <option value="local">Local</option><option value="cloud">Cloud · consent for every step</option>
        </select></label>
        {scopeType === 'neighborhood' && <>
          <label className="investigation-anchor">Anchor fact ID<input value={anchor} list="investigation-anchor-options" disabled={locked}
            onChange={(event) => setAnchor(event.target.value)} maxLength={2048} />
            <datalist id="investigation-anchor-options">{anchors.slice(0, 200).map((node) => <option key={node.id} value={node.id}>{node.label}</option>)}</datalist></label>
          <label>Hops<select value={hops} disabled={locked} onChange={(event) => setHops(Number(event.target.value))}>
            <option value={1}>1</option><option value={2}>2</option><option value={3}>3</option>
          </select></label>
        </>}
      </div>
      {scopeType === 'neighborhood' && <p className="muted">Suggestions show up to 200 loaded facts. The host validates the exact anchor and neighborhood.</p>}
      {provider && <p className={provider.available ? 'muted' : 'error-text'}>
        {provider.provider_id} · {provider.model}{!provider.available && ` — ${provider.unavailable_reason ?? 'Unavailable'}`}
      </p>}
      <label className="investigation-question">{parent ? 'Follow-up question' : 'Question'}
        <textarea ref={questionRef} value={question} disabled={locked} maxLength={2048} rows={3}
          placeholder={specialistId.startsWith('domain-analyst@') ? 'What business behavior is evidenced in this scope, and what remains unknown?' : 'Which claims have supporting evidence, and where is that evidence incomplete?'}
          onChange={(event) => setQuestion(event.target.value)} />
      </label>
      <p className={questionBytes > 2048 ? 'error-text' : 'muted'}>{questionBytes} / 2,048 UTF-8 bytes. Scope uses the recovered graph at preparation time.</p>
      {parent && <p className="muted">New task following <code>{parent.parent_id}</code>; earlier findings remain T3 history.
        <button type="button" className="secondary-button" disabled={locked} onClick={() => setParent(null)}>Start a separate conversation</button></p>}
      {state.catalog && <p className="muted">Up to {state.catalog.limits.model_invocations} model invocations and {state.catalog.limits.tool_actions} tool actions;
        {' '}{state.catalog.limits.active_seconds}s active execution. Source and byte limits apply. Missing evidence is not proof of absence.</p>}
      <button type="submit" disabled={!startEnabled}>{state.starting ? 'Saving investigation…' : parent ? 'Start follow-up' : 'Start investigation'}</button>
      {state.startError && <div role="alert" className="error-text"><p>{state.startError}</p>
        <button type="button" className="secondary-button" disabled={state.starting} onClick={() => void state.retryStart()}>Retry same start request</button></div>}
    </form>

    <div className="investigation-layout">
      <aside className="investigation-history" aria-label="Investigation history">
        <h3>Saved investigations</h3><p className="muted">Questions, activity and findings survive restart and job cleanup.</p>
        {state.historyLoading && <p role="status">Loading history…</p>}
        {state.historyError && <p role="alert" className="error-text">{state.historyError}</p>}
        {!state.historyLoading && state.history.length === 0 && <p className="muted">No saved investigations loaded.</p>}
        <ul>{state.history.map((item) => <li key={item.investigation_id}>
          <button type="button" className={`investigation-history-item${item.investigation_id === state.selectedId ? ' selected' : ''}`}
            aria-pressed={item.investigation_id === state.selectedId} onClick={() => void state.open(item.investigation_id)}>
            <strong>{item.question}</strong><span>{item.specialist_id}</span><span>{INVESTIGATION_STATUS[item.status]}</span>
            <time>{item.created_at}</time>{item.cancel_requested && <span>Cancellation requested</span>}
          </button></li>)}</ul>
        {state.nextCursor && <button type="button" className="secondary-button" disabled={state.historyLoading} onClick={() => void state.loadHistory(true)}>Load older investigations</button>}
      </aside>
      <div className="investigation-detail" aria-label="Selected investigation">
        {!state.selectedId ? <p className="muted">Start an investigation or select its saved history.</p> : <>
          <div className="investigation-heading"><div><h3>{summary?.question ?? 'Loading investigation'}</h3><code>{state.selectedId}</code></div>
            <button type="button" className="secondary-button" disabled={state.loading} onClick={() => void state.refreshSelected()}>Refresh selected investigation</button></div>
          {summary && <>
            <p role="status">{INVESTIGATION_STATUS[summary.status]}{summary.cancel_requested && ' · Cancellation requested'}</p>
            {summary.invocation_pending && <p className="investigation-notice">A model request is outstanding. Its outcome is pending;
              cancellation does not immediately terminate remote inference. A valid completed answer can still be retained.</p>}
            {summary.status === 'outcome_unknown' && <p className="investigation-notice">The previous model outcome is unknown. This task will not be replayed automatically.</p>}
            {summary.status === 'interrupted' && <p className="investigation-notice">The original worker input is no longer available. A follow-up uses a new task and basis.</p>}
            <p className="muted">Scope: {summary.scope.type === 'all' ? 'Recovered system' : `${summary.scope.anchor} · ${summary.scope.hops} hop(s)`}</p>
            <p className="muted">Prepared graph: <code>{summary.graph_snapshot_id ?? 'Pending preparation'}</code></p>
            <div className="investigation-actions">
              {summary.actions.can_cancel && <button type="button" className="danger-button" disabled={state.actionBusy}
                onClick={() => void state.cancel()}>Request cancellation</button>}
              {summary.actions.can_follow_up && <button type="button" className="secondary-button" disabled={locked} onClick={() => {
                setParent({ conversation_id: summary.conversation_id, parent_id: summary.investigation_id });
                setScopeType(summary.scope.type); if (summary.scope.type === 'neighborhood') { setAnchor(summary.scope.anchor); setHops(summary.scope.hops); }
                setSpecialistId(summary.specialist_id); setProviderMode(summary.provider_mode); setQuestion(''); questionRef.current?.focus();
              }}>Ask a follow-up</button>}
              {state.consent && <button type="button" disabled={state.actionBusy || state.loading} onClick={state.showConsent}>Review next cloud action</button>}
            </div>
          </>}
          {state.loading && !state.detail && <p role="status">Loading saved investigation…</p>}
          {state.error && <p className="error-text" role="alert">{state.error}</p>}
          {state.actionError && <p className="error-text" role="alert">{state.actionError}</p>}
          {state.detail && <>
            <details className="investigation-section"><summary>Specialist, provider and consumed limits</summary>
              <p>{state.detail.specialist.name} · <code>{state.detail.specialist.id}</code> · T3 / InferredWeak</p>
              <p>Prompt fingerprint: <code>{state.detail.specialist.prompt_fingerprint}</code></p>
              <p>Configured model: <code>{state.detail.provider.provider_id} / {state.detail.provider.model}</code></p>
              <p>Model invocations used: {state.detail.usage.model_invocations} / {state.detail.limits.model_invocations};
                tool actions used: {state.detail.usage.tool_actions} / {state.detail.limits.tool_actions}.</p>
              <p>Admitted facts: {state.detail.usage.selected_facts}; copied evidence: {state.detail.usage.evidence_items} items / {state.detail.usage.evidence_bytes} bytes.</p>
              <p>Captured-file validation: {state.detail.usage.captured_validation_bytes} / {state.detail.limits.captured_validation_bytes} bytes.</p>
              <p>Generated-token reservations: {state.detail.usage.generated_token_reservations} / {state.detail.limits.generated_token_reservations}.</p>
              <p>Reported input/output tokens: {state.detail.usage.reported_input_tokens ?? 'Unavailable'} / {state.detail.usage.reported_output_tokens ?? 'Unavailable'}.</p>
              <p className="muted">Reservations are limits, not measured token use or a monetary guarantee.</p>
            </details>
            {state.detail.error && <p className="error-text" role="alert">{state.detail.error}</p>}
          </>}
          <InvestigationActivity events={state.events} />
          <InvestigationFindings result={state.result} citations={state.detail?.citations ?? []}
            citationId={state.citationId} read={state.citationRead} loading={state.citationLoading} error={state.citationError}
            onRead={(id) => void state.readCitation(id)} onClose={state.clearCitation} />
        </>}
      </div>
    </div>
    {state.consentOpen && state.consent && state.consent.investigation_id === state.selectedId && <EgressConsentDialog
      preview={state.consent.preview} busy={state.actionBusy} cancelLabel="Decline this action"
      onCancel={() => void state.decline()} onConsent={() => void state.approve()}
      onClose={state.hideConsent} closeLabel="Review later"
      additionalDetails={<><p>Investigation <code>{state.consent.investigation_id}</code> · step <code>{state.consent.step_id}</code></p>
        <p>Model <code>{state.consent.provider.model}</code> · endpoint <code>{state.consent.provider.endpoint}</code></p>
        <p>Deployment: {state.consent.provider.deployment ?? 'Not supplied'} · expires {state.consent.expires_at}</p>
        <p>Declared step limits: {state.consent.completion_limits.max_output_tokens} generated tokens;
          {' '}{state.consent.completion_limits.request_timeout_ms} ms request timeout. The actual deadline may narrow as task time elapses.</p>
        <details><summary>Exact provider profile and step limits</summary>
          <pre>{JSON.stringify({ profile: state.consent.provider_profile, limits: state.consent.completion_limits }, null, 2)}</pre></details>
        <p>Approval covers this step only. Declining does not switch to a local provider; reviewing later leaves this task waiting.</p></>} />}
  </section>;
}
