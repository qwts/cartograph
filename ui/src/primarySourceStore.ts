import { create } from 'zustand';
import { invokeOr } from './tauri';
import type { GraphNode } from './store';

export type FactKey = { kind: 'node'; id: string } |
  { kind: 'edge'; source: string; label: string; destination: string };

export function sameFactKey(left: FactKey | null, right: FactKey): boolean {
  return left?.kind === 'node' && right.kind === 'node' ? left.id === right.id :
    left?.kind === 'edge' && right.kind === 'edge' && left.source === right.source &&
      left.label === right.label && left.destination === right.destination;
}

export interface CapturedDescription {
  fact: FactKey;
  receipt_id: string;
  emitted_fact_digest: string;
  source_id: string;
  repo_key: string;
  ranges: { index: number; path: string; byte_start: number; byte_end: number }[];
  scope: 'primary_source_only';
  input_closure: 'input_closure_not_established';
}

export interface CapturedText {
  receipt_id: string;
  range_index: number;
  text: string;
  path: string;
  byte_start: number;
  byte_end: number;
}

export interface RetainedSource {
  source_id: string;
  repo_key: string;
  display_name: string;
}

export interface RetentionPreview extends RetainedSource {
  captures: number;
  files: number;
  bytes: number;
  receipts: number;
  current_references: number;
  historical_references: number;
  fingerprint: string;
}

interface PrimarySourceState {
  selection: number;
  subject: GraphNode | null;
  subjectFact: FactKey | null;
  description: CapturedDescription | null;
  text: CapturedText | null;
  rangeIndex: number | null;
  loading: boolean;
  error: string | null;
  inspect: (node: GraphNode, fact?: FactKey) => Promise<void>;
  readRange: (index: number) => Promise<void>;
  clear: () => void;
  sources: RetainedSource[];
  preview: RetentionPreview | null;
  retentionSelection: number;
  retentionBusy: boolean;
  retentionError: string | null;
  retentionMessage: string | null;
  loadSources: () => Promise<void>;
  previewSource: (sourceId: string) => Promise<void>;
  dismissPreview: () => void;
  forget: () => Promise<void>;
}

const INVENTORY_ERROR = 'Retained-source inventory is unavailable.';
let inventoryRequest = 0;

export const usePrimarySourceStore = create<PrimarySourceState>((set, get) => ({
  selection: 0, subject: null, subjectFact: null, description: null, text: null, rangeIndex: null, loading: false, error: null,
  sources: [], preview: null, retentionSelection: 0, retentionBusy: false,
  retentionError: null, retentionMessage: null,
  inspect: async (node, fact = { kind: 'node', id: node.id }) => {
    const selection = get().selection + 1;
    set({ selection, subject: node, subjectFact: fact, description: null, text: null, rangeIndex: null, loading: true, error: null });
    try {
      const description = await invokeOr<CapturedDescription | null>('describe_captured_source', null, {
        fact,
        expectedNode: fact.kind === 'node' ? node : null,
        expectedEdge: fact.kind === 'edge' ? {
          src: fact.source, dst: fact.destination, label: fact.label, props: node.props,
        } : null,
      });
      if (description && (!sameFactKey(description.fact, fact) || !description.receipt_id ||
          description.scope !== 'primary_source_only' || description.input_closure !== 'input_closure_not_established')) {
        throw new Error('Captured description does not match the selected fact.');
      }
      if (get().selection === selection) set({ description: description ?? null, loading: false });
    } catch {
      if (get().selection === selection) set({ loading: false, error: 'No current retained primary-source binding is available for this selected fact.' });
    }
  },
  readRange: async (index) => {
    const { selection, description } = get();
    if (!description || !description.ranges.some((range) => range.index === index)) return;
    set({ rangeIndex: index, text: null, loading: true, error: null });
    try {
      const text = await invokeOr<CapturedText | null>('read_captured_source', null, {
        fact: description.fact, receiptId: description.receipt_id, rangeIndex: index,
      });
      const current = get();
      if (current.selection !== selection || current.rangeIndex !== index ||
          current.description?.receipt_id !== description.receipt_id) return;
      if (!text || text.receipt_id !== description.receipt_id || text.range_index !== index) {
        set({ text: null, loading: false, error: 'Captured source is unavailable. Select the fact again if recovery changed.' });
      } else set({ text, loading: false });
    } catch {
      const current = get();
      if (current.selection === selection && current.rangeIndex === index) {
        set({ text: null, loading: false, error: 'Captured source is unavailable or stale. No working-tree source was substituted.' });
      }
    }
  },
  clear: () => set((state) => ({ selection: state.selection + 1, subject: null, subjectFact: null, description: null, text: null, rangeIndex: null, loading: false, error: null })),
  loadSources: async () => {
    const request = ++inventoryRequest;
    if (get().retentionError === INVENTORY_ERROR) set({ retentionError: null });
    try {
      const sources = await invokeOr<RetainedSource[]>('list_retained_sources', []);
      if (request !== inventoryRequest) return;
      set((state) => ({ sources, retentionError: state.retentionError === INVENTORY_ERROR ? null : state.retentionError }));
    } catch {
      if (request === inventoryRequest) set({ retentionError: INVENTORY_ERROR });
    }
  },
  previewSource: async (sourceId) => {
    const retentionSelection = get().retentionSelection + 1;
    set({ retentionSelection, preview: null, retentionBusy: true, retentionError: null, retentionMessage: null });
    try {
      const preview = await invokeOr<RetentionPreview | null>('preview_forget_source', null, { sourceId });
      if (get().retentionSelection !== retentionSelection) return;
      if (!preview || preview.source_id !== sourceId) throw new Error('unavailable');
      set({ preview, retentionBusy: false });
    } catch {
      if (get().retentionSelection === retentionSelection) set({ retentionBusy: false, retentionError: 'Could not obtain a current retention preview. Nothing was forgotten.' });
    }
  },
  dismissPreview: () => set((state) => ({ retentionSelection: state.retentionSelection + 1, preview: null, retentionBusy: false })),
  forget: async () => {
    const { preview, retentionSelection } = get();
    if (!preview || get().retentionBusy) return;
    set({ retentionBusy: true, retentionError: null });
    try {
      const removed = await invokeOr<number | null>('forget_retained_source', null, {
        sourceId: preview.source_id, fingerprint: preview.fingerprint,
      });
      if (removed === null) throw new Error('unavailable');
      // Never keep copied source visible after the user has forgotten its source.
      if (get().description?.source_id === preview.source_id) get().clear();
      if (get().retentionSelection === retentionSelection) {
        set({ preview: null, retentionBusy: false, retentionMessage: `Forgot ${removed} retained captures for ${preview.display_name}. Receipt and review history is preserved.` });
      }
    } catch {
      if (get().retentionSelection === retentionSelection) set({ preview: null, retentionBusy: false, retentionError: 'Source changed, is busy, or could not be forgotten. Refresh the preview before trying again.' });
    }
  },
}));
