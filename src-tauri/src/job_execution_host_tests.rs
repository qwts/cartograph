//! Actual host ownership, blocking-worker and completed-staging boundaries.

use crate::*;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

pub(super) fn locks(state_path: &Path) -> JobExecutionLocks {
    let jobs = JobStore::open(state_path).unwrap();
    JobExecutionLocks::open(state_path.parent().unwrap(), jobs.execution_namespace()).unwrap()
}

pub(super) fn claim(
    jobs: &mut JobStore,
    locks: &JobExecutionLocks,
    id: i64,
    mode: ClaimMode,
) -> JobExecution {
    let plan = jobs.claim_plan(id, mode).unwrap();
    let reservation = locks.try_reserve(plan.lock_target()).unwrap();
    jobs.claim_execution(&plan, reservation).unwrap().1
}

struct WorkerReturned(mpsc::Sender<()>);

impl Drop for WorkerReturned {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

struct ReleaseWorker {
    release: Option<mpsc::Sender<()>>,
    returned: mpsc::Receiver<()>,
}

impl ReleaseWorker {
    fn finish(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
            self.returned.recv_timeout(Duration::from_secs(30)).unwrap();
        }
    }
}

impl Drop for ReleaseWorker {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
            let _ = self.returned.recv_timeout(Duration::from_secs(30));
        }
    }
}

#[test]
fn detached_job_worker_retains_ownership_after_waiter_abort() {
    // AC-0161: the real production blocking helper retains execution after its
    // waiter is aborted. Channels establish order; timeouts only bound failure.
    let dir = tempfile::tempdir().unwrap();
    let state = registered_source_tests::app_state(dir.path());
    let (job, execution) = start_job(&state, "noop").unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (returned_tx, returned_rx) = mpsc::channel();
    let mut cleanup = ReleaseWorker {
        release: Some(release_tx),
        returned: returned_rx,
    };
    let waiter = tauri::async_runtime::spawn(async move {
        off_ui_thread_for_job(execution, move |_execution| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            // The abandoned runtime output is dropped only after the blocking
            // helper has returned and its retained execution has been dropped.
            Ok(WorkerReturned(returned_tx))
        })
        .await
    });
    started_rx.recv_timeout(Duration::from_secs(30)).unwrap();
    waiter.abort();
    assert!(tauri::async_runtime::block_on(waiter).is_err());

    let reopened = registered_source_tests::app_state(dir.path());
    assert!(
        recover_jobs(&reopened.jobs, &reopened.job_execution_locks)
            .unwrap()
            .is_empty()
    );
    reopened.jobs.lock().unwrap().cancel(job.id).unwrap();
    let cancelled =
        serde_json::to_value(reopened.jobs.lock().unwrap().get(job.id).unwrap()).unwrap();
    assert!(prepare_job_retry(&reopened, job.id).is_err());
    assert_eq!(
        serde_json::to_value(reopened.jobs.lock().unwrap().get(job.id).unwrap()).unwrap(),
        cancelled
    );
    cleanup.finish();
    let (retried, execution, _, _) = prepare_job_retry(&reopened, job.id).unwrap();
    assert_eq!(retried.status, "running");
    assert_eq!(retried.id, job.id);
    let done = updated_job(
        reopened
            .jobs
            .lock()
            .unwrap()
            .finish_execution(&execution, &[])
            .unwrap(),
    );
    assert_eq!(done.status, "done");
}

