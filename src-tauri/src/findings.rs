//! Durable register findings on the state spine (#116).
//!
//! Findings are the register entries that are **not graph facts** (R-INT-2's
//! honest complement): `unsupported` — no adapter covers the construct, a
//! tool limitation — and `no-evidence` — a question the recovery could not
//! find any evidence for. System Gaps are *not* stored here: a Gap is an
//! explicit graph node with provenance; conflating the lanes is exactly what
//! the three-way classification forbids.

use rusqlite::{Connection, params};
use serde::Serialize;
use std::path::Path;

/// One persisted register finding.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// Row id.
    pub id: i64,
    /// `unsupported` | `no-evidence`.
    pub kind: String,
    /// Detector registry version that produced it (e.g. `preflight@1`).
    pub detector: String,
    /// Repo identity the finding belongs to.
    pub repo: String,
    /// Repo-relative path.
    pub path: String,
    /// 1-based line.
    pub line: i64,
    /// Human explanation for the register row.
    pub message: String,
    /// Creation timestamp (UTC, ISO-8601).
    pub created_at: String,
}

/// Input for one finding to record.
pub struct NewFinding<'a> {
    pub kind: &'a str,
    pub detector: &'a str,
    pub path: &'a str,
    pub line: i64,
    pub message: &'a str,
}

/// Store for register findings, backed by the state-spine database.
pub struct FindingStore {
    conn: Connection,
}

impl FindingStore {
    /// Open (creating if absent) the findings table on the spine at `path`.
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS findings (
                 id         INTEGER PRIMARY KEY AUTOINCREMENT,
                 kind       TEXT NOT NULL CHECK (kind IN ('unsupported', 'no-evidence')),
                 detector   TEXT NOT NULL,
                 repo       TEXT NOT NULL,
                 path       TEXT NOT NULL,
                 line       INTEGER NOT NULL,
                 message    TEXT NOT NULL,
                 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
             ) STRICT;",
        )?;
        Ok(Self { conn })
    }

    /// Replace `repo`'s findings from `detector` with `batch` — a re-run of
    /// the same detector supersedes its previous report instead of piling
    /// duplicates (deterministic re-preflight ⇒ identical register).
    pub fn replace_for(
        &mut self,
        repo: &str,
        detector: &str,
        batch: &[NewFinding<'_>],
    ) -> rusqlite::Result<Vec<Finding>> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM findings WHERE repo = ?1 AND detector = ?2",
            params![repo, detector],
        )?;
        for finding in batch {
            tx.execute(
                "INSERT INTO findings (kind, detector, repo, path, line, message)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    finding.kind,
                    finding.detector,
                    repo,
                    finding.path,
                    finding.line,
                    finding.message
                ],
            )?;
        }
        tx.commit()?;
        self.list_for(repo)
    }

    /// All findings for `repo`, oldest first (stable register order).
    pub fn list_for(&self, repo: &str) -> rusqlite::Result<Vec<Finding>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, detector, repo, path, line, message, created_at
             FROM findings WHERE repo = ?1 ORDER BY id",
        )?;
        stmt.query_map(params![repo], finding_row)?
            .collect::<Result<Vec<_>, _>>()
    }

    /// All findings across repos, oldest first.
    pub fn list(&self) -> rusqlite::Result<Vec<Finding>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, detector, repo, path, line, message, created_at
             FROM findings ORDER BY id",
        )?;
        stmt.query_map([], finding_row)?
            .collect::<Result<Vec<_>, _>>()
    }

    /// Count per kind: `(unsupported, no_evidence)`.
    pub fn counts(&self) -> rusqlite::Result<(u64, u64)> {
        let count = |kind: &str| -> rusqlite::Result<u64> {
            self.conn.query_row(
                "SELECT COUNT(*) FROM findings WHERE kind = ?1",
                params![kind],
                |r| r.get::<_, i64>(0).map(|n| n as u64),
            )
        };
        Ok((count("unsupported")?, count("no-evidence")?))
    }
}

fn finding_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Finding> {
    Ok(Finding {
        id: row.get(0)?,
        kind: row.get(1)?,
        detector: row.get(2)?,
        repo: row.get(3)?,
        path: row.get(4)?,
        line: row.get(5)?,
        message: row.get(6)?,
        created_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unsupported(path: &'static str, message: &'static str) -> NewFinding<'static> {
        NewFinding {
            kind: "unsupported",
            detector: "preflight@1",
            path,
            line: 1,
            message,
        }
    }

    #[test]
    fn findings_are_durable_and_rescans_supersede() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            let mut store = FindingStore::open(&path).unwrap();
            store
                .replace_for(
                    "local/app",
                    "preflight@1",
                    &[
                        unsupported("src/a.ts", "inline eval()"),
                        unsupported("mod.wasm", "WASM module"),
                    ],
                )
                .unwrap();
            // Re-preflight of the same tree supersedes, never duplicates.
            store
                .replace_for(
                    "local/app",
                    "preflight@1",
                    &[unsupported("src/a.ts", "inline eval()")],
                )
                .unwrap();
        }
        let store = FindingStore::open(&path).unwrap();
        let rows = store.list_for("local/app").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, "unsupported");
        assert_eq!(rows[0].detector, "preflight@1");
        assert_eq!(store.counts().unwrap(), (1, 0));
    }

    #[test]
    fn kind_vocabulary_is_enforced() {
        // A Gap can never be smuggled into the findings register — the
        // three-way split is a schema constraint, not a convention.
        let dir = tempfile::tempdir().unwrap();
        let mut store = FindingStore::open(dir.path().join("state.db")).unwrap();
        let bogus = NewFinding {
            kind: "gap",
            detector: "preflight@1",
            path: "x",
            line: 1,
            message: "not allowed",
        };
        assert!(
            store
                .replace_for("local/app", "preflight@1", &[bogus])
                .is_err()
        );
    }
}
