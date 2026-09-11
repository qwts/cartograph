import type { InvestigationEvent } from '../investigationTypes';

const LABELS: Record<InvestigationEvent['kind'], string> = {
  created: 'Investigation saved', preparation_started: 'Preparing context',
  context_prepared: 'Context prepared', model_started: 'Model request started',
  model_completed: 'Model response received', action_admitted: 'Action validated',
  tool_started: 'Tool started', tool_completed: 'Tool completed',
  evidence_validation_reserved: 'Source validation budget reserved',
  consent_required: 'Cloud action awaiting consent', consent_approved: 'Cloud action approved',
  consent_declined: 'Cloud action declined', cancel_requested: 'Cancellation requested',
  result_persisted: 'Findings saved', completed: 'Execution completed', failed: 'Execution failed',
  cancelled: 'Execution cancelled', interrupted: 'Execution interrupted', outcome_unknown: 'Model outcome unknown',
};

export interface InvestigationActivityProps { events: InvestigationEvent[] }

/** Only durable host events describe work. No simulated progress or model reasoning. */
export function InvestigationActivity({ events }: InvestigationActivityProps) {
  return <section className="investigation-section" aria-label="Investigation activity">
    <h3>Activity</h3>
    <p className="muted">Recorded tool and execution events, in host sequence order.</p>
    {events.length === 0 ? <p className="muted">No activity loaded yet.</p> :
      <ol className="investigation-events">
        {events.map((event) => <li key={event.sequence}>
          <span className="investigation-sequence" aria-label={`Event ${event.sequence}`}>{event.sequence}</span>
          <div><strong>{LABELS[event.kind] ?? 'Unsupported event'}</strong>
            {event.tool && <code className="investigation-tool">{event.tool}</code>}
            <p>{event.summary}</p>
            <p className="muted"><time>{event.created_at}</time>{event.step_id && <> · step <code>{event.step_id}</code></>}</p>
          </div>
        </li>)}
      </ol>}
  </section>;
}
