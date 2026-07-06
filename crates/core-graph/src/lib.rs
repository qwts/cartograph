//! Unified knowledge-graph store (SPEC-00 §4).
//!
//! [`GraphStore`] is the storage abstraction; the primary implementation is
//! SQLite/WAL with recursive-CTE traversal ([`SqliteGraphStore`]), per
//! ADR-0008 (Kuzu was archived upstream at the M0 verify-at-build). A future
//! embedded-graph-engine adapter implements the same trait if the OQ-3
//! benchmark ever demands it.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A node in the unified graph (code or domain layer, SPEC-00 §4.1–4.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// Stable identifier (content-addressed by callers from M1 on).
    pub id: String,
    /// Node label, e.g. `Symbol`, `Endpoint`, `Resource`, `Channel`.
    pub label: String,
    /// JSON properties (schema per label).
    pub props: serde_json::Value,
}

/// A directed edge in the unified graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    /// Source node id.
    pub src: String,
    /// Destination node id.
    pub dst: String,
    /// Edge label, e.g. `CALLS`, `PUBLISHES`, `DEPENDS_ON`.
    pub label: String,
    /// JSON properties (provenance is attached here from M1 on).
    pub props: serde_json::Value,
}

/// Errors from graph-store operations.
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    /// Underlying storage failure.
    #[error("storage: {0}")]
    Storage(#[from] rusqlite::Error),
    /// Property (de)serialization failure.
    #[error("props: {0}")]
    Props(#[from] serde_json::Error),
}

/// Storage abstraction for the unified graph (ADR-0008).
pub trait GraphStore {
    /// Insert or replace a node by id.
    fn put_node(&mut self, node: &Node) -> Result<(), GraphError>;
    /// Insert or replace an edge by (src, dst, label).
    fn put_edge(&mut self, edge: &Edge) -> Result<(), GraphError>;
    /// Fetch a node by id.
    fn get_node(&self, id: &str) -> Result<Option<Node>, GraphError>;
    /// Number of nodes.
    fn node_count(&self) -> Result<u64, GraphError>;
    /// Number of edges.
    fn edge_count(&self) -> Result<u64, GraphError>;
    /// All node ids reachable from `start` following outgoing edges,
    /// optionally restricted to one edge label. Excludes `start` itself
    /// unless it lies on a cycle.
    fn reachable_from(&self, start: &str, label: Option<&str>) -> Result<Vec<String>, GraphError>;
    /// All nodes carrying `label`, ordered by id.
    fn nodes_with_label(&self, label: &str) -> Result<Vec<Node>, GraphError>;
}

/// SQLite/WAL implementation — node/edge tables + recursive-CTE traversal.
pub struct SqliteGraphStore {
    conn: Connection,
}

impl SqliteGraphStore {
    /// Open (creating if absent) a graph database at `path`, in WAL mode.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GraphError> {
        Self::init(Connection::open(path)?)
    }

    /// Open an in-memory graph (tests, scratch analysis).
    pub fn open_in_memory() -> Result<Self, GraphError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, GraphError> {
        // WAL is a no-op for in-memory databases; harmless to set anyway.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS nodes (
                 id    TEXT PRIMARY KEY,
                 label TEXT NOT NULL,
                 props TEXT NOT NULL DEFAULT '{}'
             ) STRICT;
             CREATE TABLE IF NOT EXISTS edges (
                 src   TEXT NOT NULL REFERENCES nodes(id),
                 dst   TEXT NOT NULL REFERENCES nodes(id),
                 label TEXT NOT NULL,
                 props TEXT NOT NULL DEFAULT '{}',
                 PRIMARY KEY (src, dst, label)
             ) STRICT;
             CREATE INDEX IF NOT EXISTS idx_edges_src ON edges(src);
             CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges(dst);",
        )?;
        Ok(Self { conn })
    }
}

impl GraphStore for SqliteGraphStore {
    fn put_node(&mut self, node: &Node) -> Result<(), GraphError> {
        self.conn.execute(
            "INSERT INTO nodes (id, label, props) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET label = excluded.label, props = excluded.props",
            params![node.id, node.label, serde_json::to_string(&node.props)?],
        )?;
        Ok(())
    }

    fn put_edge(&mut self, edge: &Edge) -> Result<(), GraphError> {
        self.conn.execute(
            "INSERT INTO edges (src, dst, label, props) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(src, dst, label) DO UPDATE SET props = excluded.props",
            params![
                edge.src,
                edge.dst,
                edge.label,
                serde_json::to_string(&edge.props)?
            ],
        )?;
        Ok(())
    }

