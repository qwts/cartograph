import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC as mockTauriIPC } from '@tauri-apps/api/mocks';
import { mergeInvestigationEvents, useInvestigationStore } from './investigationStore';
import { investigationCitations, investigationConsent, investigationDetail, investigationEvents, investigationResult } from './investigationFixtures';
import type { InvestigationCitationRead, InvestigationDetail, InvestigationEvent, StartInvestigationRequest } from './investigationTypes';

// These commands use named JSON arguments; Tauri also supports unrelated binary payloads.
function mockIPC(handler: (command: string, args?: Record<string, unknown>) => unknown) {
  mockTauriIPC((command, args) => handler(command, args as Record<string, unknown> | undefined));
}

function deferred<T>() {
  let resolve: (value: T) => void = () => {};
  let reject: (error: Error) => void = () => {};
  const promise = new Promise<T>((success, failure) => { resolve = success; reject = failure; });
  return { promise, resolve, reject };
}

function response(command: string, args?: Record<string, unknown>) {
  const id = args?.investigationId as string;
  switch (command) {
    case 'get_investigation': return investigationDetail(id);
    case 'investigation_result': return investigationResult(id);
    case 'investigation_consent': return null;
    case 'investigation_events': {
      const events = investigationEvents(id);
      return { investigation_id: id, items: events.filter((event) => event.sequence > Number(args?.afterSequence ?? 0)),
        next_sequence: events.length, has_more: false };
    }
    default: throw new Error('Unexpected command in fixture.');
  }
}

beforeEach(() => {
  vi.stubGlobal('window', {});
  useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
});
afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
  useInvestigationStore.setState(useInvestigationStore.getInitialState(), true);
});

