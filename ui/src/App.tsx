import { lazy, Suspense, useCallback, useEffect, useState } from 'react';
import { tierDistribution, useAppStore } from './store';
import { railSurface, SURFACES, surfaceLabel, type SurfaceView } from './views';
import { NavRail } from './components/NavRail';
import { ShellHeader, type Scope } from './components/ShellHeader';
import { StatusBar } from './components/StatusBar';
import { GlobalProgress } from './components/GlobalProgress';
import { CommandPalette } from './components/CommandPalette';
import { LegendPopover } from './components/LegendPopover';
import { RouteErrorBoundary } from './components/RouteErrorBoundary';
import { EmptySurface } from './components/EmptySurface';
import { StatusBadge } from './components/StatusBadge';
import { GraphStatsCard } from './components/GraphStatsCard';
import { JobsSurface } from './components/JobsSurface';
import { ConnectSurface } from './components/ConnectSurface';
import { PreflightSurface } from './components/PreflightSurface';
import { RecoverSurface } from './components/RecoverSurface';
import { SettingsSurface } from './components/SettingsSurface';
import { WorkspaceSurface } from './components/WorkspaceSurface';
import { EndpointsCard } from './components/EndpointsCard';
import { EvidencePanel } from './components/EvidencePanel';
import { TopologyCard } from './components/TopologyCard';
import type { Job, SpecArtifact, SpecBundle } from './store';

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
    view,
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
    ingestSource,
    ingestTarget,
    preflight,
    preflightBusy,
    preflightError,
    clearBusy,
    clearError,
    findings,
    tierSettings,
    egress,
    disclosures,
    settingsError,
    selected,
    refresh,
    enqueueJob,
    clearGraph,
    setSpecMode,
    curateAssertion,
    select,
    clearSelection,
    setView,
    cancelJob,
    retryJob,
    applyJobEvent,
    setIngestSource,
    setIngestTarget,
    runPreflight,
    startRecovery,
    setTierEnabled,
    setTierProvider,
    grantCloudConsent,
    revokeCloudConsent,
  } = useAppStore();

  const [paletteOpen, setPaletteOpen] = useState(false);
  const [legendOpen, setLegendOpen] = useState(false);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Live job transitions (#117): the core pushes every change, so progress
  // and the global bar stay current without polling. Outside Tauri (browser
  // preview, stories) there is no event bridge — refresh() still covers it.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        const stop = await listen<Job>('job://changed', (event) => applyJobEvent(event.payload));
        if (disposed) stop();
        else unlisten = stop;
      } catch {
        // No event bridge available.
      }
    })();
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [applyJobEvent]);

  const navigate = useCallback(
    (next: SurfaceView) => {
      setView(next);
      setPaletteOpen(false);
    },
    [setView],
  );

  // ⌘K / Ctrl+K toggles the palette; ⌘1–⌘8 jump straight to a surface.
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey)) return;
      if (event.key.toLowerCase() === 'k') {
        event.preventDefault();
        setPaletteOpen((open) => !open);
      } else if (event.key >= '1' && event.key <= '8') {
        event.preventDefault();
        navigate(SURFACES[Number(event.key) - 1].id);
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [navigate]);

  const busy =
    ingestBusy || specBusy || clearBusy || jobs.some((job) => job.status === 'running');
  const scope: Scope = selected
    ? { kind: 'trail', label: 'Single evidence trail' }
    : { kind: 'system', label: 'Whole system' };
  const systemName = ingestSummary ? 'Ingested system' : null;
  const status = ingestBusy
    ? 'Ingesting…'
    : backend === 'up'
      ? 'Ready'
      : backend === 'browser'
        ? 'No core (browser preview)'
        : 'Connecting…';
  // Without a core nothing can egress, so the local-only line stays honest.
  const egressLine = egress?.label ?? 'Local-only · 0 bytes egress';

  const surface = (() => {
    switch (view) {
      case 'workspace':
        return (
          <>
            <WorkspaceSurface
              summary={ingestSummary}
              findings={findings}
              distribution={tierDistribution(atlas)}
              bundle={specBundle}
              onReingest={() => navigate('connect')}
              onTriageGaps={() => navigate('gaps')}
              onProvenance={() => navigate('prov')}
              onOpenArtifact={() => navigate('spec')}
            />
            <div className="card-grid utility">
              <GraphStatsCard
                stats={stats}
                canClear={backend === 'up' && !ingestBusy && (stats?.nodes ?? 0) > 0}
                clearing={clearBusy}
                error={clearError}
                onClear={() => void clearGraph()}
              />
            </div>
            <div className="card-grid artifacts">
              <EndpointsCard endpoints={endpoints} onSelect={(node) => void select(node)} />
              <TopologyCard mermaid={topology} />
            </div>
          </>
        );
      case 'atlas':
        return (
          <Suspense fallback={<section className="atlas-card">Loading Atlas graph…</section>}>
            <AtlasCanvas snapshot={atlas} onSelect={(node) => void select(node)} />
          </Suspense>
        );
      case 'flows':
        return (
          <Suspense
            fallback={<section className="card flow-inspector-card">Loading Flow Inspector…</section>}
          >
            <FlowsCard flows={flowList} dossier={flows} />
          </Suspense>
        );
      case 'spec':
        return (
          <Suspense
            fallback={<section className="card spec-workbench-card">Loading Spec Workbench…</section>}
          >
            <SpecWorkbench
              bundle={specBundle}
              mode={specMode}
              decisions={curation}
              busy={specBusy}
              error={specError}
              canCurate={backend === 'up'}
              onModeChange={(mode) => void setSpecMode(mode)}
              onCurate={(assertion, decision, note) =>
                void curateAssertion(assertion, decision, note)
              }
              onCopyArtifact={copySpecArtifact}
              onExportBundle={exportSpecBundle}
            />
          </Suspense>
        );
      case 'gaps':
        return (
          <EmptySurface
            icon="report"
            title="Gaps & Drift"
            description="The open-findings register — system gaps, unsupported patterns, no-evidence findings, and ADR drift — lands with the register surface (#109) once the core's finding model (#116) is in place."
          />
        );
      case 'prov':
        return (
          <EmptySurface
            icon="analytics"
            title="Provenance & Eval"
            description="Tier distribution, extractor coverage, paired-eval gates, and evidence health over re-ingests land with the provenance surface (#110) on top of the recovery metrics (#119)."
          />
        );
      case 'jobs':
        return (
          <JobsSurface
            jobs={jobs}
            canEnqueue={backend === 'up'}
            onEnqueue={(kind) => void enqueueJob(kind)}
            onCancel={(id) => void cancelJob(id)}
            onRetry={(id) => void retryJob(id)}
          />
        );
      case 'connect':
        return (
          <ConnectSurface
            source={ingestSource}
            target={ingestTarget}
            canPreflight={backend === 'up'}
            onSourceChange={setIngestSource}
            onTargetChange={setIngestTarget}
            onBack={() => navigate('workspace')}
            onPreflight={() => void runPreflight()}
          />
        );
      case 'preflight':
        return (
          <PreflightSurface
            source={ingestSource}
            target={ingestTarget}
            report={preflight}
            busy={preflightBusy}
            error={preflightError}
            canRecover={backend === 'up'}
            onBack={() => navigate('connect')}
            onRunRecovery={() => void startRecovery()}
          />
        );
      case 'recover':
        return (
          <RecoverSurface
            job={jobs.find((job) => job.status === 'running' || job.status === 'queued') ?? null}
            busy={ingestBusy}
            error={ingestError}
            onBack={() => navigate('connect')}
            onBackground={() => navigate('jobs')}
          />
        );
      case 'settings':
        return (
          <SettingsSurface
            tiers={tierSettings}
            egressLabel={egressLine}
            disclosures={disclosures}
            error={settingsError}
            canEdit={backend === 'up'}
            onToggleTier={(tier, enabled) => void setTierEnabled(tier, enabled)}
            onProviderChange={(tier, provider) => void setTierProvider(tier, provider)}
            onGrantConsent={(tier) => void grantCloudConsent(tier)}
            onRevokeConsent={(tier) => void revokeCloudConsent(tier)}
          />
        );
    }
  })();

  return (
    <div className="shell">
      <GlobalProgress active={busy} />
      <NavRail
        active={railSurface(view)}
        onNavigate={navigate}
        onOpenPalette={() => setPaletteOpen(true)}
      />
      <div className="shell-main">
        <ShellHeader
          system={systemName}
          surface={surfaceLabel(view)}
          scope={scope}
          onShowLegend={() => setLegendOpen(true)}
        />
        <main className="shell-content">
          <RouteErrorBoundary view={view}>{surface}</RouteErrorBoundary>
        </main>
        <StatusBar status={status} busy={busy} egress={egressLine}>
          <StatusBadge backend={backend} version={version} />
        </StatusBar>
      </div>
      <CommandPalette
        open={paletteOpen}
        onClose={() => setPaletteOpen(false)}
        onNavigate={navigate}
      />
      <LegendPopover open={legendOpen} onClose={() => setLegendOpen(false)} />
      {selected && (
        <div className="evidence-area">
          <EvidencePanel node={selected.node} source={selected.source} onClose={clearSelection} />
        </div>
      )}
    </div>
  );
}
