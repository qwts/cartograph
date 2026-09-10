//! Durable job table on the SQLite/WAL state spine (SPEC-00 §8.3–8.4).
//!
//! v2 (#117): jobs carry stage + progress + failure detail + artifact
//! links, and support the full lifecycle — cancel (cooperative), retry,
//! and resume after an interrupted run. Long work stays non-blocking; the
//! UI observes transitions via `job://changed` events emitted by the shell.
//! SPEC-07 adds private attempt ownership; production workers require execution
//! handles, while the old ID-only mutators remain test fixture helpers only.

use crate::job_execution::ExecutionNamespace;
use rusqlite::{Connection, TransactionBehavior, params};

#[path = "jobs/execution.rs"]
mod execution;
pub(crate) use execution::{ClaimMode, ClaimPlan, ExecutionCheck, ExecutionUpdate};
#[cfg(test)]
#[path = "jobs/ownership_tests.rs"]
mod ownership_tests;
use serde::Serialize;
use std::path::Path;

/// A durable job row.
#[derive(Debug, Clone, Serialize)]
pub struct Job {
    /// Whether this row participates in attempt ownership; not a liveness claim.
    pub execution_tracking: ExecutionTracking,
    /// Row id.
    pub id: i64,
    /// Job kind, e.g. `ingest-source-v1:src_<id>`; historical path kinds remain readable.
    pub kind: String,
    /// `queued` | `running` | `done` | `failed` | `cancelled` | `interrupted`.
    pub status: String,
    /// Current pipeline stage while running (e.g. `extract`, `load`).
    pub stage: Option<String>,
    /// Percent complete (0–100) while running; 100 when done.
    pub progress: Option<f64>,
    /// Failure detail once `failed`.
    pub error: Option<String>,
    /// Outputs produced by a completed job (artifact identifiers).
    pub artifacts: Vec<String>,
    /// Creation timestamp (UTC, ISO-8601).
    pub created_at: String,
    /// Last transition timestamp (UTC, ISO-8601).
    pub updated_at: String,
}

/// Private metadata presence, returned with the same snapshot as the job row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionTracking {
    Recorded,
    LegacyUnknown,
}

/// Durable paired-eval result on the state spine (SPEC-00 §8.3, §13).
#[derive(Debug, Clone, Serialize)]
pub struct EvalResult {
    /// Row id.
    pub id: i64,
    /// Provider and model identity used for calibration.
    pub provider: String,
    /// Requested precision floor.
    pub precision_floor: f64,
    /// Calibrated similarity threshold.
    pub similarity_threshold: f64,
    /// Measured precision.
    pub precision: f64,
    /// Measured recall.
    pub recall: f64,
    /// Whether the floor passed.
    pub passed: bool,
    /// Number of staged proposals.
    pub proposals: u64,
    /// Number admitted to the overlay.
    pub approved: u64,
    /// Creation timestamp (UTC, ISO-8601).
    pub created_at: String,
}

/// Store for durable jobs, backed by the state-spine database.
pub struct JobStore {
    conn: Connection,
    namespace: ExecutionNamespace,
}

