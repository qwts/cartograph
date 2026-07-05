import { useEffect } from 'react';
import { useAppStore } from './store';

export default function App() {
  const { backend, version, stats, jobs, refresh, enqueueJob } = useAppStore();

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return (
    <>
      <header className="topbar">
        <h1>Cartograph</h1>
        {backend === 'up' && <span className="badge up">core v{version}</span>}
        {backend === 'browser' && <span className="badge browser">browser preview — no core</span>}
        {backend === 'unknown' && <span className="badge">connecting…</span>}
      </header>
      <main>
        <section className="card">
          <h2>Unified graph</h2>
          <div className="stat">{stats ? stats.nodes : '—'}</div>
          <div className="stat-label">nodes</div>
          <div className="stat">{stats ? stats.edges : '—'}</div>
          <div className="stat-label">edges</div>
        </section>
        <section className="card">
          <h2>Jobs</h2>
          {jobs.length === 0 ? (
            <p className="muted">No jobs yet. The job spine is durable — restart the app and this list survives.</p>
          ) : (
            <ul className="jobs-list">
              {jobs.map((job) => (
                <li key={job.id}>
                  <span>
                    #{job.id} {job.kind}
                  </span>
                  <span className="muted">{job.status}</span>
                </li>
              ))}
            </ul>
          )}
          <p style={{ marginTop: '0.75rem' }}>
            <button onClick={() => void enqueueJob('noop')} disabled={backend !== 'up'}>
              Enqueue test job
            </button>
          </p>
        </section>
      </main>
    </>
  );
}
