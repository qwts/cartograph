import { useEffect } from 'react';
import { useAppStore } from './store';
import { StatusBadge } from './components/StatusBadge';
import { GraphStatsCard } from './components/GraphStatsCard';
import { JobsCard } from './components/JobsCard';
import { IngestCard } from './components/IngestCard';
import { EndpointsCard } from './components/EndpointsCard';
import { EvidencePanel } from './components/EvidencePanel';
import { TopologyCard } from './components/TopologyCard';

export default function App() {
  const {
    backend,
    version,
    stats,
    jobs,
    endpoints,
    topology,
    ingestBusy,
    ingestSummary,
    ingestError,
    selected,
    refresh,
    enqueueJob,
    ingest,
    select,
    clearSelection,
  } = useAppStore();

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return (
    <>
      <header className="topbar">
        <h1>Cartograph</h1>
        <StatusBadge backend={backend} version={version} />
      </header>
      <main>
        <IngestCard
          busy={ingestBusy}
          summary={ingestSummary}
          error={ingestError}
          canIngest={backend === 'up'}
          onIngest={(path) => void ingest(path)}
        />
        <GraphStatsCard stats={stats} />
        <EndpointsCard endpoints={endpoints} onSelect={(node) => void select(node)} />
        <TopologyCard mermaid={topology} />
        <JobsCard
          jobs={jobs}
          canEnqueue={backend === 'up'}
          onEnqueue={(kind) => void enqueueJob(kind)}
        />
      </main>
      {selected && (
        <div className="evidence-area">
          <EvidencePanel node={selected.node} source={selected.source} onClose={clearSelection} />
        </div>
      )}
    </>
  );
}
