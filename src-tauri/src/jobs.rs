//! Durable job table on the SQLite/WAL state spine (SPEC-00 §8.3–8.4).
//!
//! M0 exit gate: jobs survive an app restart. Orchestration (workers,
//! resumption, progress events) grows here in later milestones.

use rusqlite::{Connection, params};
use serde::Serialize;
use std::path::Path;

/// A durable job row.
#[derive(Debug, Clone, Serialize)]
pub struct Job {
    /// Row id.
    pub id: i64,
    /// Job kind, e.g. `ingest`.
    pub kind: String,
    /// `queued` | `running` | `done` | `failed`.
    pub status: String,
    /// Creation timestamp (UTC, ISO-8601).
    pub created_at: String,
    /// Last transition timestamp (UTC, ISO-8601).
    pub updated_at: String,
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
}

impl JobStore {
    /// Open (creating if absent) the state spine at `path`, in WAL mode.
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS jobs (
                 id         INTEGER PRIMARY KEY AUTOINCREMENT,
                 kind       TEXT NOT NULL,
                 status     TEXT NOT NULL DEFAULT 'queued',
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
        Ok(Self { conn })
    }

    /// Enqueue a job of `kind`; returns the stored row.
    pub fn enqueue(&mut self, kind: &str) -> rusqlite::Result<Job> {
        self.conn
            .execute("INSERT INTO jobs (kind) VALUES (?1)", params![kind])?;
        let id = self.conn.last_insert_rowid();
        self.get(id)
    }

    /// Transition a job to `status`.
    pub fn set_status(&mut self, id: i64, status: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE jobs SET status = ?2,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
             WHERE id = ?1",
            params![id, status],
        )?;
        Ok(())
    }

    /// All jobs, newest first.
    pub fn list(&self) -> rusqlite::Result<Vec<Job>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, status, created_at, updated_at FROM jobs ORDER BY id DESC",
        )?;
        let jobs = stmt
            .query_map([], |r| {
                Ok(Job {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    status: r.get(2)?,
                    created_at: r.get(3)?,
                    updated_at: r.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
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

    fn get(&self, id: i64) -> rusqlite::Result<Job> {
        self.conn.query_row(
            "SELECT id, kind, status, created_at, updated_at FROM jobs WHERE id = ?1",
            params![id],
            |r| {
                Ok(Job {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    status: r.get(2)?,
                    created_at: r.get(3)?,
                    updated_at: r.get(4)?,
                })
            },
        )
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
