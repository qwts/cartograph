import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { usePrimarySourceStore, type CapturedDescription, type CapturedText, type RetainedSource } from './primarySourceStore';
import { useAppStore, type GraphNode } from './store';

const node = (id: string): GraphNode => ({ id, label: 'BusinessRule', props: {} });
const description = (id: string): CapturedDescription => ({
  fact: { kind: 'node', id }, receipt_id: `receipt:${id}`, emitted_fact_digest: `digest:${id}`,
  source_id: 'src_first', repo_key: 'local/src_first', scope: 'primary_source_only',
  input_closure: 'input_closure_not_established',
  ranges: [{ index: 0, path: 'rules.ts', byte_start: 0, byte_end: 5 }],
});

beforeEach(() => {
  vi.stubGlobal('window', {});
  usePrimarySourceStore.setState({ sources: [], retentionError: null, retentionMessage: null });
});

afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
  usePrimarySourceStore.getState().clear();
  usePrimarySourceStore.getState().dismissPreview();
});

describe('retained-source inventory reloads (AC-0154)', () => {
  const sources: RetainedSource[] = [{ source_id: 'src_current', repo_key: 'local/src_current', display_name: 'current' }];

  it('clears an earlier inventory error when a reload starts and succeeds', async () => {
    let reply: (value: RetainedSource[]) => void = () => {};
    let calls = 0;
    mockIPC(() => {
      calls += 1;
      if (calls === 1) throw new Error('inventory unavailable');
      return new Promise<RetainedSource[]>((resolve) => { reply = resolve; });
    });
    await usePrimarySourceStore.getState().loadSources();
    expect(usePrimarySourceStore.getState().retentionError).toBe('Retained-source inventory is unavailable.');
    const loading = usePrimarySourceStore.getState().loadSources();
    expect(usePrimarySourceStore.getState().retentionError).toBeNull();
    reply(sources);
    await loading;
    expect(usePrimarySourceStore.getState().sources).toEqual(sources);
    expect(usePrimarySourceStore.getState().retentionError).toBeNull();
  });

  it('ignores an older inventory failure after a newer reload succeeds', async () => {
    let reject: (reason: Error) => void = () => {};
    let calls = 0;
    mockIPC(() => {
      calls += 1;
      return calls === 1 ? new Promise<RetainedSource[]>((_, fail) => { reject = fail; }) : sources;
    });
    const earlier = usePrimarySourceStore.getState().loadSources();
    await usePrimarySourceStore.getState().loadSources();
    reject(new Error('late inventory failure'));
    await earlier;
    expect(usePrimarySourceStore.getState().sources).toEqual(sources);
    expect(usePrimarySourceStore.getState().retentionError).toBeNull();
  });

  it('ignores an older inventory success after the latest reload fails', async () => {
    let reply: (value: RetainedSource[]) => void = () => {};
    let calls = 0;
    mockIPC(() => {
      calls += 1;
      if (calls === 1) return new Promise<RetainedSource[]>((resolve) => { reply = resolve; });
      throw new Error('latest inventory failure');
    });
    const earlier = usePrimarySourceStore.getState().loadSources();
    await usePrimarySourceStore.getState().loadSources();
    reply(sources);
    await earlier;
    expect(usePrimarySourceStore.getState().sources).toEqual([]);
    expect(usePrimarySourceStore.getState().retentionError).toBe('Retained-source inventory is unavailable.');
  });

  it('preserves an unrelated retention-preview error on successful inventory reload', async () => {
    mockIPC((command) => {
      if (command === 'preview_forget_source') throw new Error('preview unavailable');
      return sources;
    });
    await usePrimarySourceStore.getState().previewSource('src_current');
    const previewError = usePrimarySourceStore.getState().retentionError;
    expect(previewError).toContain('retention preview');
    await usePrimarySourceStore.getState().loadSources();
    expect(usePrimarySourceStore.getState().sources).toEqual(sources);
    expect(usePrimarySourceStore.getState().retentionError).toBe(previewError);
  });
});