impl JobStore {
    /// Open (creating if absent) the state spine at `path`, in WAL mode.
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let mut conn = Connection::open(path.as_ref())?;
        conn.busy_timeout(std::time::Duration::from_secs(1))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS jobs (
                 id         INTEGER PRIMARY KEY AUTOINCREMENT,
                 kind       TEXT NOT NULL,
                 status     TEXT NOT NULL DEFAULT 'queued',
                 stage      TEXT,
                 progress   REAL,
                 error      TEXT,
                 artifacts  TEXT,
                 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
                 updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
             ) STRICT;
             CREATE TABLE IF NOT EXISTS eval_results (
                 id                   INTEGER PRIMARY KEY AUTOINCREMENT,
                 provider             TEXT NOT NULL,
                 precision_floor      REAL NOT NULL,
                 similarity_threshold REAL NOT NULL,
                 precision            REAL NOT NULL,
                 recall               REAL NOT NULL,
                 passed               INTEGER NOT NULL CHECK (passed IN (0, 1)),
                 proposals             INTEGER NOT NULL,
                 approved              INTEGER NOT NULL,
                 created_at            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
             ) STRICT;",
        )?;
        migrate_v1_jobs(&conn)?;
        let namespace = execution::initialize(&mut conn, path.as_ref())?;
        Ok(Self { conn, namespace })
    }

    /// Enqueue and record unclaimed ownership in one transaction. Earlier rows
    /// without metadata remain legacy-unknown; opening the store never adopts them.
    pub fn enqueue(&mut self, kind: &str) -> rusqlite::Result<Job> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        execution::validate(&tx, &self.namespace)?;
        execution::one(tx.execute("INSERT INTO jobs (kind) VALUES (?1)", params![kind])?)?;
        let id = tx.last_insert_rowid();
        execution::one(tx.execute(
            "INSERT INTO job_attempts (job_id, generation, owner) VALUES (?1, 0, NULL)",
            [id],
        )?)?;
        let job = execution::read_job(&tx, id)?;
        tx.commit()?;
        Ok(job)
    }

    /// Transition a job to `status`.
    #[cfg(test)]
    pub fn set_status(&mut self, id: i64, status: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE jobs SET status = ?2,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
             WHERE id = ?1",
            params![id, status],
        )?;
        Ok(())
    }

    /// Record stage + percent for a running job.
    #[cfg(test)]
    pub fn set_progress(&mut self, id: i64, stage: &str, progress: f64) -> rusqlite::Result<Job> {
        self.conn.execute(
            "UPDATE jobs SET stage = ?2, progress = ?3,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
             WHERE id = ?1",
            params![id, stage, progress.clamp(0.0, 100.0)],
        )?;
        self.get(id)
    }

    /// Complete a job, recording the artifacts it produced. Guarded: only a
    /// job still `queued`/`running` transitions — a cancellation that lands
    /// mid-pipeline is never overwritten back to `done` (the returned row
    /// tells the caller which outcome won).
    #[cfg(test)]
    pub fn finish(&mut self, id: i64, artifacts: &[String]) -> rusqlite::Result<Job> {
        let artifacts_json = serde_json::to_string(artifacts).expect("string vec serializes");
        self.conn.execute(
            "UPDATE jobs SET status = 'done', progress = 100.0, error = NULL,
                 artifacts = ?2,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
             WHERE id = ?1 AND status IN ('queued', 'running')",
            params![id, artifacts_json],
        )?;
        self.get(id)
    }

    /// Fail a job with its error detail preserved for display. Guarded like
    /// [`Self::finish`]: a concurrent cancel wins over a late failure.
    #[cfg(test)]
    pub fn fail(&mut self, id: i64, error: &str) -> rusqlite::Result<Job> {
        self.conn.execute(
            "UPDATE jobs SET status = 'failed', error = ?2,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
             WHERE id = ?1 AND status IN ('queued', 'running')",
            params![id, error],
        )?;
        self.get(id)
    }

    /// Cancel atomically. A completed row wins over a later cancellation, and
    /// cancellation never releases a live worker's execution guard.
    pub fn cancel(&mut self, id: i64) -> Result<Job, JobTransitionError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        execution::validate(&tx, &self.namespace)?;
        let before = execution::read_job(&tx, id).map_err(execution::missing)?;
        if !matches!(before.status.as_str(), "queued" | "running") {
            return Err(JobTransitionError::InvalidFrom {
                verb: "cancel",
                status: before.status,
            });
        }
        execution::one(tx.execute("UPDATE jobs SET status = 'cancelled', updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id = ?1 AND status IN ('queued', 'running')", [id])?)?;
        let job = execution::read_job(&tx, id)?;
        tx.commit()?;
        Ok(job)
    }

    /// Re-queue a failed, cancelled, or interrupted job (clears progress and
    /// error). The caller re-dispatches execution for the job's kind.
    #[cfg(test)]
    pub fn retry(&mut self, id: i64) -> Result<Job, JobTransitionError> {
        let job = self.get(id).map_err(JobTransitionError::Store)?;
        match job.status.as_str() {
            "failed" | "cancelled" | "interrupted" => {
                self.conn
                    .execute(
                        "UPDATE jobs SET status = 'queued', stage = NULL,
                             progress = NULL, error = NULL,
                             updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
                         WHERE id = ?1",
                        params![id],
                    )
                    .map_err(JobTransitionError::Store)?;
                self.get(id).map_err(JobTransitionError::Store)
            }
            other => Err(JobTransitionError::InvalidFrom {
                verb: "retry",
                status: other.to_string(),
            }),
        }
    }

    /// True when the job was cancelled — checked between pipeline stages so
    /// running work stops at the next safe boundary.
    #[cfg(test)]
    pub fn is_cancelled(&self, id: i64) -> rusqlite::Result<bool> {
        match self.get(id) {
            Ok(job) => Ok(job.status == "cancelled"),
            // A missing row means the job was removed (e.g. cleared right
            // after a cancel) while a worker still held its id — fail
            // closed: the guard reads cancelled, the worker stops (#157
            // review). Never fall open and keep writing facts.
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(true),
            Err(error) => Err(error),
        }
    }

    /// Delete terminal jobs (`done` / `failed` / `cancelled`) from the
    /// spine. Queued, running, and interrupted (resumable) work is never
    /// discarded. Returns the number of rows removed.
    pub fn clear_finished(&mut self) -> rusqlite::Result<usize> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        execution::validate(&tx, &self.namespace)?;
        tx.execute("DELETE FROM job_attempts WHERE job_id IN (SELECT id FROM jobs WHERE status IN ('done', 'failed', 'cancelled'))", [])?;
        let removed = tx.execute(
            "DELETE FROM jobs WHERE status IN ('done', 'failed', 'cancelled')",
            [],
        )?;
        tx.commit()?;
        Ok(removed)
    }

    /// All jobs, newest first, with tracking from the same read transaction.
    pub fn list(&self) -> rusqlite::Result<Vec<Job>> {
        let tx = self.conn.unchecked_transaction()?;
        execution::validate(&tx, &self.namespace)?;
        let jobs = {
            let mut stmt = tx.prepare(&format!("{} ORDER BY j.id DESC", execution::JOB_SELECT))?;
            stmt.query_map([], job_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        tx.commit()?;
        Ok(jobs)
    }

    /// Persist one paired-eval operating point and its staging counts.
    pub fn record_eval(
        &mut self,
        provider: &str,
        report: &semantic::EvalReport,
        proposals: usize,
        approved: usize,
    ) -> rusqlite::Result<EvalResult> {
        self.conn.execute(
            "INSERT INTO eval_results (
                 provider, precision_floor, similarity_threshold, precision,
                 recall, passed, proposals, approved
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                provider,
                f64::from(report.precision_floor),
                f64::from(report.similarity_threshold),
                f64::from(report.precision),
                f64::from(report.recall),
                report.passed,
                proposals as i64,
                approved as i64,
            ],
        )?;
        self.get_eval(self.conn.last_insert_rowid())
    }

    /// Eval results, newest first.
    pub fn list_evals(&self) -> rusqlite::Result<Vec<EvalResult>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, provider, precision_floor, similarity_threshold,
                    precision, recall, passed, proposals, approved, created_at
             FROM eval_results ORDER BY id DESC",
        )?;
        stmt.query_map([], eval_row)?.collect::<Result<Vec<_>, _>>()
    }

    /// One job by id.
    pub fn get(&self, id: i64) -> rusqlite::Result<Job> {
        let tx = self.conn.unchecked_transaction()?;
        execution::validate(&tx, &self.namespace)?;
        let job = execution::read_job(&tx, id)?;
        tx.commit()?;
        Ok(job)
    }

    fn get_eval(&self, id: i64) -> rusqlite::Result<EvalResult> {
        self.conn.query_row(
            "SELECT id, provider, precision_floor, similarity_threshold,
                    precision, recall, passed, proposals, approved, created_at
             FROM eval_results WHERE id = ?1",
            params![id],
            eval_row,
        )
    }
}

