import { create } from 'zustand';
import { invokeOr } from './tauri';

export interface GraphStats {
  nodes: number;
  edges: number;
}

export interface Job {
  id: number;
  kind: string;
  status: string;
  created_at: string;
  updated_at: string;
}

export interface AppStore {
  /** Backend liveness: unknown until the first ping resolves. */
  backend: 'unknown' | 'up' | 'browser';
  version: string | null;
  stats: GraphStats | null;
  jobs: Job[];
  refresh: () => Promise<void>;
  enqueueJob: (kind: string) => Promise<void>;
}

export const useAppStore = create<AppStore>((set, get) => ({
  backend: 'unknown',
  version: null,
  stats: null,
  jobs: [],

  refresh: async () => {
    const ping = await invokeOr<{ app: string; version: string } | null>('ping', null);
    if (ping === null) {
      set({ backend: 'browser', version: null, stats: null, jobs: [] });
      return;
    }
    const [stats, jobs] = await Promise.all([
      invokeOr<GraphStats>('graph_stats', { nodes: 0, edges: 0 }),
      invokeOr<Job[]>('list_jobs', []),
    ]);
    set({ backend: 'up', version: ping.version, stats, jobs });
  },

  enqueueJob: async (kind: string) => {
    await invokeOr<Job | null>('enqueue_job', null, { kind });
    await get().refresh();
  },
}));