describe('durable investigation observations (AC-0190)', () => {
  it('preserves older explicit pages and their cursor when refreshing ordered recent history', async () => {
    let recent = investigationDetail('recent', { revision: 10 });
    mockIPC((command, args) => {
      expect(command).toBe('list_investigations');
      return args?.cursor ? { items: [investigationDetail('older')], next_cursor: 'after-older' }
        : { items: [recent], next_cursor: 'after-recent' };
    });
    await useInvestigationStore.getState().loadHistory();
    await useInvestigationStore.getState().loadHistory(true);
    recent = investigationDetail('recent', { revision: 9, question: 'Stale summary' });
    await useInvestigationStore.getState().loadHistory();
    expect(useInvestigationStore.getState().history.map((item) => item.investigation_id)).toEqual(['recent', 'older']);
    expect(useInvestigationStore.getState().history[0].revision).toBe(10);
    expect(useInvestigationStore.getState().nextCursor).toBe('after-older');
  });

  it('orders exact duplicate pages and rejects gaps, foreign identities and conflicting events', () => {
    const events = investigationEvents('original');
    expect(mergeInvestigationEvents('original', events.slice(0, 4), events.slice(3))).toEqual(events);
    expect(() => mergeInvestigationEvents('original', [], [events[1]])).toThrow('Missing event');
    expect(() => mergeInvestigationEvents('other', [], [events[0]])).toThrow('Invalid event identity');
    expect(() => mergeInvestigationEvents('original', [events[0]], [{ ...events[0], summary: 'changed' }])).toThrow('Conflicting event');
  });

  it('recovers an uncertain start with exactly the original nonce and request', async () => {
    const starts: StartInvestigationRequest[] = [];
    mockIPC((command, args) => {
      if (command === 'start_investigation') {
        starts.push((args as { request: StartInvestigationRequest }).request);
        if (starts.length === 1) throw new Error('Transport lost after durable admission.');
        return investigationDetail('same-durable-task');
      }
      return response(command, args);
    });
    await useInvestigationStore.getState().start({ specialist_id: 'domain-analyst@2', question: 'Inspect this scope.',
      scope: { type: 'all' }, provider_mode: 'local', limit_profile: 'investigation-v1' });
    expect(useInvestigationStore.getState().pendingStart).toEqual(starts[0]);
    await useInvestigationStore.getState().start({ specialist_id: 'evidence-auditor@2', question: 'Cannot replace pending start.',
      scope: { type: 'all' }, provider_mode: 'local', limit_profile: 'investigation-v1' });
    expect(starts).toHaveLength(1);
    await useInvestigationStore.getState().retryStart();
    expect(starts).toHaveLength(2);
    expect(starts[1]).toEqual(starts[0]);
    expect(starts[0].request_nonce.length).toBeGreaterThan(0);
    expect(useInvestigationStore.getState().selectedId).toBe('same-durable-task');
    expect(useInvestigationStore.getState().pendingStart).toBeNull();
  });

  it('ignores an older task response after selecting another task', async () => {
    const first = deferred<InvestigationDetail>();
    mockIPC((command, args) => command === 'get_investigation' && args?.investigationId === 'first'
      ? first.promise : response(command, args));
    const earlier = useInvestigationStore.getState().open('first');
    await useInvestigationStore.getState().open('second');
    first.resolve(investigationDetail('first'));
    await earlier;
    expect(useInvestigationStore.getState().detail?.investigation_id).toBe('second');
    expect(useInvestigationStore.getState().result?.investigation_id).toBe('second');
    expect(useInvestigationStore.getState().events.every((event) => event.investigation_id === 'second')).toBe(true);
  });

  it('ignores a superseded same-task failure and an earlier same-ID reopen', async () => {
    const first = deferred<InvestigationDetail>();
    const reopened = deferred<InvestigationDetail>();
    let calls = 0;
    mockIPC((command, args) => {
      if (command === 'get_investigation') {
        calls += 1;
        if (calls === 1) return first.promise;
        if (calls === 3) return reopened.promise;
      }
      return response(command, args);
    });
    const earlier = useInvestigationStore.getState().open('same');
    await useInvestigationStore.getState().refreshSelected();
    first.reject(new Error('Earlier failure with private details.'));
    await earlier;
    expect(useInvestigationStore.getState().error).toBeNull();
    const earlierOpen = useInvestigationStore.getState().open('same');
    await useInvestigationStore.getState().open('same');
    reopened.resolve(investigationDetail('same', { question: 'Stale question' }));
    await earlierOpen;
    expect(useInvestigationStore.getState().detail?.question).not.toBe('Stale question');
  });

  it('retains saved activity when a page is missing and recovers by reading the durable cursor', async () => {
    const events = investigationEvents('same');
    let complete = false;
    const cursors: unknown[] = [];
    mockIPC((command, args) => {
      if (command === 'investigation_events') {
        cursors.push(args?.afterSequence);
        return { investigation_id: 'same', items: complete ? events : events.filter((event) => event.sequence !== 4),
          next_sequence: 9, has_more: false };
      }
      return response(command, args);
    });
    await useInvestigationStore.getState().open('same');
    expect(useInvestigationStore.getState().events).toEqual([]);
    expect(useInvestigationStore.getState().error).toContain('could not be refreshed consistently');
    complete = true;
    await useInvestigationStore.getState().refreshSelected();
    expect(cursors).toEqual([0, 0]);
    expect(useInvestigationStore.getState().events).toEqual(events);
    expect(useInvestigationStore.getState().error).toBeNull();
  });

  it('retains an admitted late result while cancellation remains authoritative', async () => {
    // AC-0187/0190: task status is not rewritten as completed to display a saved answer.
    mockIPC((command, args) => command === 'get_investigation'
      ? investigationDetail('same', { status: 'cancelled', cancel_requested: true,
        actions: { can_cancel: false, can_follow_up: true } }) : response(command, args));
    await useInvestigationStore.getState().open('same');
    expect(useInvestigationStore.getState().detail?.status).toBe('cancelled');
    expect(useInvestigationStore.getState().result).toEqual(investigationResult('same'));
  });

  it('fetches multiple journal pages sequentially without inventing or dropping activity', async () => {
    const events: InvestigationEvent[] = Array.from({ length: 70 }, (_, index) => ({
      ...investigationEvents('paged')[0], sequence: index + 1, revision: index + 1,
    }));
    const cursors: number[] = [];
    mockIPC((command, args) => {
      if (command === 'get_investigation') return investigationDetail('paged', { last_event_sequence: 70, revision: 70 });
      if (command === 'investigation_events') {
        const after = Number(args?.afterSequence);
        cursors.push(after);
        const items = events.slice(after, after + 50);
        return { investigation_id: 'paged', items, next_sequence: items.at(-1)!.sequence,
          has_more: after + 50 < events.length };
      }
      return response(command, args);
    });
    await useInvestigationStore.getState().open('paged');
    expect(cursors).toEqual([0, 50]);
    expect(useInvestigationStore.getState().events).toEqual(events);
  });
});

