//! Unified knowledge-graph store (SPEC-00 §4).
//!
//! [`GraphStore`] is the storage abstraction; the primary implementation is
//! SQLite/WAL with recursive-CTE traversal ([`SqliteGraphStore`]), per
//! ADR-0008 (Kuzu was archived upstream at the M0 verify-at-build). A future
//! embedded-graph-engine adapter implements the same trait if the OQ-3
//! benchmark ever demands it.

pub mod rules;
pub mod source;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
#[cfg(any(test, feature = "test-support"))]
use std::cell::RefCell;
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

/// A scoped set of graph changes published against an expected full snapshot.
/// Deletions precede upserts, so an id can be removed and then recreated.
#[derive(Debug, Clone, Default)]
pub struct GraphPatch {
    /// Nodes inserted or updated by stable id.
    pub upsert_nodes: Vec<Node>,
    /// Edges inserted or updated by `(src, dst, label)`.
    pub upsert_edges: Vec<Edge>,
    /// Nodes to remove together with all their incident edges.
    pub delete_node_ids: Vec<String>,
    /// Individual edge keys `(src, dst, label)` to remove.
    pub delete_edges: Vec<(String, String, String)>,
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
    /// Invalid or incompatible current-source association metadata.
    #[error("source binding: {0}")]
    SourceBinding(&'static str),
}

/// Storage abstraction for the unified graph (ADR-0008).
pub trait GraphStore {
    /// Insert or replace a node by id.
    fn put_node(&mut self, node: &Node) -> Result<(), GraphError>;
    /// Insert or replace an edge by (src, dst, label).
    fn put_edge(&mut self, edge: &Edge) -> Result<(), GraphError>;
    /// Delete outgoing edges from `src` carrying `label`.
    fn delete_edges_from_with_label(&mut self, src: &str, label: &str) -> Result<(), GraphError>;
    /// Delete one edge by its stable `(src, dst, label)` key.
    fn delete_edge(&mut self, src: &str, dst: &str, label: &str) -> Result<(), GraphError>;
    /// Delete a node and all of its incident edges.
    fn delete_node(&mut self, id: &str) -> Result<(), GraphError>;
    /// Fetch a node by id.
    fn get_node(&self, id: &str) -> Result<Option<Node>, GraphError>;
    /// Number of nodes.
    fn node_count(&self) -> Result<u64, GraphError>;
    /// Number of edges.
    fn edge_count(&self) -> Result<u64, GraphError>;
    /// Node and edge counts from one database revision, without loading facts.
    fn fact_counts(&self) -> Result<(u64, u64), GraphError>;
    /// Owned nodes and edges from one database revision, ordered by node id
    /// and edge (src, dst, label). The read transaction ends before return.
    /// Implementations must not compose unrelated autocommit reads.
    fn read_snapshot(&self) -> Result<(Vec<Node>, Vec<Edge>), GraphError>;
    /// One coherent snapshot with independent exact-label selections. `None`
    /// selects all labels; `Some(&[])` selects no facts of that kind. Filtering
    /// precedes property decoding, so unselected malformed JSON is ignored.
    /// Results retain the same ordering and transaction lifetime as a full read.
    fn read_snapshot_filtered(
        &self,
        node_labels: Option<&[&str]>,
        edge_labels: Option<&[&str]>,
    ) -> Result<(Vec<Node>, Vec<Edge>), GraphError>;
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
/// US-0001 slice 2; v3: scope-qualified callable identities, AC-0120;
/// v4: registered source namespaces and root-free Repo facts, AC-0144/0146).
/// A mismatched db is cleared on open: the graph is a
/// disposable ingest artifact (ADR-0008), and stale-scheme rows can never
/// be upserted again — they would shadow every re-ingest as zombies (#50).
pub const GRAPH_SCHEMA_VERSION: u32 = 4;

/// SQLite/WAL implementation — node/edge tables + recursive-CTE traversal.
pub struct SqliteGraphStore {
    conn: Connection,
    #[cfg(any(test, feature = "test-support"))]
    snapshot_after_nodes: RefCell<Option<SnapshotAfterNodesHook>>,
    #[cfg(any(test, feature = "test-support"))]
    source_binding_after_lookup: RefCell<Option<SnapshotAfterNodesHook>>,
}

#[cfg(any(test, feature = "test-support"))]
type SnapshotAfterNodesHook = Box<dyn FnOnce() -> Result<(), GraphError> + Send>;

impl SqliteGraphStore {
    /// Open (creating if absent) a graph database at `path`, in WAL mode.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GraphError> {
        Self::init(Connection::open(path)?)
    }

