//! Current primary-source associations, separate from canonical graph facts.
//!
//! A binding identifies an immutable producer receipt stored elsewhere. It does
//! not establish input closure, receipt availability, source freshness or tier.

use crate::{Edge, GraphError, GraphPatch, Node, SqliteGraphStore};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const MAX_ID_BYTES: usize = 8192;
const MAX_LABEL_BYTES: usize = 256;
const MAX_REPO_BYTES: usize = 256;
const MAX_RECEIPT_BYTES: usize = 256;
const MAX_BINDINGS: usize = 100_000;

/// Maximum raw selection entries, before duplicate identities are removed.
pub const MAX_SOURCE_SELECTION_KEYS: usize = 65;

/// Unambiguous node or directed-edge identity, matching context FactReference.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FactKey {
    /// Node identity; its label is part of the complete fact digest.
    Node {
        /// Stable node identifier.
        id: String,
    },
    /// Directed edge identity.
    Edge {
        /// Source node identifier.
        source: String,
        /// Exact directed relation label.
        label: String,
        /// Destination node identifier.
        destination: String,
    },
}

impl FactKey {
    /// Stable identity of a node.
    pub fn from_node(node: &Node) -> Self {
        Self::Node {
            id: node.id.clone(),
        }
    }

    /// Stable identity of an edge.
    pub fn from_edge(edge: &Edge) -> Self {
        Self::Edge {
            source: edge.src.clone(),
            label: edge.label.clone(),
            destination: edge.dst.clone(),
        }
    }

    fn validate(&self) -> Result<(), GraphError> {
        match self {
            Self::Node { id } => bounded(id, MAX_ID_BYTES),
            Self::Edge {
                source,
                label,
                destination,
            } => {
                bounded(source, MAX_ID_BYTES)?;
                bounded(label, MAX_LABEL_BYTES)?;
                bounded(destination, MAX_ID_BYTES)
            }
        }
    }
}

/// Exact current receipt for one complete emitted fact. No source text is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceBinding {
    /// Complete typed fact identity.
    pub fact: FactKey,
    /// Registered repository ownership supplied by the host.
    pub repo_key: String,
    /// Immutable producer receipt identity, not a source-access capability.
    pub receipt_id: String,
    /// Versioned digest of the entire emitted node or edge.
    pub emitted_fact_digest: String,
}

/// Owned graph and selected association metadata from the same read revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSelectionSnapshot {
    /// Complete graph in ordinary snapshot order, not just the selected facts.
    pub graph: (Vec<Node>, Vec<Edge>),
    /// Unique selections sorted by typed fact identity.
    pub selections: Vec<FactSourceSelection>,
}

/// The observed source association state for one requested typed identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactSourceSelection {
    /// Requested identity, including when its fact is missing.
    pub fact: FactKey,
    /// Association and complete-fact state in the copied graph revision.
    pub state: FactSourceState,
}

/// A source association never establishes receipt availability or source truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactSourceState {
    /// Neither the selected fact nor an association exists.
    Missing,
    /// The fact exists and has no current association.
    Absent {
        /// Digest of the complete selected fact.
        fact_digest: String,
    },
    /// Bounded, valid metadata matches the complete selected fact.
    Present {
        /// Digest of the complete selected fact.
        fact_digest: String,
        /// Exact current association, without a receipt body or source bytes.
        binding: SourceBinding,
    },
    /// An association is malformed, orphaned or does not match the fact.
    Invalid {
        /// Digest when the selected fact exists; absent for an orphan binding.
        fact_digest: Option<String>,
    },
}

impl SourceBinding {
    fn validate(&self) -> Result<(), GraphError> {
        self.fact.validate()?;
        bounded(&self.repo_key, MAX_REPO_BYTES)?;
        bounded(&self.receipt_id, MAX_RECEIPT_BYTES)?;
        let prefix = match self.fact {
            FactKey::Node { .. } => "node-v1:",
            FactKey::Edge { .. } => "edge-v1:",
        };
        let digest = self
            .emitted_fact_digest
            .strip_prefix(prefix)
            .ok_or(GraphError::SourceBinding("invalid complete fact digest"))?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(GraphError::SourceBinding("invalid complete fact digest"));
        }
        Ok(())
    }
}

/// Canonical digest of every node field, including all properties/provenance.
pub fn node_digest(node: &Node) -> Result<String, GraphError> {
    digest("node-v1", serde_json::to_value(node)?)
}