describe('investigation consent and historical reads (AC-0186/0189/0190)', () => {
  it('submits exactly the displayed step revision and hash, and never approves a later step implicitly', async () => {
    let step = 1;
    const approvals: unknown[] = [];
    mockIPC((command, args) => {
      if (command === 'get_investigation') return investigationDetail('same', { status: 'awaiting_consent',
        provider_mode: 'cloud', revision: step + 3, actions: { can_cancel: true, can_follow_up: false } });
      if (command === 'investigation_consent') return investigationConsent('same', step + 3, `step-${step}`);
      if (command === 'approve_investigation_step') {
        approvals.push(args); step = 2;
        return investigationDetail('same', { status: 'awaiting_consent', revision: 5 });
      }
      return response(command, args);
    });
    await useInvestigationStore.getState().open('same');
    useInvestigationStore.getState().showConsent();
    await useInvestigationStore.getState().approve();
    expect(approvals).toEqual([{ investigationId: 'same', stepId: 'step-1', revision: 4, payloadHash: 'payload:same:step-1' }]);
    expect(useInvestigationStore.getState().consent?.step_id).toBe('step-2');
    expect(useInvestigationStore.getState().consentOpen).toBe(false);
  });

  it('clears stale consent observations without inventing local fallback or a grant', async () => {
    const commands: string[] = [];
    mockIPC((command, args) => {
      commands.push(command);
      if (command === 'get_investigation') return investigationDetail('same', { status: 'cancelled', cancel_requested: true });
      if (command === 'investigation_consent') return investigationConsent('same');
      return response(command, args);
    });
    await useInvestigationStore.getState().open('same');
    useInvestigationStore.getState().showConsent();
    await useInvestigationStore.getState().approve();
    expect(useInvestigationStore.getState().consent).toBeNull();
    expect(commands.some((command) => command.includes('approve') || command.includes('start'))).toBe(false);
  });

  it('discards late historical source when the selected task changes', async () => {
    const first = deferred<InvestigationCitationRead>();
    const reads: unknown[] = [];
    mockIPC((command, args) => {
      if (command === 'read_investigation_citation') { reads.push(args); return first.promise; }
      return response(command, args);
    });
    await useInvestigationStore.getState().open('first');
    const pending = useInvestigationStore.getState().readCitation('C1');
    await useInvestigationStore.getState().open('second');
    first.resolve({ investigation_id: 'first', citation_id: 'C1', citation: investigationCitations[0],
      status: 'available', text: 'ORIGINAL HISTORICAL SOURCE' });
    await pending;
    expect(reads).toEqual([{ investigationId: 'first', citationId: 'C1' }]);
    expect(useInvestigationStore.getState().citationRead).toBeNull();
    expect(useInvestigationStore.getState().citationId).toBeNull();
  });

  it('rejects source-bearing legacy reads and never calls a current checkout reader', async () => {
    const commands: string[] = [];
    mockIPC((command, args) => {
      commands.push(command);
      if (command === 'read_investigation_citation') return { investigation_id: 'same', citation_id: 'C3',
        citation: investigationCitations[2], status: 'working_tree_unverified', text: 'MUST NOT DISPLAY' };
      return response(command, args);
    });
    await useInvestigationStore.getState().open('same');
    await useInvestigationStore.getState().readCitation('C3');
    expect(useInvestigationStore.getState().citationRead).toBeNull();
    expect(useInvestigationStore.getState().citationError).toContain('No current checkout source was substituted');
    expect(commands.filter((command) => command.startsWith('read_'))).toEqual(['read_investigation_citation']);
  });
});
