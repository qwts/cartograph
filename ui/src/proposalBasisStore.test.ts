import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { useAppStore, type BasisAssessment, type StagedProposal } from './store';
import { mixedTaskBasis } from './taskBasisFixtures';

const proposal = (id: string): StagedProposal => ({
  schema_version: 1, proposal_id: id, gap_id: 'same-gap', source_id: 'source', target_id: 'target',
  edge_label: 'CALLS', annotation: 'Saved rationale.', basis_hash: 'b'.repeat(64),
  evidence_binding: 'working_tree_unverified', context_status: 'awaiting_reconciliation',
  review_revision: 0, review_decision: null, review_note: null, created_at: 'original', reviewed_at: null,
  provenance: { tier: 'Agentic', confidence_tier: 'InferredWeak', evidence: [], extractor_id: 'broker', content_hash: 'c'.repeat(64) },
});

const assessment = (id: string, graph: 'changed' | 'unchanged' = 'unchanged'): BasisAssessment => ({
  schema_version: 1, proposal_id: id, current_graph_snapshot_id: `snapshot:${graph}`,
  graph_comparison: graph, association_comparison: 'unchanged', evidence: [],
  unverified_evidence: 0, captured_validation_bytes: 0, max_captured_validation_bytes: 134217728,
});

function deferred<T>() {
  let resolve: (value: T) => void = () => {};
  let reject: (error: Error) => void = () => {};
  const promise = new Promise<T>((success, failure) => { resolve = success; reject = failure; });
  return { promise, resolve, reject };
}

beforeEach(() => {
  vi.stubGlobal('window', {});
  useAppStore.setState({ escalation: null, stagedProposals: [], stagedError: null });
});

afterEach(() => {
  clearMocks();
  vi.unstubAllGlobals();
  useAppStore.setState({ escalation: null, stagedProposals: [], stagedError: null });
});

describe('selected proposal assessment identity (AC-0179)', () => {
  it('ignores a late response after selecting another proposal of the same gap', async () => {
    const first = deferred<BasisAssessment>();
    const requests: unknown[] = [];
    mockIPC((command, args) => {
      expect(command).toBe('assess_staged_basis');
      requests.push(args);
      return requests.length === 1 ? first.promise : assessment('second', 'changed');
    });
    const history = [proposal('first'), proposal('second')];
    const immutable = JSON.stringify(history);
    useAppStore.setState({ stagedProposals: history });
    useAppStore.getState().openStagedProposal(history[0]);
    const earlier = useAppStore.getState().assessStagedBasis();
    useAppStore.getState().openStagedProposal(history[1]);
    await useAppStore.getState().assessStagedBasis();
    first.resolve(assessment('first'));
    await earlier;
    expect(requests).toEqual([{ proposalId: 'first' }, { proposalId: 'second' }]);
    expect(useAppStore.getState().escalation?.basisAssessment?.result).toEqual(assessment('second', 'changed'));
    expect(JSON.stringify(useAppStore.getState().stagedProposals)).toBe(immutable);
    expect(useAppStore.getState().escalation?.proposal).toBe(history[1]);
  });

  it('ignores an older failure after a newer check of the same proposal succeeds', async () => {
    const first = deferred<BasisAssessment>();
    let calls = 0;
    mockIPC(() => ++calls === 1 ? first.promise : assessment('same', 'changed'));
    useAppStore.getState().openStagedProposal(proposal('same'));
    const earlier = useAppStore.getState().assessStagedBasis();
    const firstGeneration = useAppStore.getState().escalation?.basisAssessment?.requestGeneration;
    await useAppStore.getState().assessStagedBasis();
    expect(useAppStore.getState().escalation?.basisAssessment?.requestGeneration).not.toBe(firstGeneration);
    first.reject(new Error('old failure with private details'));
    await earlier;
    expect(useAppStore.getState().escalation?.basisAssessment?.result?.graph_comparison).toBe('changed');
    expect(useAppStore.getState().escalation?.basisAssessment?.error).toBeNull();
  });

  it('does not restore an old success after the latest check fails', async () => {
    const first = deferred<BasisAssessment>();
    let calls = 0;
    mockIPC(() => {
      if (++calls === 1) return first.promise;
      throw new Error('private path must not reach assessment diagnostics');
    });
    useAppStore.getState().openStagedProposal(proposal('same'));
    const earlier = useAppStore.getState().assessStagedBasis();
    await useAppStore.getState().assessStagedBasis();
    first.resolve(assessment('same'));
    await earlier;
    expect(useAppStore.getState().escalation?.basisAssessment?.result).toBeNull();
    expect(useAppStore.getState().escalation?.basisAssessment?.error).toBe(
      'Current basis could not be assessed. No proposal or review was changed.',
    );
  });

  it('clears observations on reopen and discards an earlier same-ID response', async () => {
    const first = deferred<BasisAssessment>();
    mockIPC(() => first.promise);
    const saved = proposal('same');
    useAppStore.getState().openStagedProposal(saved);
    const earlier = useAppStore.getState().assessStagedBasis();
    useAppStore.getState().closeResolution();
    useAppStore.getState().openStagedProposal(saved);
    first.resolve(assessment('same'));
    await earlier;
    expect(useAppStore.getState().escalation?.basisAssessment).toBeUndefined();
  });

  it('clears a prior observation while a refresh is still pending', async () => {
    const next = deferred<BasisAssessment>();
    let calls = 0;
    mockIPC(() => ++calls === 1 ? assessment('same') : next.promise);
    useAppStore.getState().openStagedProposal(proposal('same'));
    await useAppStore.getState().assessStagedBasis();
    const refreshing = useAppStore.getState().assessStagedBasis();
    expect(useAppStore.getState().escalation?.basisAssessment).toMatchObject({ loading: true, result: null, error: null });
    next.resolve(assessment('same', 'changed'));
    await refreshing;
    expect(useAppStore.getState().escalation?.basisAssessment?.result?.graph_comparison).toBe('changed');
  });

  it('rejects a wrong-proposal response without changing a saved review', async () => {
    const saved = { ...proposal('same'), review_revision: 1, review_decision: 'accepted' as const };
    useAppStore.setState({ stagedProposals: [saved] });
    useAppStore.getState().openStagedProposal(saved);
    mockIPC(() => assessment('another'));
    await useAppStore.getState().assessStagedBasis();
    expect(useAppStore.getState().escalation?.basisAssessment?.result).toBeNull();
    expect(useAppStore.getState().escalation?.basisAssessment?.error).toContain('could not be assessed');
    expect(useAppStore.getState().escalation?.proposal).toBe(saved);
    expect(useAppStore.getState().stagedProposals).toEqual([saved]);
    expect(useAppStore.getState().escalation?.decided).toBe('accepted');
  });

  it('does not roll back a review completed while an assessment was in flight', async () => {
    const reading = deferred<BasisAssessment>();
    const saved = proposal('same');
    const reviewed: StagedProposal = { ...saved, review_revision: 1, review_decision: 'accepted', reviewed_at: 'reviewed' };
    const commands: string[] = [];
    mockIPC((command) => {
      commands.push(command);
      return command === 'assess_staged_basis' ? reading.promise : reviewed;
    });
    useAppStore.setState({ stagedProposals: [saved] });
    useAppStore.getState().openStagedProposal(saved);
    const pending = useAppStore.getState().assessStagedBasis();
    await useAppStore.getState().decideProposal('accepted');
    reading.resolve(assessment('same', 'changed'));
    await pending;
    expect(commands).toEqual(['assess_staged_basis', 'record_agent_decision']);
    expect(useAppStore.getState().escalation?.proposal).toEqual(reviewed);
    expect(useAppStore.getState().escalation?.proposal?.context_status).toBe('awaiting_reconciliation');
    expect(useAppStore.getState().escalation?.basisAssessment?.result?.graph_comparison).toBe('changed');
    expect(useAppStore.getState().stagedProposals).toEqual([reviewed]);
  });
});