/// A lifecycle verb applied from a state it does not accept, or a storage
/// failure while transitioning.
#[derive(Debug)]
pub enum JobTransitionError {
    InvalidFrom { verb: &'static str, status: String },
    Store(rusqlite::Error),
    Busy,
    LockUnavailable,
    ForeignStore,
    LegacyUnknown,
    Missing,
    StaleAttempt,
    Stopped,
    InvalidMetadata,
    GenerationOverflow,
}

impl std::fmt::Display for JobTransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidFrom { verb, status } => {
                write!(f, "cannot {verb} a job in status '{status}'")
            }
            Self::Store(error) => write!(f, "job store error: {error}"),
            Self::Busy => {
                f.write_str("The previous job execution is still active; retry after it exits.")
            }
            Self::LockUnavailable => f.write_str(
                "Job execution locking is unavailable or invalid; ownership was not assumed.",
            ),
            Self::ForeignStore => f.write_str("Job execution belongs to a different state store."),
            Self::LegacyUnknown => {
                f.write_str("Historical job ownership is unknown; start a fresh operation.")
            }
            Self::Missing => f.write_str("Job execution was removed; work must stop."),
            Self::StaleAttempt => f.write_str("Job execution is stale; work must stop."),
            Self::Stopped => f.write_str("Job execution has been interrupted; work must stop."),
            Self::InvalidMetadata => {
                f.write_str("Job execution metadata is invalid; work must stop.")
            }
            Self::GenerationOverflow => {
                f.write_str("Job execution generation is exhausted; start a fresh operation.")
            }
        }
    }
}

