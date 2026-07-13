import { useEffect } from 'react';
import { useAppStore } from './store';
import { StatusBadge } from './components/StatusBadge';
import { GraphStatsCard } from './components/GraphStatsCard';
import { JobsCard } from './components/JobsCard';
import { IngestCard } from './components/IngestCard';
import { EndpointsCard } from './components/EndpointsCard';
import { EvidencePanel } from './components/EvidencePanel';
import { TopologyCard } from './components/TopologyCard';
import { FlowsCard } from './components/FlowsCard';

export default function App() {
  const {
    backend,
    version,
    stats,
    jobs,
    endpoints,
    topology,
    flows,
    flowList,
    ingestBusy,
    ingestSummary,
    ingestError,
    clearBusy,
    clearError,
    selected,
    refresh,
    enqueueJob,
    ingest,
    clearGraph,
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
        {/* Compact utility strip: drive the engine, watch its vitals. */}
        <div className="card-grid utility">
          <IngestCard
            busy={ingestBusy}
            summary={ingestSummary}
            error={ingestError}
            canIngest={backend === 'up'}
            onIngest={(path) => void ingest(path)}
          />
          <GraphStatsCard
            stats={stats}
            canClear={backend === 'up' && !ingestBusy && (stats?.nodes ?? 0) > 0}
            clearing={clearBusy}
            error={clearError}
            onClear={() => void clearGraph()}
          />
          <JobsCard
            jobs={jobs}
            canEnqueue={backend === 'up'}
            onEnqueue={(kind) => void enqueueJob(kind)}
          />
        </div>
        {/* Recovered spec: content-heavy cards get wide tracks. */}
        <div className="card-grid artifacts">
          <EndpointsCard endpoints={endpoints} onSelect={(node) => void select(node)} />
          <TopologyCard mermaid={topology} />
          <FlowsCard flows={flowList} dossier={flows} />
        </div>
      </main>
      {selected && (
        <div className="evidence-area">
          <EvidencePanel node={selected.node} source={selected.source} onClose={clearSelection} />
        </div>
      )}
    </>
  );
}