#[test]
fn completed_job_staging_preserves_cancelled_results_and_history() {
    // AC-0161, AC-0163: the actual single/batch staging seams retain completed
    // proposals when cancellation wins, without reviving the job or rewriting
    // immutable accepted payloads through cleanup/restart.
    let dir = tempfile::tempdir().unwrap();
    let state = registered_source_tests::app_state(dir.path());
    let (task, proposal) = registered_source_tests::historical_proposal();
    let (job, execution) = start_job(&state, "escalate:fixture:local").unwrap();
    state.jobs.lock().unwrap().cancel(job.id).unwrap();
    let cancelled = serde_json::to_value(state.jobs.lock().unwrap().get(job.id).unwrap()).unwrap();
    let staged =
        stage_completed_job_proposal(&state, &execution, &task, &proposal, "snapshot:fixture")
            .unwrap();
    let done = updated_job(
        state
            .jobs
            .lock()
            .unwrap()
            .finish_execution(&execution, std::slice::from_ref(&staged.proposal_id))
            .unwrap(),
    );
    assert_eq!(serde_json::to_value(done).unwrap(), cancelled);
    let accepted = state
        .proposals
        .lock()
        .unwrap()
        .review(
            &staged.proposal_id,
            0,
            agents::ProposalDecision::Accepted,
            None,
        )
        .unwrap();
    let accepted = serde_json::to_value(accepted).unwrap();
    let immutable = registered_source_tests::staged_bytes(
        &dir.path().join("proposals.sqlite"),
        &staged.proposal_id,
    );

    let (batch, batch_execution) = start_job(&state, "escalate-class:2:local").unwrap();
    let mut calls = 0;
    let outcome = run_job_staged_batch(
        &state,
        &batch_execution,
        vec![
            ("first".into(), Ok(task.clone())),
            ("second".into(), Ok(task.clone())),
        ],
        |_, _| Ok(()),
        |task| {
            calls += 1;
            state.jobs.lock().unwrap().cancel(batch.id).unwrap();
            stage_completed_job_proposal(
                &state,
                &batch_execution,
                task,
                &proposal,
                "snapshot:fixture",
            )
        },
    )
    .unwrap();
    assert_eq!(calls, 1);
    assert!(outcome.cancelled);
    assert_eq!(outcome.proposals.len(), 1);
    assert!(outcome.failures.is_empty());
    assert_eq!(
        updated_job(
            state
                .jobs
                .lock()
                .unwrap()
                .finish_execution(&batch_execution, &[])
                .unwrap()
        )
        .status,
        "cancelled"
    );
    assert_eq!(state.jobs.lock().unwrap().clear_finished().unwrap(), 2);
    assert!(job_cancelled(&state, &execution).is_err());
    assert_eq!(
        stage_completed_job_proposal(&state, &execution, &task, &proposal, "snapshot:fixture")
            .unwrap()
            .proposal_id,
        staged.proposal_id
    );
    drop(execution);
    drop(batch_execution);
    drop(state);

    let state = registered_source_tests::app_state(dir.path());
    let history = state.proposals.lock().unwrap().list(20, None).unwrap();
    assert!(
        history
            .items
            .iter()
            .any(|row| serde_json::to_value(row).unwrap() == accepted)
    );
    assert_eq!(
        registered_source_tests::staged_bytes(
            &dir.path().join("proposals.sqlite"),
            &staged.proposal_id
        ),
        immutable
    );
}

#[test]
fn batch_ownership_errors_are_not_reported_as_cancellation() {
    // AC-0161, AC-0163: clearing a cancelled job during an already-started call
    // cannot discard its completed result. The bool broker stop callback still
    // propagates the missing attempt distinctly and launches no further work.
    let dir = tempfile::tempdir().unwrap();
    let state = registered_source_tests::app_state(dir.path());
    let (task, proposal) = registered_source_tests::historical_proposal();
    let (job, execution) = start_job(&state, "escalate-class:1:local").unwrap();
    let mut staged_id = None;
    let error = run_job_staged_batch(
        &state,
        &execution,
        vec![("first".into(), Ok(task))],
        |_, _| Ok(()),
        |task| {
            state.jobs.lock().unwrap().cancel(job.id).unwrap();
            state.jobs.lock().unwrap().clear_finished().unwrap();
            let staged = stage_completed_job_proposal(
                &state,
                &execution,
                task,
                &proposal,
                "snapshot:fixture",
            )?;
            staged_id = Some(staged.proposal_id.clone());
            Ok(staged)
        },
    )
    .unwrap_err();
    assert_ne!(error, "cancelled");
    assert_eq!(error, job_cancelled(&state, &execution).unwrap_err());
    let staged_id = staged_id.unwrap();
    assert_eq!(
        state
            .proposals
            .lock()
            .unwrap()
            .list(20, None)
            .unwrap()
            .items[0]
            .proposal_id,
        staged_id
    );
    assert!(
        state
            .jobs
            .lock()
            .unwrap()
            .finish_execution(&execution, &[])
            .is_err()
    );
    drop(execution);
    drop(state);
    let reopened = registered_source_tests::app_state(dir.path());
    assert_eq!(
        reopened
            .proposals
            .lock()
            .unwrap()
            .list(20, None)
            .unwrap()
            .items[0]
            .proposal_id,
        staged_id
    );
}