    fn get_node(&self, id: &str) -> Result<Option<Node>, GraphError> {
        let row = self
            .conn
            .query_row(
                "SELECT id, label, props FROM nodes WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(id, label, props)| {
            Ok(Node {
                id,
                label,
                props: serde_json::from_str(&props)?,
            })
        })
        .transpose()
    }

    fn node_count(&self) -> Result<u64, GraphError> {
        // SQLite integers are i64; rusqlite 0.40 dropped FromSql for u64.
        // COUNT(*) is never negative, so the cast is lossless.
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    fn edge_count(&self) -> Result<u64, GraphError> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    fn reachable_from(&self, start: &str, label: Option<&str>) -> Result<Vec<String>, GraphError> {
        // UNION (not UNION ALL) deduplicates and therefore terminates on cycles.
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE reach(id) AS (
                 SELECT dst FROM edges WHERE src = ?1 AND (?2 IS NULL OR label = ?2)
                 UNION
                 SELECT e.dst FROM edges e JOIN reach r ON e.src = r.id
                 WHERE (?2 IS NULL OR e.label = ?2)
             )
             SELECT id FROM reach ORDER BY id",
        )?;
        let ids = stmt
            .query_map(params![start, label], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
    }

    fn nodes_with_label(&self, label: &str) -> Result<Vec<Node>, GraphError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, label, props FROM nodes WHERE label = ?1 ORDER BY id")?;
        let rows = stmt.query_map(params![label], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut nodes = Vec::new();
        for row in rows {
            let (id, label, props) = row?;
            nodes.push(Node {
                id,
                label,
                props: serde_json::from_str(&props)?,
            });
        }
        Ok(nodes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, label: &str) -> Node {
        Node {
            id: id.into(),
            label: label.into(),
            props: serde_json::json!({}),
        }
    }

    fn edge(src: &str, dst: &str, label: &str) -> Edge {
        Edge {
            src: src.into(),
            dst: dst.into(),
            label: label.into(),
            props: serde_json::json!({}),
        }
    }

    #[test]
    fn empty_graph_round_trips_through_reopen() {
        // M0 exit gate: "empty graph round-trips".
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        {
            let store = SqliteGraphStore::open(&path).unwrap();
            assert_eq!(store.node_count().unwrap(), 0);
            assert_eq!(store.edge_count().unwrap(), 0);
        }
        let store = SqliteGraphStore::open(&path).unwrap();
        assert_eq!(store.node_count().unwrap(), 0);
        assert_eq!(store.edge_count().unwrap(), 0);
    }

    #[test]
    fn nodes_and_edges_persist_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        {
            let mut store = SqliteGraphStore::open(&path).unwrap();
            store.put_node(&node("sym:a", "Symbol")).unwrap();
            store.put_node(&node("sym:b", "Symbol")).unwrap();
            store.put_edge(&edge("sym:a", "sym:b", "CALLS")).unwrap();
        }
        let store = SqliteGraphStore::open(&path).unwrap();
        assert_eq!(store.node_count().unwrap(), 2);
        assert_eq!(store.edge_count().unwrap(), 1);
        assert_eq!(store.get_node("sym:a").unwrap().unwrap().label, "Symbol");
    }

    #[test]
    fn put_is_idempotent_by_key() {
        // Re-ingesting the same fact must not duplicate it (US-0014 groundwork).
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        for _ in 0..3 {
            store.put_node(&node("n1", "File")).unwrap();
            store.put_node(&node("n2", "File")).unwrap();
            store.put_edge(&edge("n1", "n2", "IMPORTS")).unwrap();
        }
        assert_eq!(store.node_count().unwrap(), 2);
        assert_eq!(store.edge_count().unwrap(), 1);
    }

    #[test]
    fn nodes_with_label_filters_and_orders() {
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        store.put_node(&node("ep:b", "Endpoint")).unwrap();
        store.put_node(&node("ep:a", "Endpoint")).unwrap();
        store.put_node(&node("f1", "File")).unwrap();
        let eps = store.nodes_with_label("Endpoint").unwrap();
        let ids: Vec<_> = eps.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["ep:a", "ep:b"]);
    }

    #[test]
    fn recursive_cte_traversal_follows_labels_and_survives_cycles() {
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        for id in ["a", "b", "c", "d"] {
            store.put_node(&node(id, "Symbol")).unwrap();
        }
        store.put_edge(&edge("a", "b", "CALLS")).unwrap();
        store.put_edge(&edge("b", "c", "CALLS")).unwrap();
        store.put_edge(&edge("c", "a", "CALLS")).unwrap(); // cycle
        store.put_edge(&edge("b", "d", "IMPORTS")).unwrap(); // different label

        let calls = store.reachable_from("a", Some("CALLS")).unwrap();
        assert_eq!(calls, vec!["a", "b", "c"]); // cycle returns to a, terminates
        let all = store.reachable_from("a", None).unwrap();
        assert_eq!(all, vec!["a", "b", "c", "d"]);
    }
}