    /// Open an in-memory graph (tests, scratch analysis).
    pub fn open_in_memory() -> Result<Self, GraphError> {
        Self::init(Connection::open_in_memory()?)
    }

    /// Copy nodes and edges from one SQLite read snapshot, including when
    /// another connection or process commits during the copy. The transaction
    /// ends before the owned facts return for expensive downstream analysis.
    pub fn read_snapshot(&self) -> Result<(Vec<Node>, Vec<Edge>), GraphError> {
        <Self as GraphStore>::read_snapshot(self)
    }

    /// Atomically validate an ordered full [`Self::read_snapshot`] and apply a
    /// scoped patch. An immediate transaction excludes competing writers before
    /// validation and until commit. A stale snapshot returns `false` without
    /// changes; any read, serialization or write failure rolls back the patch.
    /// Facts absent from the patch remain intact, except edges incident to a
    /// deleted node. This does not make earlier ingestion writes atomic.
    pub fn apply_patch_if_snapshot_matches(
        &mut self,
        expected: &(Vec<Node>, Vec<Edge>),
        patch: &GraphPatch,
    ) -> Result<bool, GraphError> {
        // Shared connection borrowing lets the ordinary private readers/writers
        // run inside this transaction; nested transactions still fail at runtime.
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        if &self.read_snapshot_rows(None, None)? != expected {
            return Ok(false);
        }
        self.apply_patch_rows(patch)?;
        transaction.commit()?;
        Ok(true)
    }

    /// Apply scoped changes inside the caller's transaction.
    fn apply_patch_rows(&self, patch: &GraphPatch) -> Result<(), GraphError> {
        for (src, dst, label) in &patch.delete_edges {
            self.conn.execute(
                "DELETE FROM edges WHERE src = ?1 AND dst = ?2 AND label = ?3",
                params![src, dst, label],
            )?;
        }
        for id in &patch.delete_node_ids {
            self.conn
                .execute("DELETE FROM edges WHERE src = ?1 OR dst = ?1", params![id])?;
            self.conn
                .execute("DELETE FROM nodes WHERE id = ?1", params![id])?;
        }
        for node in &patch.upsert_nodes {
            self.write_node(node)?;
        }
        for edge in &patch.upsert_edges {
            self.write_edge(edge)?;
        }
        Ok(())
    }

    /// Install a one-shot interleaving hook for regression tests. The ordinary
    /// snapshot and conditional-patch paths consume it after the node query,
    /// while their transaction is still open, and propagate errors with normal
    /// transaction cleanup.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn set_snapshot_after_nodes_hook(
        &self,
        hook: impl FnOnce() -> Result<(), GraphError> + Send + 'static,
    ) {
        *self.snapshot_after_nodes.borrow_mut() = Some(Box::new(hook));
    }

    /// Read through the active transaction owned by the public operation.
    fn read_snapshot_rows(
        &self,
        node_labels: Option<&[&str]>,
        edge_labels: Option<&[&str]>,
    ) -> Result<(Vec<Node>, Vec<Edge>), GraphError> {
        let nodes = self.read_nodes(node_labels)?;
        #[cfg(any(test, feature = "test-support"))]
        {
            // Release the RefCell borrow before calling user-supplied test code.
            let after_nodes = self.snapshot_after_nodes.borrow_mut().take();
            if let Some(after_nodes) = after_nodes {
                after_nodes()?;
            }
        }
        let edges = self.read_edges(edge_labels)?;
        Ok((nodes, edges))
    }

