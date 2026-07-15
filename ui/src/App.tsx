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
import { StatusBadge } from './components/StatusBadge';
import { GraphStatsCard } from './components/GraphStatsCard';
import { JobsSurface } from './components/JobsSurface';
import { ConnectSurface } from './components/ConnectSurface';
import { PreflightSurface } from './components/PreflightSurface';
import { RecoverSurface } from './components/RecoverSurface';
import { SettingsSurface } from './components/SettingsSurface';
import { WorkspaceSurface } from './components/WorkspaceSurface';
import { GapsDriftSurface } from './components/GapsDriftSurface';
import { ProvenanceSurface } from './components/ProvenanceSurface';
import { ResolutionStrategyModal } from './components/ResolutionStrategyModal';
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
    registerFindings,
    ingestHistory,
    coverage,
    evals,
    tierSettings,
    egress,
    disclosures,
    settingsError,
    selected,
    refresh,
    clearFinishedJobs,
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
    escalation,
    openResolution,
    closeResolution,
    runStrategy,
    consentAndRun,
    dismissPreview,
    decideProposal,
  } = useAppStore();

  const [paletteOpen, setPaletteOpen] = useState(false);
  const [legendOpen, setLegendOpen] = useState(false);
  const [atlasLayer, setAtlasLayer] = useState('All layers');

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
      // AtlasCanvas remounts with its filter reset to All layers, so the
      // scope chip must forget the layer too or it lies on return (#143).
      if (next !== 'atlas') setAtlasLayer('All layers');
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
    : view === 'atlas' && atlasLayer !== 'All layers'
      ? { kind: 'layer', label: `Atlas · ${atlasLayer}` }
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
            <AtlasCanvas
              snapshot={atlas}
              onSelect={(node) => void select(node)}
              onSelectEdge={(edge) =>
                // Edges are evidence subjects too: the drawer reads the
                // edge's own provenance via a synthetic subject.
                void select({
                  id: `${edge.src} ${edge.label} ${edge.dst}`,
                  label: edge.label,
                  props: edge.props,
                })
              }
              onLayerChange={setAtlasLayer}
            />
          </Suspense>
        );
      case 'flows':
        return (
          <Suspense
            fallback={<section className="card flow-inspector-card">Loading Flow Inspector…</section>}
          >
            <FlowsCard
              flows={flowList}
              dossier={flows}
              onSelectHop={(hop) =>
                // Hops are evidence subjects like edges: the drawer reads the
                // hop's own provenance via a synthetic subject.
                void select({
                  id: `${hop.src} ${hop.label} ${hop.dst}`,
                  label: hop.label,
                  props: {
                    name: `${hop.src_name} ${hop.label} ${hop.dst_name}`,
                    prov: hop.provenance,
                  },
                })
              }
              onOpenResolution={(gapId) => void openResolution(gapId)}
            />
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
      case 'gaps': {
        const registerArtifact = (file: string) =>
          specBundle?.artifacts.find((artifact) => artifact.file_name === file)?.assertions ??
          [];
        return (
          <GapsDriftSurface
            summary={findings}
            gaps={registerArtifact('gap_register.md')}
            drift={registerArtifact('drift_register.md')}
            registerFindings={registerFindings}
            onOpenGap={(assertion) => {
              // A gap node opens its Resolution Strategy (#120 runner); an
              // edge/flow gap has no node to escalate, so its row opens the
              // evidence drawer from the assertion's own provenance.
              if (assertion.id.startsWith('node:')) {
                void openResolution(assertion.subject_id);
                return;
              }
              const node = atlas.nodes.find(
                (candidate) => candidate.id === assertion.subject_id,
              ) ?? {
                id: assertion.subject_id,
                label: assertion.subject_kind,
                props: { name: assertion.summary, prov: assertion.provenance },
              };
              void select(node);
            }}
          />
        );
      }
      case 'prov':
        return (
          <ProvenanceSurface
            findings={findings}
            distribution={tierDistribution(atlas)}
            coverage={coverage}
            evals={evals}
            history={ingestHistory}
          />
        );
      case 'jobs':
        return (
          <JobsSurface
            jobs={jobs}
            canClear={backend === 'up'}
            onClearFinished={() => void clearFinishedJobs()}
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
          <EvidencePanel
            node={selected.node}
            source={selected.source}
            evidenceIndex={selected.evidenceIndex}
            onClose={clearSelection}
            onShowEvidence={(index) => void select(selected.node, index)}
            onOpenResolution={
              // Only a real Gap node has strategies to derive; a synthetic
              // edge/flow subject would ask the backend for a node it does
              // not have — the CTA stays disabled for those (#142 review).
              atlas.nodes.some((candidate) => candidate.id === selected.node.id)
                ? (node) => void openResolution(node.id)
                : undefined
            }
          />
        </div>
      )}
      {escalation && (
        <ResolutionStrategyModal
          state={escalation}
          onRun={(strategyId) => void runStrategy(strategyId)}
          onConsent={(preview) => void consentAndRun(preview)}
          onDismissPreview={dismissPreview}
          onDecide={(decision) => void decideProposal(decision)}
          onClose={closeResolution}
        />
      )}
    </div>
  );
}
