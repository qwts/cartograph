import { useEffect } from 'react';
import { useAppStore } from './store';
import { StatusBadge } from './components/StatusBadge';
import { GraphStatsCard } from './components/GraphStatsCard';
import { JobsCard } from './components/JobsCard';

export default function App() {
  const { backend, version, stats, jobs, refresh, enqueueJob } = useAppStore();

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
        <GraphStatsCard stats={stats} />
        <JobsCard jobs={jobs} canEnqueue={backend === 'up'} onEnqueue={(kind) => void enqueueJob(kind)} />
      </main>
    </>
  );
}