/// Canonical digest of every edge field, including all properties/provenance.
pub fn edge_digest(edge: &Edge) -> Result<String, GraphError> {
    digest("edge-v1", serde_json::to_value(edge)?)
}

fn digest(prefix: &str, value: Value) -> Result<String, GraphError> {
    let bytes = serde_json::to_vec(&(format!("cartograph.source.{prefix}"), canonicalize(value)))?;
    Ok(format!("{prefix}:{}", core_prov::content_hash(&bytes)))
}

fn canonicalize(value: Value) -> Value {
    match value {
        Value::Object(properties) => {
            let sorted: BTreeMap<_, _> = properties
                .into_iter()
                .map(|(key, value)| (key, canonicalize(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize).collect()),
        scalar => scalar,
    }
}

fn bounded(value: &str, maximum: usize) -> Result<(), GraphError> {
    if value.is_empty() || value.len() > maximum || value.contains('\0') {
        return Err(GraphError::SourceBinding("invalid bounded identity"));
    }
    Ok(())
}

// Exact owned SQL is checked before use. A partial/future schema is never
// silently CREATE-repaired, and this version does not use graph user_version.
const SCHEMA: &[(&str, &str, &str)] = &[
    (
        "table",
        "source_binding_meta",
        "CREATE TABLE source_binding_meta (singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1), version INTEGER NOT NULL CHECK (version = 1)) STRICT",
    ),
    (
        "table",
        "source_node_bindings",
        "CREATE TABLE source_node_bindings (node_id TEXT PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE CHECK (length(CAST(node_id AS BLOB)) BETWEEN 1 AND 8192), repo_key TEXT NOT NULL CHECK (length(CAST(repo_key AS BLOB)) BETWEEN 1 AND 256), receipt_id TEXT NOT NULL CHECK (length(CAST(receipt_id AS BLOB)) BETWEEN 1 AND 256), emitted_fact_digest TEXT NOT NULL CHECK (length(CAST(emitted_fact_digest AS BLOB)) = 72)) STRICT",
    ),
    (
        "table",
        "source_edge_bindings",
        "CREATE TABLE source_edge_bindings (source TEXT NOT NULL CHECK (length(CAST(source AS BLOB)) BETWEEN 1 AND 8192), label TEXT NOT NULL CHECK (length(CAST(label AS BLOB)) BETWEEN 1 AND 256), destination TEXT NOT NULL CHECK (length(CAST(destination AS BLOB)) BETWEEN 1 AND 8192), repo_key TEXT NOT NULL CHECK (length(CAST(repo_key AS BLOB)) BETWEEN 1 AND 256), receipt_id TEXT NOT NULL CHECK (length(CAST(receipt_id AS BLOB)) BETWEEN 1 AND 256), emitted_fact_digest TEXT NOT NULL CHECK (length(CAST(emitted_fact_digest AS BLOB)) = 72), PRIMARY KEY (source, label, destination), FOREIGN KEY (source, destination, label) REFERENCES edges(src, dst, label) ON DELETE CASCADE) STRICT",
    ),
    (
        "index",
        "source_binding_nodes_repo",
        "CREATE INDEX source_binding_nodes_repo ON source_node_bindings(repo_key)",
    ),
    (
        "index",
        "source_binding_edges_repo",
        "CREATE INDEX source_binding_edges_repo ON source_edge_bindings(repo_key)",
    ),
    (
        "trigger",
        "source_binding_node_insert",
        "CREATE TRIGGER source_binding_node_insert AFTER INSERT ON nodes BEGIN DELETE FROM source_node_bindings WHERE node_id = NEW.id; DELETE FROM source_edge_bindings WHERE source = NEW.id OR destination = NEW.id; END",
    ),
    (
        "trigger",
        "source_binding_node_update",
        "CREATE TRIGGER source_binding_node_update BEFORE UPDATE ON nodes BEGIN DELETE FROM source_node_bindings WHERE node_id = OLD.id OR node_id = NEW.id; DELETE FROM source_edge_bindings WHERE OLD.id != NEW.id AND (source = OLD.id OR destination = OLD.id OR source = NEW.id OR destination = NEW.id); END",
    ),
    (
        "trigger",
        "source_binding_node_delete",
        "CREATE TRIGGER source_binding_node_delete BEFORE DELETE ON nodes BEGIN DELETE FROM source_node_bindings WHERE node_id = OLD.id; DELETE FROM source_edge_bindings WHERE source = OLD.id OR destination = OLD.id; END",
    ),
    (
        "trigger",
        "source_binding_edge_insert",
        "CREATE TRIGGER source_binding_edge_insert AFTER INSERT ON edges BEGIN DELETE FROM source_edge_bindings WHERE source = NEW.src AND label = NEW.label AND destination = NEW.dst; END",
    ),
    (
        "trigger",
        "source_binding_edge_update",
        "CREATE TRIGGER source_binding_edge_update BEFORE UPDATE ON edges BEGIN DELETE FROM source_edge_bindings WHERE (source = OLD.src AND label = OLD.label AND destination = OLD.dst) OR (source = NEW.src AND label = NEW.label AND destination = NEW.dst); END",
    ),
    (
        "trigger",
        "source_binding_edge_delete",
        "CREATE TRIGGER source_binding_edge_delete BEFORE DELETE ON edges BEGIN DELETE FROM source_edge_bindings WHERE source = OLD.src AND label = OLD.label AND destination = OLD.dst; END",
    ),
];

fn owned_schema(connection: &Connection) -> Result<BTreeMap<String, (String, String)>, GraphError> {
    // Include arbitrarily named triggers/unique indexes affecting these tables,
    // not just objects with our prefix: they can suppress or rewrite publication.
    let mut statement = connection.prepare("SELECT name, type, sql FROM sqlite_schema WHERE name GLOB 'source_binding_*' OR name IN ('source_node_bindings', 'source_edge_bindings') OR (tbl_name IN ('source_binding_meta', 'source_node_bindings', 'source_edge_bindings') AND name NOT GLOB 'sqlite_*') OR (type = 'trigger' AND tbl_name IN ('nodes', 'edges'))")?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, (row.get(1)?, row.get(2)?))))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn check_schema(connection: &Connection) -> Result<(), GraphError> {
    let actual = owned_schema(connection)?;
    if actual.len() != SCHEMA.len()
        || SCHEMA.iter().any(|(kind, name, sql)| {
            actual
                .get(*name)
                .map(|(actual_kind, actual_sql)| (actual_kind.as_str(), actual_sql.as_str()))
                != Some((*kind, *sql))
        })
    {
        return Err(GraphError::SourceBinding("incompatible association schema"));
    }
    let mut statement =
        connection.prepare("SELECT singleton, version FROM source_binding_meta LIMIT 2")?;
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    if rows != [(1, 1)] {
        return Err(GraphError::SourceBinding("unsupported association version"));
    }
    Ok(())
}

