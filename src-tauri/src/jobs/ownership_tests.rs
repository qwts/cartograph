use super::*;
use crate::job_execution::{JobExecution, JobExecutionLocks};

fn fixture() -> (tempfile::TempDir, JobStore, JobExecutionLocks) {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open(dir.path().join("state.db")).unwrap();
    let locks = JobExecutionLocks::open(dir.path(), store.execution_namespace()).unwrap();
    (dir, store, locks)
}

fn start(store: &mut JobStore, locks: &JobExecutionLocks) -> JobExecution {
    let id = store.enqueue("ingest-source-v1:src_test").unwrap().id;
    let plan = store.claim_plan(id, ClaimMode::StartQueued).unwrap();
    let reservation = locks.try_reserve(plan.lock_target()).unwrap();
    store.claim_execution(&plan, reservation).unwrap().1
}

fn value(job: &Job) -> serde_json::Value {
    serde_json::to_value(job).unwrap()
}

#[test]
fn execution_metadata_preserves_legacy_history_and_namespace() {
    // AC-0156 / AC-0163: metadata migration does not rewrite historical rows.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE jobs (id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'queued', stage TEXT, progress REAL, error TEXT, artifacts TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL) STRICT;
        INSERT INTO jobs VALUES (42, 'ingest:/old', 'running', 'extract', 45.0, 'old error', '[\"proposal:old\"]', 'original creation', 'original update');
        CREATE TABLE historical_proposals (id TEXT, payload BLOB, review_revision INTEGER);
        INSERT INTO historical_proposals VALUES ('proposal:old', X'007B22686973746F7279227D', 7);
        PRAGMA user_version = 73;").unwrap();
    drop(conn);
    let mut store = JobStore::open(&path).unwrap();
    let old = store.get(42).unwrap();
    assert_eq!(old.execution_tracking, ExecutionTracking::LegacyUnknown);
    assert_eq!(old.kind, "ingest:/old");
    assert_eq!(old.status, "running");
    assert_eq!(old.stage.as_deref(), Some("extract"));
    assert_eq!(old.progress, Some(45.0));
    assert_eq!(old.error.as_deref(), Some("old error"));
    assert_eq!(old.artifacts, ["proposal:old"]);
    assert_eq!(old.created_at, "original creation");
    assert_eq!(old.updated_at, "original update");
    assert!(store.recovery_candidates().unwrap().is_empty());
    assert!(matches!(
        store.claim_plan(42, ClaimMode::StartQueued),
        Err(JobTransitionError::LegacyUnknown)
    ));
    store.cancel(42).unwrap();
    assert!(matches!(
        store.claim_plan(42, ClaimMode::RetryTerminal),
        Err(JobTransitionError::LegacyUnknown)
    ));
    // The custom legacy schema deliberately has no timestamp defaults, so use
    // the original v2 schema for fresh enqueue coverage in the separate store.
    let (_fresh_dir, mut fresh, _fresh_locks) = fixture();
    let job = fresh.enqueue("fresh").unwrap();
    assert_eq!(job.execution_tracking, ExecutionTracking::Recorded);
    assert_eq!(value(&job)["execution_tracking"], "recorded");
    assert!(!value(&job).as_object().unwrap().contains_key("owner"));
    let namespace = store.execution_namespace();
    drop(store);
    let reopened = JobStore::open(&path).unwrap();
    assert!(reopened.execution_namespace() == namespace);
    assert_eq!(
        reopened
            .conn
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        73
    );
    let history: (Vec<u8>, i64) = reopened
        .conn
        .query_row(
            "SELECT payload, review_revision FROM historical_proposals",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(history, (b"\0{\"history\"}".to_vec(), 7));
}

#[test]
fn cancelled_live_execution_excludes_retry_and_updates_are_fenced() {
    // AC-0158 / AC-0160: cancellation does not hand ownership to a retry.
    let (dir, mut store, locks) = fixture();
    let mut other = JobStore::open(dir.path().join("state.db")).unwrap();
    let execution = start(&mut store, &locks);
    store
        .progress_execution(&execution, "extract", 41.0)
        .unwrap();
    let cancelled = other.cancel(execution.id()).unwrap();
    assert_eq!(
        store.check_execution(&execution).unwrap(),
        ExecutionCheck::Cancelled
    );
    for result in [
        store.progress_execution(&execution, "late", 90.0),
        store.finish_execution(&execution, &["late".into()]),
        store.fail_execution(&execution, "late"),
    ] {
        let ExecutionUpdate::Unchanged(job) = result.unwrap() else {
            panic!("cancel must win");
        };
        assert_eq!(value(&job), value(&cancelled));
    }
    let retry = other
        .claim_plan(execution.id(), ClaimMode::RetryTerminal)
        .unwrap();
    assert!(matches!(
        locks.try_reserve(retry.lock_target()),
        Err(JobTransitionError::Busy)
    ));
    let id = execution.id();
    drop(execution);
    let reservation = locks.try_reserve(retry.lock_target()).unwrap();
    let (running, retry_execution) = other.claim_execution(&retry, reservation).unwrap();
    assert_eq!(running.status, "running");
    assert_eq!(running.id, id);
    assert_eq!(running.created_at, cancelled.created_at);
    assert_eq!(running.kind, cancelled.kind);
    assert_eq!(running.stage, None);
    assert_eq!(running.progress, None);
    assert_eq!(retry_execution.inner.generation, 2);
    // Independently challenge the SQL fence with a valid but different owner.
    // Normal callers cannot mutate private attempt metadata this way.
    store
        .conn
        .execute(
            "UPDATE job_attempts SET generation = generation + 1, owner = ?2 WHERE job_id = ?1",
            params![id, "00000000000000000000000000000000"],
        )
        .unwrap();
    for result in [
        other.progress_execution(&retry_execution, "stale", 99.0),
        other.finish_execution(&retry_execution, &["stale".into()]),
        other.fail_execution(&retry_execution, "stale"),
    ] {
        assert!(matches!(result, Err(JobTransitionError::StaleAttempt)));
    }
    assert!(matches!(
        other.check_execution(&retry_execution),
        Err(JobTransitionError::StaleAttempt)
    ));
    assert_eq!(value(&store.get(id).unwrap()), value(&running));
}

#[test]
fn competing_claims_and_cancel_plans_preserve_the_winner() {
    // AC-0157 / AC-0160: independent connections compete using copied plans.
    let (dir, mut first, locks) = fixture();
    let mut second = JobStore::open(dir.path().join("state.db")).unwrap();
    let id = first.enqueue("noop").unwrap().id;
    let a = first.claim_plan(id, ClaimMode::StartQueued).unwrap();
    let b = second.claim_plan(id, ClaimMode::StartQueued).unwrap();
    let reservation = locks.try_reserve(a.lock_target()).unwrap();
    assert!(matches!(
        locks.try_reserve(b.lock_target()),
        Err(JobTransitionError::Busy)
    ));
    let (_, execution) = first.claim_execution(&a, reservation).unwrap();
    let done = first
        .finish_execution(&execution, &["artifact".into()])
        .unwrap();
    let ExecutionUpdate::Applied(done) = done else {
        panic!("finish should apply");
    };
    drop(execution);
    let reservation = locks.try_reserve(b.lock_target()).unwrap();
    assert!(matches!(
        second.claim_execution(&b, reservation),
        Err(JobTransitionError::StaleAttempt)
    ));
    assert!(matches!(
        second.cancel(id),
        Err(JobTransitionError::InvalidFrom { .. })
    ));
    assert_eq!(value(&second.get(id).unwrap()), value(&done));
    let id = first.enqueue("noop").unwrap().id;
    let plan = first.claim_plan(id, ClaimMode::StartQueued).unwrap();
    let reservation = locks.try_reserve(plan.lock_target()).unwrap();
    let cancelled = second.cancel(id).unwrap();
    assert!(matches!(
        first.claim_execution(&plan, reservation),
        Err(JobTransitionError::StaleAttempt)
    ));
    assert_eq!(value(&first.get(id).unwrap()), value(&cancelled));
    assert_eq!(
        first
            .conn
            .query_row(
                "SELECT generation FROM job_attempts WHERE job_id = ?1",
                [id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn recovery_is_conditional_on_the_observed_attempt() {
    // AC-0159: terminal transitions and newer attempts beat copied candidates.
    let (dir, mut worker, locks) = fixture();
    let mut recovery = JobStore::open(dir.path().join("state.db")).unwrap();
    for outcome in ["done", "cancelled", "deleted", "newer"] {
        let execution = start(&mut worker, &locks);
        let candidate = recovery.recovery_candidates().unwrap().pop().unwrap();
        assert!(matches!(
            locks.try_reserve(candidate.lock_target()),
            Err(JobTransitionError::Busy)
        ));
        match outcome {
            "done" => {
                worker
                    .finish_execution(&execution, &["complete".into()])
                    .unwrap();
            }
            _ => {
                worker.cancel(execution.id()).unwrap();
            }
        }
        let id = execution.id();
        drop(execution);
        if outcome == "deleted" {
            worker.clear_finished().unwrap();
        }
        if outcome == "newer" {
            let plan = worker.claim_plan(id, ClaimMode::RetryTerminal).unwrap();
            let reservation = locks.try_reserve(plan.lock_target()).unwrap();
            let (_, newer) = worker.claim_execution(&plan, reservation).unwrap();
            drop(newer);
        }
        let before = worker.get(id).ok().map(|j| value(&j));
        let reservation = locks.try_reserve(candidate.lock_target()).unwrap();
        assert!(
            recovery
                .recover_reserved(&candidate, reservation)
                .unwrap()
                .is_none()
        );
        assert_eq!(worker.get(id).ok().map(|j| value(&j)), before);
    }
}

#[test]
fn interrupted_jobs_are_recovered_and_resumable() {
    // AC-0159 / AC-0160: only a released, recorded attempt can be recovered.
    let (dir, mut store, locks) = fixture();
    let execution = start(&mut store, &locks);
    let id = execution.id();
    drop(execution);
    drop(store);
    let mut store = JobStore::open(dir.path().join("state.db")).unwrap();
    let candidate = store.recovery_candidates().unwrap().pop().unwrap();
    let reservation = locks.try_reserve(candidate.lock_target()).unwrap();
    assert_eq!(
        store
            .recover_reserved(&candidate, reservation)
            .unwrap()
            .unwrap()
            .status,
        "interrupted"
    );
    assert!(store.recovery_candidates().unwrap().is_empty());
    let retry = store.claim_plan(id, ClaimMode::RetryTerminal).unwrap();
    let reservation = locks.try_reserve(retry.lock_target()).unwrap();
    let (_, execution) = store.claim_execution(&retry, reservation).unwrap();
    assert_eq!(execution.inner.generation, 2);
    // Fault injection proves an interrupted handle stops distinctly, even if
    // some outside actor changes state while its execution guard remains live.
    store.set_status(id, "interrupted").unwrap();
    assert!(matches!(
        store.check_execution(&execution),
        Err(JobTransitionError::Stopped)
    ));
    assert!(matches!(
        store.progress_execution(&execution, "late", 1.0),
        Err(JobTransitionError::Stopped)
    ));
    assert!(matches!(
        store.finish_execution(&execution, &[]),
        Err(JobTransitionError::Stopped)
    ));
    assert!(matches!(
        store.fail_execution(&execution, "late"),
        Err(JobTransitionError::Stopped)
    ));
}

#[test]
fn cleared_execution_stops_without_erasing_other_history() {
    // AC-0158 / AC-0163: cleanup does not release a worker or delete proposal history.
    let (_dir, mut store, locks) = fixture();
    store.conn.execute_batch("CREATE TABLE historical_proposals (payload BLOB, review_revision INTEGER); INSERT INTO historical_proposals VALUES (X'006669786564FF', 8);").unwrap();
    let execution = start(&mut store, &locks);
    store.cancel(execution.id()).unwrap();
    let plan = store
        .claim_plan(execution.id(), ClaimMode::RetryTerminal)
        .unwrap();
    assert_eq!(store.clear_finished().unwrap(), 1);
    assert!(matches!(
        locks.try_reserve(plan.lock_target()),
        Err(JobTransitionError::Busy)
    ));
    assert!(matches!(
        store.check_execution(&execution),
        Err(JobTransitionError::Missing)
    ));
    assert!(matches!(
        store.progress_execution(&execution, "late", 5.0),
        Err(JobTransitionError::Missing)
    ));
    assert!(matches!(
        store.finish_execution(&execution, &[]),
        Err(JobTransitionError::Missing)
    ));
    assert!(matches!(
        store.fail_execution(&execution, "late"),
        Err(JobTransitionError::Missing)
    ));
    let history: (Vec<u8>, i64) = store
        .conn
        .query_row(
            "SELECT payload, review_revision FROM historical_proposals",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(history, (b"\0fixed\xff".to_vec(), 8));
    let next = store.enqueue("fresh").unwrap();
    assert!(next.id > execution.id());
    drop(execution);
    assert!(locks.try_reserve(plan.lock_target()).is_ok());
}

#[test]
fn invalid_metadata_and_ignored_claims_fail_closed() {
    // AC-0156 / AC-0158 / AC-0160: malformed policy never grants an attempt.
    let (_dir, mut store, locks) = fixture();
    let id = store.enqueue("noop").unwrap().id;
    let plan = store.claim_plan(id, ClaimMode::StartQueued).unwrap();
    let reservation = locks.try_reserve(plan.lock_target()).unwrap();
    store.conn.execute_batch("CREATE TRIGGER suppress_job_claim BEFORE UPDATE ON jobs BEGIN SELECT RAISE(IGNORE); END;").unwrap();
    assert!(store.claim_execution(&plan, reservation).is_err());
    store
        .conn
        .execute_batch("DROP TRIGGER suppress_job_claim;")
        .unwrap();
    assert_eq!(store.get(id).unwrap().status, "queued");
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT generation FROM job_attempts WHERE job_id = ?1",
                [id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let plan = store.claim_plan(id, ClaimMode::StartQueued).unwrap();
    let reservation = locks.try_reserve(plan.lock_target()).unwrap();
    store.conn.execute_batch("CREATE TRIGGER suppress_metadata_claim BEFORE UPDATE ON job_attempts BEGIN SELECT RAISE(IGNORE); END;").unwrap();
    assert!(store.claim_execution(&plan, reservation).is_err());
    store
        .conn
        .execute_batch(
            "DROP TRIGGER suppress_metadata_claim; PRAGMA ignore_check_constraints = ON;",
        )
        .unwrap();
    for (generation, owner) in [
        (-1_i64, None),
        (0, Some("oversized-non-null-owner".repeat(100))),
        (1, Some("bad".into())),
    ] {
        store
            .conn
            .execute(
                "UPDATE job_attempts SET generation = ?2, owner = ?3 WHERE job_id = ?1",
                params![id, generation, owner],
            )
            .unwrap();
        assert!(store.claim_plan(id, ClaimMode::StartQueued).is_err());
        assert!(store.get(id).is_err());
    }
    store
        .conn
        .execute(
            "UPDATE job_attempts SET generation = 0, owner = NULL WHERE job_id = ?1",
            [id],
        )
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE job_execution_meta SET namespace = ?1",
            ["../unsafe"],
        )
        .unwrap();
    assert!(store.list().is_err());
    assert!(store.enqueue("not published").is_err());
}

#[test]
fn generation_overflow_and_foreign_reservations_leave_history_unchanged() {
    // AC-0156 / AC-0158: no wrapping and no reservation from another state store.
    let (dir, mut store, locks) = fixture();
    let execution = start(&mut store, &locks);
    let id = execution.id();
    store.cancel(id).unwrap();
    drop(execution);
    store
        .conn
        .execute(
            "UPDATE job_attempts SET generation = ?2 WHERE job_id = ?1",
            params![id, i64::MAX],
        )
        .unwrap();
    let plan = store.claim_plan(id, ClaimMode::RetryTerminal).unwrap();
    let before = value(&store.get(id).unwrap());
    let reservation = locks.try_reserve(plan.lock_target()).unwrap();
    assert!(matches!(
        store.claim_execution(&plan, reservation),
        Err(JobTransitionError::GenerationOverflow)
    ));
    assert_eq!(value(&store.get(id).unwrap()), before);
    let mut other = JobStore::open(dir.path().join("other.db")).unwrap();
    let other_locks = JobExecutionLocks::open(dir.path(), other.execution_namespace()).unwrap();
    let other_id = other.enqueue("other").unwrap().id;
    let other_plan = other.claim_plan(other_id, ClaimMode::StartQueued).unwrap();
    let reservation = other_locks.try_reserve(other_plan.lock_target()).unwrap();
    assert!(matches!(
        store.claim_execution(&plan, reservation),
        Err(JobTransitionError::ForeignStore)
    ));
    assert_eq!(value(&store.get(id).unwrap()), before);
}