#[test]
fn legacy_unknown_host_retry_preserves_original_job_fields() {
    // AC-0163: even an otherwise supported kind cannot acquire an invented
    // execution history. Startup, rejected retry and fresh work preserve it.
    let dir = tempfile::tempdir().unwrap();
    let state = registered_source_tests::app_state(dir.path());
    let conn = rusqlite::Connection::open(dir.path().join("state.db")).unwrap();
    conn.execute("INSERT INTO jobs(kind,status,stage,progress,error) VALUES ('noop','running','legacy',37,'original detail')", []).unwrap();
    let id = conn.last_insert_rowid();
    let original = serde_json::to_value(state.jobs.lock().unwrap().get(id).unwrap()).unwrap();
    assert_eq!(original["execution_tracking"], "legacy_unknown");
    assert!(
        recover_jobs(&state.jobs, &state.job_execution_locks)
            .unwrap()
            .is_empty()
    );
    assert!(prepare_job_retry(&state, id).is_err());
    assert_eq!(
        serde_json::to_value(state.jobs.lock().unwrap().get(id).unwrap()).unwrap(),
        original
    );
    let (fresh, execution) = start_job(&state, "noop").unwrap();
    assert_ne!(fresh.id, id);
    assert_eq!(
        serde_json::to_value(&fresh).unwrap()["execution_tracking"],
        "recorded"
    );
    state
        .jobs
        .lock()
        .unwrap()
        .finish_execution(&execution, &[])
        .unwrap();
    assert_eq!(
        serde_json::to_value(state.jobs.lock().unwrap().get(id).unwrap()).unwrap(),
        original
    );
}

#[test]
fn system_job_removed_during_extraction_stops_before_publication() {
    // AC-0161: cancel+clear from the actual per-file extraction event before the
    // parser returns. The production system intake must not publish those facts
    // or retained source at its post-extraction boundary. No sleeps or polling.
    use std::sync::atomic::{AtomicBool, Ordering};
    use tauri::Listener;

    let dir = tempfile::tempdir().unwrap();
    let app_data = dir.path().join("private");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&app_data).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("source.ts"),
        "export function guarded(enabled: boolean) { if (enabled) return false; }",
    )
    .unwrap();
    let manifest = dir.path().join("cartograph.system.toml");
    std::fs::write(
        &manifest,
        "[[repos]]\nurl = \"project\"\nlayers = [\"client\"]\n",
    )
    .unwrap();
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(registered_source_tests::app_state(&app_data));
    let handle = app.handle().clone();
    let event_handle = handle.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let event_cancelled = cancelled.clone();
    let listener = handle.listen("job://detail", move |event| {
        if event_cancelled.swap(true, Ordering::SeqCst) {
            return;
        }
        let payload: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
        let id = payload["id"].as_i64().unwrap();
        let state = event_handle.state::<AppState>();
        let mut jobs = state.jobs.lock().unwrap();
        jobs.cancel(id).unwrap();
        assert_eq!(jobs.clear_finished().unwrap(), 1);
    });
    let error = add_system_blocking(manifest.to_str().unwrap().to_string(), handle.clone())
        .err()
        .unwrap();
    handle.unlisten(listener);
    assert!(cancelled.load(Ordering::SeqCst));
    assert_eq!(error, JobTransitionError::Missing.to_string());
    let state = handle.state::<AppState>();
    let graph = state.graph.lock().unwrap().read_snapshot().unwrap();
    assert!(graph.0.is_empty() && graph.1.is_empty());
    let source = state.sources.lock().unwrap().list().unwrap().pop().unwrap();
    let retention = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(retention.captures, 0);
    assert_eq!(retention.receipts, 0);
}
