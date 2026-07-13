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
    /// Every node, ordered by stable id (Atlas/read-only export surfaces).
    fn all_nodes(&self) -> Result<Vec<Node>, GraphError>;
    /// Every edge, ordered by (src, dst, label).
    fn all_edges(&self) -> Result<Vec<Edge>, GraphError>;
    /// All nodes carrying `label`, ordered by id.
    fn nodes_with_label(&self, label: &str) -> Result<Vec<Node>, GraphError>;
    /// All edges whose label is one of `labels`, ordered by (src, dst, label).
    fn edges_with_labels(&self, labels: &[&str]) -> Result<Vec<Edge>, GraphError>;
    /// Delete every graph fact while preserving the store itself.
    fn clear(&mut self) -> Result<(), GraphError>;
}

/// Version of the graph's fact schema — the node/edge *id scheme*, not the
/// SQL shape. Bumped when ids change meaning (v2: repo-namespaced ids,
/// US-0001 slice 2). A mismatched db is cleared on open: the graph is a
/// disposable ingest artifact (ADR-0008), and stale-scheme rows can never
/// be upserted again — they would shadow every re-ingest as zombies (#50).
pub const GRAPH_SCHEMA_VERSION: u32 = 2;

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
        let version: u32 = conn.query_row("SELECT * FROM pragma_user_version", [], |r| r.get(0))?;
        if version != GRAPH_SCHEMA_VERSION {
            // Pre-versioned or older-scheme db: clear the facts, keep the
            // shape. Deletion order respects the edge → node foreign keys.
            conn.execute_batch("DELETE FROM edges; DELETE FROM nodes;")?;
            conn.pragma_update(None, "user_version", GRAPH_SCHEMA_VERSION)?;
        }
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

    fn all_nodes(&self) -> Result<Vec<Node>, GraphError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, label, props FROM nodes ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
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

    fn all_edges(&self) -> Result<Vec<Edge>, GraphError> {
        let mut stmt = self
            .conn
            .prepare("SELECT src, dst, label, props FROM edges ORDER BY src, dst, label")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut edges = Vec::new();
        for row in rows {
            let (src, dst, label, props) = row?;
            edges.push(Edge {
                src,
                dst,
                label,
                props: serde_json::from_str(&props)?,
            });
        }
        Ok(edges)
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

    fn edges_with_labels(&self, labels: &[&str]) -> Result<Vec<Edge>, GraphError> {
        if labels.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; labels.len()].join(",");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT src, dst, label, props FROM edges
             WHERE label IN ({placeholders}) ORDER BY src, dst, label"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(labels), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut edges = Vec::new();
        for row in rows {
            let (src, dst, label, props) = row?;
            edges.push(Edge {
                src,
                dst,
                label,
                props: serde_json::from_str(&props)?,
            });
        }
        Ok(edges)
    }

    fn clear(&mut self) -> Result<(), GraphError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM edges", [])?;
        tx.execute("DELETE FROM nodes", [])?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // #50: an older id-scheme db is cleared on open (zombie rows from a
    // previous scheme can never be upserted and would shadow re-ingests);
    // a current-version db keeps its facts.
    #[test]
    fn version_mismatch_clears_the_graph_current_version_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        {
            let mut store = SqliteGraphStore::open(&path).unwrap();
            store
                .put_node(&Node {
                    id: "sym:acme/shop@a.ts#f".into(),
                    label: "Symbol".into(),
                    props: serde_json::json!({}),
                })
                .unwrap();
        }
        // Same version: facts survive reopen.
        {
            let store = SqliteGraphStore::open(&path).unwrap();
            assert_eq!(store.node_count().unwrap(), 1);
        }
        // Simulate a db written by an older scheme.
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", GRAPH_SCHEMA_VERSION - 1)
                .unwrap();
        }
        let store = SqliteGraphStore::open(&path).unwrap();
        assert_eq!(store.node_count().unwrap(), 0, "stale-scheme facts cleared");
        assert_eq!(store.edge_count().unwrap(), 0);
    }

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
    fn clear_removes_graph_facts_and_persists_empty() {
        // AC-0050: graph facts are disposable and can be cleared without
        // replacing/corrupting the graph database.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        {
            let mut store = SqliteGraphStore::open(&path).unwrap();
            store.put_node(&node("a", "Resource")).unwrap();
            store.put_node(&node("b", "Resource")).unwrap();
            store.put_edge(&edge("a", "b", "REFERENCES")).unwrap();
            store.clear().unwrap();
            assert_eq!(store.node_count().unwrap(), 0);
            assert_eq!(store.edge_count().unwrap(), 0);
        }
        let store = SqliteGraphStore::open(&path).unwrap();
        assert_eq!(store.node_count().unwrap(), 0);
        assert_eq!(store.edge_count().unwrap(), 0);
    }

    #[test]
    fn edges_with_labels_filters_and_orders() {
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        for id in ["a", "b", "c"] {
            store.put_node(&node(id, "Resource")).unwrap();
        }
        store.put_edge(&edge("b", "c", "TRIGGERS")).unwrap();
        store.put_edge(&edge("a", "b", "REFERENCES")).unwrap();
        store.put_edge(&edge("a", "c", "CALLS")).unwrap();
        let got = store
            .edges_with_labels(&["TRIGGERS", "REFERENCES"])
            .unwrap();
        let pairs: Vec<_> = got
            .iter()
            .map(|e| (e.src.as_str(), e.label.as_str()))
            .collect();
        assert_eq!(pairs, vec![("a", "REFERENCES"), ("b", "TRIGGERS")]);
        assert!(store.edges_with_labels(&[]).unwrap().is_empty());
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
    fn all_facts_are_ordered_for_atlas_snapshot() {
        // AC-0026: the Atlas receives one deterministic whole-graph snapshot;
        // filtering never depends on SQLite insertion order.
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        store.put_node(&node("z", "Channel")).unwrap();
        store.put_node(&node("a", "Resource")).unwrap();
        store.put_node(&node("m", "Gap")).unwrap();
        store.put_edge(&edge("z", "m", "PUBLISHES")).unwrap();
        store.put_edge(&edge("a", "z", "BACKS")).unwrap();

        let node_ids: Vec<_> = store
            .all_nodes()
            .unwrap()
            .into_iter()
            .map(|node| node.id)
            .collect();
        assert_eq!(node_ids, ["a", "m", "z"]);
        let edge_labels: Vec<_> = store
            .all_edges()
            .unwrap()
            .into_iter()
            .map(|edge| edge.label)
            .collect();
        assert_eq!(edge_labels, ["BACKS", "PUBLISHES"]);
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
