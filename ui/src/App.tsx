import { lazy, Suspense, useEffect } from 'react';
import { useAppStore } from './store';
import { StatusBadge } from './components/StatusBadge';
import { GraphStatsCard } from './components/GraphStatsCard';
import { JobsCard } from './components/JobsCard';
import { IngestCard } from './components/IngestCard';
import { EndpointsCard } from './components/EndpointsCard';
import { EvidencePanel } from './components/EvidencePanel';
import { TopologyCard } from './components/TopologyCard';
import type { SpecArtifact, SpecBundle } from './store';

const AtlasCanvas = lazy(() =>
  import('./components/AtlasCanvas').then(({ AtlasCanvas: Component }) => ({
    default: Component,
  })),
);
const FlowsCard = lazy(() =>
  import('./components/FlowsCard').then(({ FlowsCard: Component }) => ({
    default: Component,
  })),
);
const SpecWorkbench = lazy(() =>
  import('./components/SpecWorkbench').then(({ SpecWorkbench: Component }) => ({
    default: Component,
  })),
);

function exportSpecBundle(bundle: SpecBundle) {
  const files = Object.fromEntries(
    bundle.artifacts.map((artifact) => [artifact.file_name, artifact.content]),
  );
  const blob = new Blob([JSON.stringify({ ...bundle, files }, null, 2)], {
    type: 'application/json',
  });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = `cartograph-spec-${bundle.mode}.json`;
  anchor.click();
  URL.revokeObjectURL(url);
}

function copySpecArtifact(artifact: SpecArtifact) {
  void navigator.clipboard?.writeText(artifact.content);
}

export default function App() {
  const {
    backend,
    version,
    stats,
    jobs,
    endpoints,
    atlas,
    topology,
    flows,
    flowList,
    specBundle,
    specMode,
    curation,
    specBusy,
    specError,
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
    setSpecMode,
    curateAssertion,
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
          <Suspense fallback={<section className="atlas-card">Loading Atlas graph…</section>}>
            <AtlasCanvas snapshot={atlas} onSelect={(node) => void select(node)} />
          </Suspense>
          <EndpointsCard endpoints={endpoints} onSelect={(node) => void select(node)} />
          <TopologyCard mermaid={topology} />
          <Suspense fallback={<section className="card flow-inspector-card">Loading Flow Inspector…</section>}>
            <FlowsCard flows={flowList} dossier={flows} />
          </Suspense>
          <Suspense fallback={<section className="card spec-workbench-card">Loading Spec Workbench…</section>}>
            <SpecWorkbench
              bundle={specBundle}
              mode={specMode}
              decisions={curation}
              busy={specBusy}
              error={specError}
              canCurate={backend === 'up'}
              onModeChange={(mode) => void setSpecMode(mode)}
              onCurate={(assertion, decision, note) => void curateAssertion(assertion, decision, note)}
              onCopyArtifact={copySpecArtifact}
              onExportBundle={exportSpecBundle}
            />
          </Suspense>
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