pub(crate) fn initialize(connection: &Connection) -> Result<(), GraphError> {
    let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)?;
    if owned_schema(connection)?.is_empty() {
        for (_, _, sql) in SCHEMA {
            transaction.execute_batch(sql)?;
        }
        transaction.execute(
            "INSERT INTO source_binding_meta(singleton, version) VALUES (1, 1)",
            [],
        )?;
    }
    check_schema(connection)?;
    transaction.commit()?;
    Ok(())
}

fn load_binding(
    connection: &Connection,
    fact: &FactKey,
) -> Result<Option<SourceBinding>, GraphError> {
    let row: Option<(String, String, String)> = match fact {
        FactKey::Node { id } => connection.query_row(
            "SELECT repo_key, receipt_id, emitted_fact_digest FROM source_node_bindings WHERE node_id = ?1",
            [id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?,
        FactKey::Edge { source, label, destination } => connection.query_row(
            "SELECT repo_key, receipt_id, emitted_fact_digest FROM source_edge_bindings WHERE source = ?1 AND label = ?2 AND destination = ?3",
            params![source, label, destination], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional()?,
    };
    row.map(|(repo_key, receipt_id, emitted_fact_digest)| {
        let binding = SourceBinding {
            fact: fact.clone(),
            repo_key,
            receipt_id,
            emitted_fact_digest,
        };
        binding.validate()?;
        Ok(binding)
    })
    .transpose()
}

enum SelectionBinding {
    Absent,
    Invalid,
    Present(SourceBinding),
}

fn load_selection_binding(
    connection: &Connection,
    fact: &FactKey,
) -> Result<SelectionBinding, GraphError> {
    let table_and_key = match fact {
        FactKey::Node { .. } => "source_node_bindings WHERE node_id = ?1",
        FactKey::Edge { .. } => {
            "source_edge_bindings WHERE source = ?1 AND label = ?2 AND destination = ?3"
        }
    };
    // Guard the serialized byte lengths in SQL before returning any body. BLOB
    // casts let malformed UTF-8 remain a per-key invalid association rather than
    // a String conversion error that aborts the complete selection window.
    let sql = format!(
        "SELECT
         CASE WHEN typeof(repo_key) = 'text' AND length(CAST(repo_key AS BLOB)) BETWEEN 1 AND {MAX_REPO_BYTES} THEN CAST(repo_key AS BLOB) END,
         CASE WHEN typeof(receipt_id) = 'text' AND length(CAST(receipt_id AS BLOB)) BETWEEN 1 AND {MAX_RECEIPT_BYTES} THEN CAST(receipt_id AS BLOB) END,
         CASE WHEN typeof(emitted_fact_digest) = 'text' AND length(CAST(emitted_fact_digest AS BLOB)) = 72 THEN CAST(emitted_fact_digest AS BLOB) END
         FROM {table_and_key}"
    );
    let read = |row: &rusqlite::Row<'_>| {
        Ok((
            row.get::<_, Option<Vec<u8>>>(0)?,
            row.get::<_, Option<Vec<u8>>>(1)?,
            row.get::<_, Option<Vec<u8>>>(2)?,
        ))
    };
    let row = match fact {
        FactKey::Node { id } => connection.query_row(&sql, [id], read).optional()?,
        FactKey::Edge {
            source,
            label,
            destination,
        } => connection
            .query_row(&sql, params![source, label, destination], read)
            .optional()?,
    };
    let Some((repo_key, receipt_id, emitted_fact_digest)) = row else {
        return Ok(SelectionBinding::Absent);
    };
    let (Some(repo_key), Some(receipt_id), Some(emitted_fact_digest)) = (
        repo_key.and_then(|bytes| String::from_utf8(bytes).ok()),
        receipt_id.and_then(|bytes| String::from_utf8(bytes).ok()),
        emitted_fact_digest.and_then(|bytes| String::from_utf8(bytes).ok()),
    ) else {
        return Ok(SelectionBinding::Invalid);
    };
    let binding = SourceBinding {
        fact: fact.clone(),
        repo_key,
        receipt_id,
        emitted_fact_digest,
    };
    Ok(if binding.validate().is_ok() {
        SelectionBinding::Present(binding)
    } else {
        SelectionBinding::Invalid
    })
}

fn snapshot_digest(
    graph: &(Vec<Node>, Vec<Edge>),
    fact: &FactKey,
) -> Result<Option<String>, GraphError> {
    // Ordinary snapshot rows already have these orders. Avoid reloading JSON or
    // allocating a second index over the entire graph for at most 65 identities.
    match fact {
        FactKey::Node { id } => graph
            .0
            .binary_search_by(|node| node.id.cmp(id))
            .ok()
            .map(|index| node_digest(&graph.0[index]))
            .transpose(),
        FactKey::Edge {
            source,
            label,
            destination,
        } => graph
            .1
            .binary_search_by(|edge| {
                (&edge.src, &edge.dst, &edge.label).cmp(&(source, destination, label))
            })
            .ok()
            .map(|index| edge_digest(&graph.1[index]))
            .transpose(),
    }
}

fn current_digest(connection: &Connection, fact: &FactKey) -> Result<Option<String>, GraphError> {
    match fact {
        FactKey::Node { id } => {
            let row: Option<(String, String)> = connection
                .query_row(
                    "SELECT label, props FROM nodes WHERE id = ?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            row.map(|(label, props)| {
                node_digest(&Node {
                    id: id.clone(),
                    label,
                    props: serde_json::from_str(&props)?,
                })
            })
            .transpose()
        }
        FactKey::Edge {
            source,
            label,
            destination,
        } => {
            let props: Option<String> = connection
                .query_row(
                    "SELECT props FROM edges WHERE src = ?1 AND label = ?2 AND dst = ?3",
                    params![source, label, destination],
                    |row| row.get(0),
                )
                .optional()?;
            props
                .map(|props| {
                    edge_digest(&Edge {
                        src: source.clone(),
                        label: label.clone(),
                        dst: destination.clone(),
                        props: serde_json::from_str(&props)?,
                    })
                })
                .transpose()
        }
    }
}

impl SqliteGraphStore {
    /// Copy the complete graph and bounded, sorted unique source selections in
    /// one read transaction. More than 65 raw keys and invalid identities fail
    /// before database access. Empty selections still copy the complete graph.
    ///
    /// Schema, query and graph-data failures abort the operation; malformed
    /// selected association metadata instead yields a per-key `Invalid` state.
    /// The owned result grants no source access and establishes no input closure.
    pub fn read_source_selection_snapshot(
        &self,
        keys: &[FactKey],
    ) -> Result<SourceSelectionSnapshot, GraphError> {
        if keys.len() > MAX_SOURCE_SELECTION_KEYS {
            return Err(GraphError::SourceBinding("too many source selection keys"));
        }
        for key in keys {
            key.validate()?;
        }
        let unique: BTreeSet<_> = keys.iter().collect();
        let transaction = self.conn.unchecked_transaction()?;
        check_schema(&self.conn)?;
        let graph = self.read_snapshot_rows(None, None)?;
        let mut selections = Vec::with_capacity(unique.len());
        for fact in unique {
            let fact_digest = snapshot_digest(&graph, fact)?;
            let binding = load_selection_binding(&self.conn, fact)?;
            #[cfg(any(test, feature = "test-support"))]
            {
                let after_lookup = self.source_binding_after_lookup.borrow_mut().take();
                if let Some(after_lookup) = after_lookup {
                    after_lookup()?;
                }
            }
            let state = match (fact_digest, binding) {
                (None, SelectionBinding::Absent) => FactSourceState::Missing,
                (Some(fact_digest), SelectionBinding::Absent) => {
                    FactSourceState::Absent { fact_digest }
                }
                (Some(fact_digest), SelectionBinding::Present(binding))
                    if binding.emitted_fact_digest == fact_digest =>
                {
                    FactSourceState::Present {
                        fact_digest,
                        binding,
                    }
                }
                (fact_digest, _) => FactSourceState::Invalid { fact_digest },
            };
            selections.push(FactSourceSelection {
                fact: fact.clone(),
                state,
            });
        }
        transaction.commit()?;
        Ok(SourceSelectionSnapshot { graph, selections })
    }

    /// Publish scoped facts and replace only this repo's current source bindings
    /// in one write transaction. Every requested binding must match its final
    /// fact; missing, duplicate, wrong-owner or mismatched bindings roll back.
    pub fn apply_patch_with_source_bindings_if_snapshot_matches(
        &mut self,
        expected: &(Vec<Node>, Vec<Edge>),
        patch: &GraphPatch,
        repo_key: &str,
        bindings: &[SourceBinding],
    ) -> Result<bool, GraphError> {
        bounded(repo_key, MAX_REPO_BYTES)?;
        if bindings.len() > MAX_BINDINGS {
            return Err(GraphError::SourceBinding("too many associations"));
        }
        let mut seen = BTreeSet::new();
        for binding in bindings {
            binding.validate()?;
            if binding.repo_key != repo_key || !seen.insert(&binding.fact) {
                return Err(GraphError::SourceBinding(
                    "duplicate or wrong-owner association",
                ));
            }
        }
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        check_schema(&self.conn)?;
        if &self.read_snapshot_rows(None, None)? != expected {
            return Ok(false);
        }
        // Check before patch triggers invalidate touched rows: a caller cannot
        // replace another repo's association by upserting its fact first.
        for binding in bindings {
            if load_binding(&self.conn, &binding.fact)?.is_some_and(|old| old.repo_key != repo_key)
            {
                return Err(GraphError::SourceBinding(
                    "fact belongs to another source association",
                ));
            }
        }
        transaction.execute(
            "DELETE FROM source_node_bindings WHERE repo_key = ?1",
            [repo_key],
        )?;
        transaction.execute(
            "DELETE FROM source_edge_bindings WHERE repo_key = ?1",
            [repo_key],
        )?;
        self.apply_patch_rows(patch)?;
        for binding in bindings {
            if current_digest(&self.conn, &binding.fact)?.as_deref()
                != Some(binding.emitted_fact_digest.as_str())
            {
                return Err(GraphError::SourceBinding(
                    "missing fact or complete fact digest mismatch",
                ));
            }
            let inserted = match &binding.fact {
                FactKey::Node { id } => transaction.execute(
                    "INSERT INTO source_node_bindings(node_id, repo_key, receipt_id, emitted_fact_digest) VALUES (?1, ?2, ?3, ?4)",
                    params![id, repo_key, binding.receipt_id, binding.emitted_fact_digest],
                )?,
                FactKey::Edge { source, label, destination } => transaction.execute(
                    "INSERT INTO source_edge_bindings(source, label, destination, repo_key, receipt_id, emitted_fact_digest) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![source, label, destination, repo_key, binding.receipt_id, binding.emitted_fact_digest],
                )?,
            };
            if inserted != 1 {
                return Err(GraphError::SourceBinding("association publication failed"));
            }
        }
        transaction.commit()?;
        Ok(true)
    }

    /// Copy a binding and verify its complete fact in the same read revision.
    /// The caller must still validate receipt identity, registration and retained
    /// source in their own stores; this method grants no source access.
    pub fn current_source_binding(
        &self,
        fact: &FactKey,
    ) -> Result<Option<SourceBinding>, GraphError> {
        fact.validate()?;
        let transaction = self.conn.unchecked_transaction()?;
        check_schema(&self.conn)?;
        let binding = load_binding(&self.conn, fact)?;
        #[cfg(any(test, feature = "test-support"))]
        {
            let after_lookup = self.source_binding_after_lookup.borrow_mut().take();
            if let Some(after_lookup) = after_lookup {
                after_lookup()?;
            }
        }
        if let Some(binding) = &binding
            && current_digest(&self.conn, fact)?.as_deref()
                != Some(binding.emitted_fact_digest.as_str())
        {
            return Err(GraphError::SourceBinding(
                "missing fact or complete fact digest mismatch",
            ));
        }
        transaction.commit()?;
        Ok(binding)
    }

    /// Ordered current-association metadata for retention preview, without
    /// loading graph properties, receipt bodies or source bytes.
    pub fn source_bindings_for_repo(
        &self,
        repo_key: &str,
    ) -> Result<Vec<SourceBinding>, GraphError> {
        bounded(repo_key, MAX_REPO_BYTES)?;
        let transaction = self.conn.unchecked_transaction()?;
        check_schema(&self.conn)?;
        let count: i64 = self.conn.query_row(
            "SELECT (SELECT count(*) FROM source_node_bindings WHERE repo_key = ?1) + (SELECT count(*) FROM source_edge_bindings WHERE repo_key = ?1)",
            [repo_key], |row| row.get(0),
        )?;
        if usize::try_from(count)
            .ok()
            .filter(|count| *count <= MAX_BINDINGS)
            .is_none()
        {
            return Err(GraphError::SourceBinding("too many associations"));
        }
        let mut bindings = Vec::new();
        {
            let mut statement = self.conn.prepare("SELECT node_id, receipt_id, emitted_fact_digest FROM source_node_bindings WHERE repo_key = ?1 ORDER BY node_id")?;
            let rows = statement.query_map([repo_key], |row| {
                Ok(SourceBinding {
                    fact: FactKey::Node { id: row.get(0)? },
                    repo_key: repo_key.into(),
                    receipt_id: row.get(1)?,
                    emitted_fact_digest: row.get(2)?,
                })
            })?;
            for row in rows {
                let binding = row?;
                binding.validate()?;
                bindings.push(binding);
            }
        }
        {
            let mut statement = self.conn.prepare("SELECT source, label, destination, receipt_id, emitted_fact_digest FROM source_edge_bindings WHERE repo_key = ?1 ORDER BY source, label, destination")?;
            let rows = statement.query_map([repo_key], |row| {
                Ok(SourceBinding {
                    fact: FactKey::Edge {
                        source: row.get(0)?,
                        label: row.get(1)?,
                        destination: row.get(2)?,
                    },
                    repo_key: repo_key.into(),
                    receipt_id: row.get(3)?,
                    emitted_fact_digest: row.get(4)?,
                })
            })?;
            for row in rows {
                let binding = row?;
                binding.validate()?;
                bindings.push(binding);
            }
        }
        transaction.commit()?;
        Ok(bindings)
    }

    /// One-shot deterministic cross-connection hook after association lookup,
    /// while the current-binding or selection-snapshot transaction is still open.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn set_source_binding_after_lookup_hook(
        &self,
        hook: impl FnOnce() -> Result<(), GraphError> + Send + 'static,
    ) {
        *self.source_binding_after_lookup.borrow_mut() = Some(Box::new(hook));
    }
}

#[cfg(test)]
mod tests;