describe('versioned proposal history (AC-0179)', () => {
  it('keeps v1 and v2 metadata unchanged and never assesses a page implicitly', async () => {
    const legacy = proposal('legacy');
    const captured: StagedProposal = { ...proposal('captured'), schema_version: 2,
      evidence_binding: 'per_item', source_basis: mixedTaskBasis };
    const items = [captured, legacy];
    const original = JSON.stringify(items);
    const commands: string[] = [];
    mockIPC((command) => { commands.push(command); return { items, next_cursor: null }; });
    await useAppStore.getState().loadStagedProposals();
    useAppStore.getState().openStagedProposal(useAppStore.getState().stagedProposals[0]);
    expect(commands).toEqual(['list_staged_proposals']);
    expect(JSON.stringify(useAppStore.getState().stagedProposals)).toBe(original);
    expect(useAppStore.getState().stagedProposals[1].source_basis).toBeUndefined();
    expect(useAppStore.getState().escalation?.basisAssessment).toBeUndefined();
  });

  it('rejects incomplete or mismatched v2 rows without replacing loaded history', async () => {
    const legacy = proposal('legacy');
    useAppStore.setState({ stagedProposals: [legacy] });
    const malformed = [
      { ...proposal('new'), schema_version: 2, evidence_binding: 'per_item' },
      { ...proposal('new'), schema_version: 2, evidence_binding: 'per_item', source_basis: null },
      { ...proposal('new'), schema_version: 2, source_basis: mixedTaskBasis },
      { ...proposal('new'), source_basis: mixedTaskBasis },
      { ...proposal('new'), schema_version: 3 },
    ];
    for (const item of malformed) {
      mockIPC(() => ({ items: [item], next_cursor: null }));
      await useAppStore.getState().loadStagedProposals();
      expect(useAppStore.getState().stagedProposals).toEqual([legacy]);
      expect(useAppStore.getState().stagedError).toContain('did not return a saved proposal');
    }
  });
});