    fn read_nodes(&self, labels: Option<&[&str]>) -> Result<Vec<Node>, GraphError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, label, props FROM nodes {} ORDER BY id",
            label_selection(labels),
        ))?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(labels.unwrap_or_default()),
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )?;
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

    fn read_edges(&self, labels: Option<&[&str]>) -> Result<Vec<Edge>, GraphError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT src, dst, label, props FROM edges {} ORDER BY src, dst, label",
            label_selection(labels),
        ))?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(labels.unwrap_or_default()),
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )?;
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

    fn write_node(&self, node: &Node) -> Result<(), GraphError> {
        self.conn.execute(
            "INSERT INTO nodes (id, label, props) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET label = excluded.label, props = excluded.props",
            params![node.id, node.label, serde_json::to_string(&node.props)?],
        )?;
        Ok(())
    }

    fn write_edge(&self, edge: &Edge) -> Result<(), GraphError> {
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
        // Private association metadata has its own schema version. Validate it
        // before any legacy fact-schema rebuild can remove existing facts.
        source::initialize(&conn)?;
        let version: u32 = conn.query_row("SELECT * FROM pragma_user_version", [], |r| r.get(0))?;
        if version != GRAPH_SCHEMA_VERSION {
            // Pre-versioned or older-scheme db: clear the facts, keep the
            // shape. Deletion order respects the edge → node foreign keys.
            conn.execute_batch("DELETE FROM edges; DELETE FROM nodes;")?;
            conn.pragma_update(None, "user_version", GRAPH_SCHEMA_VERSION)?;
        }
        Ok(Self {
            conn,
            #[cfg(any(test, feature = "test-support"))]
            snapshot_after_nodes: RefCell::new(None),
            #[cfg(any(test, feature = "test-support"))]
            source_binding_after_lookup: RefCell::new(None),
        })
    }
}

fn label_selection(labels: Option<&[&str]>) -> String {
    match labels {
        None => String::new(),
        // Execute the node query even for no selected labels: its read
        // transaction must be established before the edge query and test hook.
        Some([]) => "WHERE 0".into(),
        Some(labels) => format!("WHERE label IN ({})", vec!["?"; labels.len()].join(",")),
    }
}

impl GraphStore for SqliteGraphStore {
    fn put_node(&mut self, node: &Node) -> Result<(), GraphError> {
        self.write_node(node)
    }

    fn put_edge(&mut self, edge: &Edge) -> Result<(), GraphError> {
        self.write_edge(edge)
    }

    fn delete_edges_from_with_label(&mut self, src: &str, label: &str) -> Result<(), GraphError> {
        self.conn.execute(
            "DELETE FROM edges WHERE src = ?1 AND label = ?2",
            params![src, label],
        )?;
        Ok(())
    }

    fn delete_edge(&mut self, src: &str, dst: &str, label: &str) -> Result<(), GraphError> {
        self.conn.execute(
            "DELETE FROM edges WHERE src = ?1 AND dst = ?2 AND label = ?3",
            params![src, dst, label],
        )?;
        Ok(())
    }

    fn delete_node(&mut self, id: &str) -> Result<(), GraphError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM edges WHERE src = ?1 OR dst = ?1", params![id])?;
        tx.execute("DELETE FROM nodes WHERE id = ?1", params![id])?;
        tx.commit()?;
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

