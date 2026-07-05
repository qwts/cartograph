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
    // Orchestration starts calling this at M1 (ingest jobs); until then only
    // the durability test exercises it.
    #[allow(dead_code)]
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
}