impl std::error::Error for JobTransitionError {}

impl From<rusqlite::Error> for JobTransitionError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(error)
    }
}

/// Add the v2 columns to a v1 `jobs` table (fresh tables already have them).
fn migrate_v1_jobs(conn: &Connection) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('jobs')")?;
    let existing: Vec<String> = stmt
        .query_map([], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for (column, ddl) in [
        ("stage", "ALTER TABLE jobs ADD COLUMN stage TEXT"),
        ("progress", "ALTER TABLE jobs ADD COLUMN progress REAL"),
        ("error", "ALTER TABLE jobs ADD COLUMN error TEXT"),
        ("artifacts", "ALTER TABLE jobs ADD COLUMN artifacts TEXT"),
    ] {
        if !existing.iter().any(|name| name == column) {
            conn.execute_batch(ddl)?;
        }
    }
    Ok(())
}

fn job_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    let artifacts_json: Option<String> = row.get(6)?;
    let execution_tracking = execution::row_attempt(row)?
        .map_or(ExecutionTracking::LegacyUnknown, |_| {
            ExecutionTracking::Recorded
        });
    Ok(Job {
        execution_tracking,
        id: row.get(0)?,
        kind: row.get(1)?,
        status: row.get(2)?,
        stage: row.get(3)?,
        progress: row.get(4)?,
        error: row.get(5)?,
        artifacts: artifacts_json
            .map(|json| serde_json::from_str(&json).unwrap_or_default())
            .unwrap_or_default(),
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn eval_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EvalResult> {
    Ok(EvalResult {
        id: row.get(0)?,
        provider: row.get(1)?,
        precision_floor: row.get(2)?,
        similarity_threshold: row.get(3)?,
        precision: row.get(4)?,
        recall: row.get(5)?,
        passed: row.get(6)?,
        proposals: row.get::<_, i64>(7)? as u64,
        approved: row.get::<_, i64>(8)? as u64,
        created_at: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_survive_reopen() {
        // M0 exit gate: "job table durable".
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let id = {
            let mut store = JobStore::open(&path).unwrap();
            store.enqueue("ingest").unwrap().id
        };
        let mut store = JobStore::open(&path).unwrap();
        let jobs = store.list().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].kind, "ingest");
        assert_eq!(jobs[0].status, "queued");

        store.set_status(id, "done").unwrap();
        drop(store);
        let store = JobStore::open(&path).unwrap();
        assert_eq!(store.list().unwrap()[0].status, "done");
    }

    #[test]
    fn lifecycle_progress_failure_and_artifacts_are_durable() {
        // #117: stage/percent, failure detail, and artifact links survive
        // restart alongside the status.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let id = {
            let mut store = JobStore::open(&path).unwrap();
            let id = store.enqueue("ingest:/repo").unwrap().id;
            store.set_status(id, "running").unwrap();
            let job = store.set_progress(id, "extract", 40.0).unwrap();
            assert_eq!(job.stage.as_deref(), Some("extract"));
            assert_eq!(job.progress, Some(40.0));
            store
                .finish(id, &["spec-bundle:best-effort".into()])
                .unwrap();
            id
        };
        let store = JobStore::open(&path).unwrap();
        let job = store.get(id).unwrap();
        assert_eq!(job.status, "done");
        assert_eq!(job.progress, Some(100.0));
        assert_eq!(job.artifacts, vec!["spec-bundle:best-effort".to_string()]);
    }

    #[test]
    fn cancel_retry_and_invalid_transitions() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = JobStore::open(dir.path().join("state.db")).unwrap();
        let id = store.enqueue("ingest:/repo").unwrap().id;

        // queued → cancelled; running work observes it cooperatively.
        assert_eq!(store.cancel(id).unwrap().status, "cancelled");
        assert!(store.is_cancelled(id).unwrap());
        // A terminal job cannot be cancelled again…
        assert!(matches!(
            store.cancel(id),
            Err(JobTransitionError::InvalidFrom { verb: "cancel", .. })
        ));
        // …but it can be retried, which clears progress and error.
        let retried = store.retry(id).unwrap();
        assert_eq!(retried.status, "queued");
        assert_eq!(retried.stage, None);
        assert_eq!(retried.error, None);
        // retry from a non-terminal state is rejected.
        assert!(matches!(
            store.retry(id),
            Err(JobTransitionError::InvalidFrom { verb: "retry", .. })
        ));

        let failed = store.enqueue("noop").unwrap().id;
        store.set_status(failed, "running").unwrap();
        let job = store.fail(failed, "adapter panicked: bad span").unwrap();
        assert_eq!(job.status, "failed");
        assert_eq!(job.error.as_deref(), Some("adapter panicked: bad span"));
    }

    #[test]
    fn cancel_wins_races_against_finish_and_fail() {
        // Review fix (#127): a cancel landing mid-pipeline must never be
        // overwritten by the pipeline's own completion or failure.
        let dir = tempfile::tempdir().unwrap();
        let mut store = JobStore::open(dir.path().join("state.db")).unwrap();

        let id = store.enqueue("ingest:/repo").unwrap().id;
        store.set_status(id, "running").unwrap();
        store.cancel(id).unwrap();
        let job = store.finish(id, &["graph:x".into()]).unwrap();
        assert_eq!(job.status, "cancelled");
        assert!(job.artifacts.is_empty());

        let other = store.enqueue("ingest:/repo2").unwrap().id;
        store.set_status(other, "running").unwrap();
        store.cancel(other).unwrap();
        let job = store.fail(other, "late error").unwrap();
        assert_eq!(job.status, "cancelled");
        assert_eq!(job.error, None);
    }

    #[test]
    fn v1_state_spine_migrates_in_place() {
        // An existing pre-#117 database gains the v2 columns without losing
        // rows (the spine is durable across schema growth).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE jobs (
                     id         INTEGER PRIMARY KEY AUTOINCREMENT,
                     kind       TEXT NOT NULL,
                     status     TEXT NOT NULL DEFAULT 'queued',
                     created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
                     updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
                 ) STRICT;
                 INSERT INTO jobs (kind, status) VALUES ('ingest:/old', 'done');",
            )
            .unwrap();
        }
        let mut store = JobStore::open(&path).unwrap();
        let jobs = store.list().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].kind, "ingest:/old");
        assert_eq!(jobs[0].artifacts, Vec::<String>::new());
        // v2 verbs work on migrated rows.
        let id = store.enqueue("noop").unwrap().id;
        store.set_status(id, "running").unwrap();
        store.set_progress(id, "extract", 10.0).unwrap();
        assert_eq!(store.get(id).unwrap().stage.as_deref(), Some("extract"));
    }

    #[test]
    fn eval_results_survive_reopen() {
        // AC-0022: the operating point is durable, not detached from the
        // best-effort result it gated.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let report = semantic::EvalReport {
            precision_floor: 0.9,
            similarity_threshold: 0.7,
            precision: 1.0,
            recall: 0.8,
            passed: true,
            true_positives: 4,
            false_positives: 0,
            false_negatives: 1,
        };
        {
            let mut store = JobStore::open(&path).unwrap();
            store
                .record_eval("ollama:nomic-embed-text", &report, 3, 1)
                .unwrap();
        }
        let store = JobStore::open(&path).unwrap();
        let rows = store.list_evals().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider, "ollama:nomic-embed-text");
        assert!(rows[0].passed);
        assert_eq!(rows[0].approved, 1);
    }
}