    fn fact_counts(&self) -> Result<(u64, u64), GraphError> {
        // Scalar subqueries in one statement share a SQLite read revision.
        // Do not decode or materialize JSON merely to count retained rows.
        Ok(self.conn.query_row(
            "SELECT (SELECT COUNT(*) FROM nodes), (SELECT COUNT(*) FROM edges)",
            [],
            |row| {
                let nodes: i64 = row.get(0)?;
                let edges: i64 = row.get(1)?;
                Ok((
                    u64::try_from(nodes)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, nodes))?,
                    u64::try_from(edges)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, edges))?,
                ))
            },
        )?)
    }

    fn read_snapshot(&self) -> Result<(Vec<Node>, Vec<Edge>), GraphError> {
        self.read_snapshot_filtered(None, None)
    }

    fn read_snapshot_filtered(
        &self,
        node_labels: Option<&[&str]>,
        edge_labels: Option<&[&str]>,
    ) -> Result<(Vec<Node>, Vec<Edge>), GraphError> {
        // The connection is not shared across threads; `unchecked_transaction`
        // permits a read-only &self API and rejects nested transactions at
        // runtime. RAII rollback releases the snapshot on any failure.
        let transaction = self.conn.unchecked_transaction()?;
        let snapshot = self.read_snapshot_rows(node_labels, edge_labels)?;
        transaction.commit()?;
        Ok(snapshot)
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
        self.read_nodes(None)
    }

    fn all_edges(&self) -> Result<Vec<Edge>, GraphError> {
        self.read_edges(None)
    }

    fn nodes_with_label(&self, label: &str) -> Result<Vec<Node>, GraphError> {
        self.read_nodes(Some(&[label]))
    }

    fn edges_with_labels(&self, labels: &[&str]) -> Result<Vec<Edge>, GraphError> {
        self.read_edges(Some(labels))
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

    #[test]
    fn conditional_patch_updates_only_requested_facts_and_incident_edges() {
        // AC-0142, AC-0147: ADR publication is a scoped patch; unrelated facts
        // survive and every incoming/outgoing edge of a deleted node is removed.
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        for id in ["a", "b", "c", "d"] {
            store.put_node(&node(id, "Symbol")).unwrap();
        }
        for (src, dst, label) in [
            ("a", "b", "DECIDES"),
            ("b", "c", "REFERENCES"),
            ("a", "c", "DECIDES"),
            ("c", "a", "REFERENCES"),
            ("c", "d", "CALLS"),
        ] {
            store.put_edge(&edge(src, dst, label)).unwrap();
        }
        let expected = store.read_snapshot().unwrap();
        let mut updated_node = node("a", "ADR");
        updated_node.props = serde_json::json!({ "title": "Recovered decision" });
        let mut updated_edge = edge("c", "d", "CALLS");
        updated_edge.props = serde_json::json!({ "evidence": "retained" });
        let patch = GraphPatch {
            delete_edges: vec![("a".into(), "c".into(), "DECIDES".into())],
            delete_node_ids: vec!["b".into()],
            upsert_nodes: vec![updated_node.clone(), node("e", "Symbol")],
            upsert_edges: vec![edge("a", "e", "DECIDES"), updated_edge.clone()],
        };
        assert!(
            store
                .apply_patch_if_snapshot_matches(&expected, &patch)
                .unwrap()
        );
        assert!(store.conn.is_autocommit());
        assert_eq!(
            store.read_snapshot().unwrap(),
            (
                vec![
                    updated_node,
                    node("c", "Symbol"),
                    node("d", "Symbol"),
                    node("e", "Symbol")
                ],
                vec![
                    edge("a", "e", "DECIDES"),
                    edge("c", "a", "REFERENCES"),
                    updated_edge
                ],
            )
        );
    }

    #[test]
    fn conditional_patch_rejects_stale_node_or_edge_snapshot_without_writes() {
        // AC-0142, AC-0147: another connection can change only properties,
        // without changing fact counts; neither a stale node nor edge is accepted.
        for change_edge in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("graph.db");
            let mut store = SqliteGraphStore::open(&path).unwrap();
            store.put_node(&node("a", "Symbol")).unwrap();
            store.put_node(&node("b", "Symbol")).unwrap();
            store.put_edge(&edge("a", "b", "CALLS")).unwrap();
            let expected = store.read_snapshot().unwrap();
            let mut writer = SqliteGraphStore::open(&path).unwrap();
            if change_edge {
                let mut changed = edge("a", "b", "CALLS");
                changed.props = serde_json::json!({ "revision": "new" });
                writer.put_edge(&changed).unwrap();
            } else {
                let mut changed = node("a", "Symbol");
                changed.props = serde_json::json!({ "revision": "new" });
                writer.put_node(&changed).unwrap();
            }
            let current = writer.read_snapshot().unwrap();
            let patch = GraphPatch {
                delete_edges: vec![("a".into(), "b".into(), "CALLS".into())],
                delete_node_ids: vec!["b".into()],
                upsert_nodes: vec![node("adr", "ADR")],
                // Validation must reject stale input before even this bad write.
                upsert_edges: vec![edge("adr", "missing", "DECIDES")],
            };
            assert!(
                !store
                    .apply_patch_if_snapshot_matches(&expected, &patch)
                    .unwrap()
            );
            assert!(store.conn.is_autocommit());
            assert_eq!(store.read_snapshot().unwrap(), current);
            writer.put_node(&node("later", "Symbol")).unwrap();
        }
    }

    #[test]
    fn conditional_patch_rolls_back_deletes_and_upserts_on_invalid_edge() {
        // AC-0142, AC-0147: a late foreign-key failure rolls back explicit and
        // incident-edge deletions, node deletion, updates and earlier inserts.
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        for id in ["a", "b", "c"] {
            store.put_node(&node(id, "Symbol")).unwrap();
        }
        for (src, dst) in [("a", "b"), ("b", "c"), ("c", "a")] {
            store.put_edge(&edge(src, dst, "CALLS")).unwrap();
        }
        let expected = store.read_snapshot().unwrap();
        let patch = GraphPatch {
            delete_edges: vec![("c".into(), "a".into(), "CALLS".into())],
            delete_node_ids: vec!["b".into()],
            upsert_nodes: vec![node("a", "ADR"), node("new", "Symbol")],
            upsert_edges: vec![edge("a", "new", "DECIDES"), edge("a", "missing", "DECIDES")],
        };
        let error = store
            .apply_patch_if_snapshot_matches(&expected, &patch)
            .unwrap_err();
        assert!(matches!(
            error,
            GraphError::Storage(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::ConstraintViolation
        ));
        assert!(store.conn.is_autocommit());
        assert_eq!(store.read_snapshot().unwrap(), expected);
        assert!(
            store
                .apply_patch_if_snapshot_matches(&expected, &GraphPatch::default())
                .unwrap()
        );
    }

    #[test]
    fn conditional_patch_invalid_properties_release_immediate_transaction() {
        // AC-0142, AC-0147: malformed persisted node/edge JSON fails closed and
        // releases the writer reservation so another connection can repair it.
        for table in ["nodes", "edges"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("graph.db");
            let mut store = SqliteGraphStore::open(&path).unwrap();
            store.put_node(&node("a", "Symbol")).unwrap();
            store.put_edge(&edge("a", "a", "CALLS")).unwrap();
            let expected = store.read_snapshot().unwrap();
            let writer = SqliteGraphStore::open(&path).unwrap();
            writer.conn.busy_timeout(std::time::Duration::ZERO).unwrap();
            writer
                .conn
                .execute(&format!("UPDATE {table} SET props = 'invalid'"), [])
                .unwrap();
            let patch = GraphPatch {
                upsert_nodes: vec![node("adr", "ADR")],
                ..GraphPatch::default()
            };
            assert!(matches!(
                store.apply_patch_if_snapshot_matches(&expected, &patch),
                Err(GraphError::Props(_))
            ));
            assert!(store.conn.is_autocommit());
            writer
                .conn
                .execute(&format!("UPDATE {table} SET props = '{{}}'"), [])
                .unwrap();
            assert_eq!(writer.read_snapshot().unwrap(), expected);
            assert!(
                store
                    .apply_patch_if_snapshot_matches(&expected, &patch)
                    .unwrap()
            );
            assert!(writer.get_node("adr").unwrap().is_some());
        }
    }

    #[test]
    fn conditional_patch_excludes_competing_writer_until_commit() {
        // AC-0142, AC-0147: attempt a second-connection write synchronously after
        // the node read. It must fail busy before validation/publication finishes;
        // no timer, process mutex or optimistic gap between check and write helps.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        let mut store = SqliteGraphStore::open(&path).unwrap();
        store.put_node(&node("a", "Symbol")).unwrap();
        store.put_edge(&edge("a", "a", "CALLS")).unwrap();
        let expected = store.read_snapshot().unwrap();
        let before = expected.clone();
        let mut writer = SqliteGraphStore::open(&path).unwrap();
        writer.conn.busy_timeout(std::time::Duration::ZERO).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        store.set_snapshot_after_nodes_hook(move || {
            // WAL readers continue to observe the committed graph.
            assert_eq!(writer.read_snapshot()?, before);
            let error = writer.put_node(&node("competitor", "Symbol")).unwrap_err();
            assert!(matches!(
                error,
                GraphError::Storage(rusqlite::Error::SqliteFailure(error, _))
                    if error.code == rusqlite::ErrorCode::DatabaseBusy
            ));
            sender.send(writer).unwrap();
            Ok(())
        });
        let patch = GraphPatch {
            upsert_nodes: vec![node("adr", "ADR")],
            upsert_edges: vec![edge("adr", "a", "DECIDES")],
            ..GraphPatch::default()
        };
        assert!(
            store
                .apply_patch_if_snapshot_matches(&expected, &patch)
                .unwrap()
        );
        assert!(store.conn.is_autocommit());
        let mut writer = receiver.try_recv().unwrap();
        assert_eq!(
            writer.read_snapshot().unwrap(),
            store.read_snapshot().unwrap()
        );
        writer.put_node(&node("competitor", "Symbol")).unwrap();
        assert!(store.get_node("competitor").unwrap().is_some());
        assert!(store.get_node("adr").unwrap().is_some());
    }

    #[test]
    fn read_snapshot_keeps_one_sqlite_revision_across_another_connection_commit() {
        // AC-0105, AC-0137: a process-local mutex cannot exclude another process.
        // Commit through a second connection exactly between the two reads.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        let mut reader = SqliteGraphStore::open(&path).unwrap();
        reader.put_node(&node("a", "Symbol")).unwrap();
        reader.put_edge(&edge("a", "a", "CALLS")).unwrap();
        let mut writer = SqliteGraphStore::open(&path).unwrap();
        let before = reader.read_snapshot().unwrap();
        reader.set_snapshot_after_nodes_hook(move || replace_graph(&mut writer, "b"));
        // Exercise the required trait API rather than a test-only read helper.
        let during = GraphStore::read_snapshot(&reader).unwrap();
        assert_eq!(during, before);
        let after = reader.read_snapshot().unwrap();
        assert_eq!(after.0, vec![node("b", "Symbol")]);
        assert_eq!(after.1, vec![edge("b", "b", "CALLS")]);
        assert_ne!(after, before);
    }

    fn replace_graph(store: &mut SqliteGraphStore, id: &str) -> Result<(), GraphError> {
        let tx = store.conn.transaction()?;
        tx.execute("DELETE FROM edges", [])?;
        tx.execute("DELETE FROM nodes", [])?;
        tx.execute("INSERT INTO nodes (id, label) VALUES (?1, 'Symbol')", [id])?;
        tx.execute(
            "INSERT INTO edges (src, dst, label) VALUES (?1, ?1, 'CALLS')",
            [id],
        )?;
        tx.commit()?;
        Ok(())
    }

    #[test]
    fn filtered_snapshot_keeps_one_revision_even_with_no_selected_nodes() {
        // AC-0137: filtered and explicitly empty node queries must pin the
        // revision before another connection replaces the selected edge set.
        for labels in [&["Symbol"][..], &[][..], &["Absent"][..]] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("graph.db");
            let mut reader = SqliteGraphStore::open(&path).unwrap();
            replace_graph(&mut reader, "a").unwrap();
            let mut writer = SqliteGraphStore::open(&path).unwrap();
            let before = reader
                .read_snapshot_filtered(Some(labels), Some(&["CALLS"]))
                .unwrap();
            reader.set_snapshot_after_nodes_hook(move || replace_graph(&mut writer, "b"));
            let during = reader
                .read_snapshot_filtered(Some(labels), Some(&["CALLS"]))
                .unwrap();
            assert_eq!(during, before);
            assert!(reader.conn.is_autocommit());
            let after = reader
                .read_snapshot_filtered(Some(labels), Some(&["CALLS"]))
                .unwrap();
            assert_eq!(after.1, vec![edge("b", "b", "CALLS")]);
            assert_ne!(after, before);
        }
    }

    #[test]
    fn filtered_snapshot_preserves_selection_order_and_unselected_malformed_props() {
        // AC-0137: exact independent label selection happens in SQL before
        // JSON decoding; duplicates/input order do not duplicate or reorder facts.
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        for (id, label) in [
            ("z", "File"),
            ("b", "Symbol"),
            ("a", "File"),
            ("bad", "Other"),
        ] {
            store.put_node(&node(id, label)).unwrap();
        }
        for (src, dst, label) in [
            ("b", "z", "CALLS"),
            ("a", "z", "IMPORTS"),
            ("a", "b", "IMPORTS"),
            ("a", "b", "CALLS"),
            ("bad", "a", "OTHER"),
        ] {
            store.put_edge(&edge(src, dst, label)).unwrap();
        }
        store
            .conn
            .execute("UPDATE nodes SET props='invalid' WHERE label='Other'", [])
            .unwrap();
        store
            .conn
            .execute("UPDATE edges SET props='invalid' WHERE label='OTHER'", [])
            .unwrap();
        let selected = store
            .read_snapshot_filtered(
                Some(&["Symbol", "File", "File"]),
                Some(&["IMPORTS", "CALLS", "IMPORTS"]),
            )
            .unwrap();
        assert_eq!(
            selected.0,
            vec![node("a", "File"), node("b", "Symbol"), node("z", "File")]
        );
        assert_eq!(
            selected.1,
            vec![
                edge("a", "b", "CALLS"),
                edge("a", "b", "IMPORTS"),
                edge("a", "z", "IMPORTS"),
                edge("b", "z", "CALLS"),
            ]
        );
        assert_eq!(
            store.read_snapshot_filtered(Some(&[]), Some(&[])).unwrap(),
            (vec![], vec![])
        );
        assert_eq!(
            store
                .read_snapshot_filtered(Some(&["File"]), Some(&[]))
                .unwrap()
                .0,
            vec![node("a", "File"), node("z", "File")]
        );
        assert_eq!(
            store
                .read_snapshot_filtered(Some(&[]), Some(&["CALLS"]))
                .unwrap()
                .1,
            vec![edge("a", "b", "CALLS"), edge("b", "z", "CALLS")]
        );
        assert_eq!(
            store
                .read_snapshot_filtered(Some(&["File' OR 1=1 --"]), Some(&["calls"]))
                .unwrap(),
            (vec![], vec![])
        );
        assert!(store.read_snapshot_filtered(None, Some(&[])).is_err());
        assert!(store.read_snapshot_filtered(Some(&[]), None).is_err());
        assert!(store.read_snapshot().is_err());
        assert!(store.conn.is_autocommit());
    }

    #[test]
    fn snapshot_hook_is_one_shot_and_failure_releases_transaction() {
        // AC-0139: test interleaving uses the production path, propagates errors
        // and releases both the transaction and the one-shot callback on failure.
        fn assert_send<T: Send>() {}
        assert_send::<SqliteGraphStore>();
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        store.put_node(&node("a", "Symbol")).unwrap();
        store.set_snapshot_after_nodes_hook(|| {
            Err(GraphError::Storage(rusqlite::Error::InvalidQuery))
        });
        assert!(GraphStore::read_snapshot(&store).is_err());
        assert!(store.conn.is_autocommit());
        store.put_node(&node("b", "Symbol")).unwrap();
        let after = store.read_snapshot().unwrap();
        assert_eq!(after.0, vec![node("a", "Symbol"), node("b", "Symbol")]);
        assert!(store.conn.is_autocommit());
    }

    #[test]
    fn fact_counts_ignore_malformed_properties_and_match_retained_rows() {
        // AC-0137: the paired count is a single SQL statement with no JSON
        // materialization; invalid node/edge props still count as retained facts.
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        assert_eq!(store.fact_counts().unwrap(), (0, 0));
        store.put_node(&node("a", "Symbol")).unwrap();
        store.put_node(&node("b", "Symbol")).unwrap();
        store.put_edge(&edge("a", "b", "CALLS")).unwrap();
        store
            .conn
            .execute("UPDATE nodes SET props='invalid'", [])
            .unwrap();
        store
            .conn
            .execute("UPDATE edges SET props='invalid'", [])
            .unwrap();
        assert_eq!(store.fact_counts().unwrap(), (2, 1));
        assert!(store.read_snapshot().is_err());
        assert!(store.conn.is_autocommit());
        store.clear().unwrap();
        assert_eq!(store.fact_counts().unwrap(), (0, 0));
    }

    #[test]
    fn failed_read_snapshot_releases_the_transaction() {
        // AC-0105, AC-0139: malformed properties must release the read snapshot
        // on failure so a later corrected read can observe current data.
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        store.put_node(&node("a", "Symbol")).unwrap();
        store.put_edge(&edge("a", "a", "CALLS")).unwrap();
        store
            .conn
            .execute("UPDATE edges SET props = 'invalid'", [])
            .unwrap();
        assert!(store.read_snapshot().is_err());
        assert!(store.conn.is_autocommit());
        store
            .conn
            .execute("UPDATE edges SET props = '{}'", [])
            .unwrap();
        assert_eq!(store.read_snapshot().unwrap().1.len(), 1);
        store
            .conn
            .execute("UPDATE nodes SET props = 'invalid'", [])
            .unwrap();
        assert!(store.read_snapshot().is_err());
        assert!(store.conn.is_autocommit());
        store
            .conn
            .execute("UPDATE nodes SET props = '{}'", [])
            .unwrap();
        store.put_node(&node("b", "Symbol")).unwrap();
        let repaired = store
            .read_snapshot_filtered(Some(&["Symbol"]), Some(&["CALLS"]))
            .unwrap();
        assert_eq!(repaired.0, vec![node("a", "Symbol"), node("b", "Symbol")]);
        assert_eq!(repaired.1, vec![edge("a", "a", "CALLS")]);
        assert!(store.conn.is_autocommit());
    }

    // #50: an older id-scheme db is cleared on open (zombie rows from a
    // previous scheme can never be upserted and would shadow re-ingests);
    // a current-version db keeps its facts.
    #[test]
    fn version_mismatch_clears_the_graph_current_version_persists() {
        // AC-0120: old file-wide ownership links must disappear on upgrade,
        // before users have individually re-ingested their repositories.
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
            store
                .put_node(&node("sym:acme/shop@a.ts#g", "Symbol"))
                .unwrap();
            store
                .put_edge(&edge(
                    "sym:acme/shop@a.ts#f",
                    "sym:acme/shop@a.ts#g",
                    "CALLS",
                ))
                .unwrap();
        }
        // Same version: facts survive reopen.
        {
            let store = SqliteGraphStore::open(&path).unwrap();
            assert_eq!(store.node_count().unwrap(), 2);
            assert_eq!(store.edge_count().unwrap(), 1);
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
    fn scoped_deletes_remove_owned_edges_and_incident_node_edges() {
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        for id in ["adr:a", "adr:b", "target"] {
            store.put_node(&node(id, "Resource")).unwrap();
        }
        store.put_edge(&edge("adr:a", "target", "DECIDES")).unwrap();
        store
            .put_edge(&edge("adr:a", "target", "REFERENCES"))
            .unwrap();
        store.put_edge(&edge("adr:b", "target", "DECIDES")).unwrap();

        store
            .delete_edges_from_with_label("adr:a", "DECIDES")
            .unwrap();
        let edges = store.all_edges().unwrap();
        assert_eq!(edges.len(), 2);
        assert!(edges.iter().any(|edge| edge.label == "REFERENCES"));
        assert!(edges.iter().any(|edge| edge.src == "adr:b"));

        store.delete_edge("adr:b", "target", "DECIDES").unwrap();
        assert_eq!(store.all_edges().unwrap().len(), 1);

        store.delete_node("target").unwrap();
        assert!(store.get_node("target").unwrap().is_none());
        assert!(store.all_edges().unwrap().is_empty());
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