describe('captured source selection identity (AC-0154)', () => {
  it('ignores a late description from an earlier selection, including same-id reselection', async () => {
    let reply: (value: CapturedDescription) => void = () => {};
    let calls = 0;
    mockIPC((command) => {
      if (command === 'describe_captured_source') {
        calls += 1;
        return calls === 1 ? new Promise<CapturedDescription>((resolve) => { reply = resolve; }) : description('same');
      }
    });
    const first = usePrimarySourceStore.getState().inspect(node('same'));
    await usePrimarySourceStore.getState().inspect(node('same'));
    reply({ ...description('same'), receipt_id: 'old-receipt' });
    await first;
    expect(usePrimarySourceStore.getState().description?.receipt_id).toBe('receipt:same');
  });

  it('rejects a late captured response after switching facts without a live-source fallback', async () => {
    let reply: (value: CapturedText) => void = () => {};
    const calls: string[] = [];
    mockIPC((command, args) => {
      calls.push(command);
      if (command === 'describe_captured_source') return description(((args as Record<string, unknown>)?.fact as { id: string }).id);
      if (command === 'read_captured_source') return new Promise<CapturedText>((resolve) => { reply = resolve; });
    });
    await usePrimarySourceStore.getState().inspect(node('first'));
    const reading = usePrimarySourceStore.getState().readRange(0);
    await usePrimarySourceStore.getState().inspect(node('second'));
    reply({ receipt_id: 'receipt:first', range_index: 0, path: 'rules.ts', byte_start: 0, byte_end: 5, text: 'first' });
    await reading;
    expect(usePrimarySourceStore.getState().text).toBeNull();
    expect(usePrimarySourceStore.getState().description?.receipt_id).toBe('receipt:second');
    expect(calls).not.toContain('read_evidence');
  });

  it('rejects a response for a different receipt', async () => {
    mockIPC((command) => command === 'describe_captured_source' ? description('first') : {
      receipt_id: 'wrong', range_index: 0, path: 'rules.ts', byte_start: 0, byte_end: 5, text: 'wrong',
    });
    await usePrimarySourceStore.getState().inspect(node('first'));
    await usePrimarySourceStore.getState().readRange(0);
    expect(usePrimarySourceStore.getState().text).toBeNull();
    expect(usePrimarySourceStore.getState().error).toContain('unavailable');
  });

  it('sends exact source and preview fingerprint then clears a forgotten source buffer', async () => {
    const sent: unknown[] = [];
    mockIPC((command, args) => {
      if (command === 'preview_forget_source') return {
        source_id: 'src_first', repo_key: 'local/src_first', display_name: 'service', fingerprint: 'fixed-preview',
        captures: 1, files: 1, bytes: 5, receipts: 1, current_references: 1, historical_references: 0,
      };
      if (command === 'forget_retained_source') { sent.push(args); return 1; }
    });
    usePrimarySourceStore.setState({ description: description('first'), text: { receipt_id: 'receipt:first', range_index: 0, text: 'first', path: 'rules.ts', byte_start: 0, byte_end: 5 } });
    await usePrimarySourceStore.getState().previewSource('src_first');
    await usePrimarySourceStore.getState().forget();
    expect(sent).toEqual([{ sourceId: 'src_first', fingerprint: 'fixed-preview' }]);
    expect(usePrimarySourceStore.getState().text).toBeNull();
    expect(usePrimarySourceStore.getState().description).toBeNull();
  });
});


describe('working-tree selection cannot roll captured selection back (AC-0154)', () => {
  it('changes the selection token on same-object reselection and preserves it on live completion', async () => {
    const replies: ((value: unknown) => void)[] = [];
    mockIPC(() => new Promise((resolve) => { replies.push(resolve); }));
    const subject: GraphNode = { id: 'same', label: 'BusinessRule', props: { prov: {
      tier: 'Deterministic', confidence_tier: 'Confirmed', extractor_id: 'test', content_hash: 'same',
      evidence: [{ repo: 'local/src_first', path: 'rules.ts', byte_start: 0, byte_end: 3, commit_sha: 'workdir' }],
    } } };
    const first = useAppStore.getState().select(subject);
    const firstToken = useAppStore.getState().selected?.requestVersion;
    const second = useAppStore.getState().select(subject);
    const secondToken = useAppStore.getState().selected?.requestVersion;
    expect(secondToken).not.toBe(firstToken);
    replies[1]({ text: 'new', window_start: 0, truncated: false });
    await second;
    expect(useAppStore.getState().selected?.requestVersion).toBe(secondToken);
    replies[0]({ text: 'old', window_start: 0, truncated: false });
    await first;
    expect(useAppStore.getState().selected?.requestVersion).toBe(secondToken);
    expect(useAppStore.getState().selected?.node).toBe(subject);
    expect(useAppStore.getState().selected?.source).toMatchObject({ text: 'new' });
    useAppStore.getState().clearSelection();
  });

  it('ignores an earlier source response when the same fact is selected from a newer snapshot', async () => {
    let reply: (value: unknown) => void = () => {};
    let calls = 0;
    mockIPC(() => {
      calls += 1;
      return calls === 1 ? new Promise((resolve) => { reply = resolve; }) : {
        text: 'new', window_start: 0, truncated: false,
      };
    });
    const previous: GraphNode = { id: 'same', label: 'BusinessRule', props: { prov: {
      tier: 'Deterministic', confidence_tier: 'Confirmed', extractor_id: 'test', content_hash: 'old',
      evidence: [{ repo: 'local/src_first', path: 'rules.ts', byte_start: 0, byte_end: 3, commit_sha: 'workdir' }],
    } } };
    const current = { ...previous, props: { ...previous.props, name: 'newer fact' } };
    const first = useAppStore.getState().select(previous);
    await useAppStore.getState().select(current);
    reply({ text: 'old', window_start: 0, truncated: false });
    await first;
    expect(useAppStore.getState().selected?.node).toBe(current);
    expect(useAppStore.getState().selected?.source).toMatchObject({ text: 'new' });
    useAppStore.getState().clearSelection();
  });
});
