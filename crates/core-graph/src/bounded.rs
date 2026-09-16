//! Opt-in admission limits for complete investigation snapshots (SPEC-10).

use crate::{Edge, GraphError, Node, SqliteGraphStore};
use rusqlite::Connection;
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use serde_json::{Map, Number, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Write};

/// Narrowable limits for the complete `(nodes, edges)` JSON graph content.
///
/// Defaults are the SPEC-10 ceilings. They bound admitted structure and bytes,
/// not SQLite's page cache, allocator overhead or exact process resident memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotReadLimits {
    /// Maximum canonical graph bytes, and separately maximum raw SQL field bytes.
    pub max_bytes: usize,
    /// Maximum combined node and edge rows.
    pub max_rows: usize,
    /// Maximum JSON value depth: graph array is depth one, properties depth four.
    pub max_depth: usize,
    /// Maximum cumulative JSON values, including graph/fact envelopes. Keys do
    /// not count as values; duplicate property values still consume this budget.
    pub max_values: usize,
}

impl SnapshotReadLimits {
    /// Hard maximum raw and canonical graph bytes, independently enforced.
    pub const MAX_BYTES: usize = 32 * 1024 * 1024;
    /// Hard maximum combined graph rows.
    pub const MAX_ROWS: usize = 50_000;
    /// Hard maximum complete-graph JSON depth.
    pub const MAX_DEPTH: usize = 32;
    /// Hard maximum cumulative complete-graph JSON values.
    pub const MAX_VALUES: usize = 1_000_000;

    pub(crate) fn validate(self) -> Result<(), GraphError> {
        if self.max_bytes == 0
            || self.max_bytes > Self::MAX_BYTES
            || self.max_rows > Self::MAX_ROWS
            || self.max_depth == 0
            || self.max_depth > Self::MAX_DEPTH
            || self.max_values == 0
            || self.max_values > Self::MAX_VALUES
        {
            return Err(SnapshotBoundsError::InvalidLimits.into());
        }
        Ok(())
    }
}

impl Default for SnapshotReadLimits {
    fn default() -> Self {
        Self {
            max_bytes: Self::MAX_BYTES,
            max_rows: Self::MAX_ROWS,
            max_depth: Self::MAX_DEPTH,
            max_values: Self::MAX_VALUES,
        }
    }
}

/// Source-free failures from the opt-in bounded snapshot reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotBoundsError {
    /// Caller limits exceeded the hard profile or used an invalid zero limit.
    #[error("invalid snapshot limits")]
    InvalidLimits,
    /// Combined row count exceeded the selected limit.
    #[error("graph row limit exceeded")]
    RowLimit,
    /// Stored field bytes exceeded the limit before any row bodies were copied.
    #[error("raw graph byte limit exceeded")]
    RawByteLimit,
    /// Complete canonical graph JSON exceeded the limit, including escaping.
    #[error("canonical graph byte limit exceeded")]
    CanonicalByteLimit,
    /// A JSON value exceeded the complete-graph depth limit.
    #[error("graph JSON depth limit exceeded")]
    JsonDepthLimit,
    /// Complete-graph JSON value count exceeded the cumulative limit.
    #[error("graph JSON value limit exceeded")]
    JsonValueLimit,
    /// Graph rows did not originate in ordinary compatible stored tables.
    #[error("incompatible graph table schema")]
    InvalidSchema,
    /// A stored field had an invalid SQL type, length or UTF-8 encoding.
    #[error("invalid graph row")]
    InvalidRow,
    /// A property body was not one complete valid JSON value.
    #[error("invalid graph property JSON")]
    InvalidJson,
}

impl SqliteGraphStore {
    /// Read a complete, ordered graph under explicit admission limits, in one
    /// SQLite revision. Raw SQL byte/type checks precede loading row bodies;
    /// streaming property decoding enforces depth/value limits before building
    /// unbounded containers. Canonical counting never allocates a graph JSON
    /// buffer. Existing unbounded snapshot APIs retain their behavior.
    pub fn read_snapshot_bounded(
        &self,
        limits: SnapshotReadLimits,
    ) -> Result<(Vec<Node>, Vec<Edge>), GraphError> {
        limits.validate()?;
        let transaction = self.conn.unchecked_transaction()?;
        let graph = self.read_snapshot_rows_bounded(limits)?;
        transaction.commit()?;
        Ok(graph)
    }

    /// Caller owns the transaction, shared with source-association selection.
    pub(crate) fn read_snapshot_rows_bounded(
        &self,
        limits: SnapshotReadLimits,
    ) -> Result<(Vec<Node>, Vec<Edge>), GraphError> {
        preflight_schema(&self.conn)?;
        let (node_count, edge_count) = preflight_rows(&self.conn, limits)?;
        let mut budget = JsonBudget::new(limits);
        budget.enter(1)?; // complete graph tuple
        budget.enter(2)?; // nodes array
        budget.enter(2)?; // edges array
        let mut nodes = Vec::with_capacity(node_count);
        {
            let mut statement = self
                .conn
                .prepare("SELECT id, label, props FROM nodes ORDER BY id")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                budget.enter(3)?;
                budget.enter(4)?; // id
                budget.enter(4)?; // label
                let id = field(row, 0)?;
                let label = field(row, 1)?;
                let props = parse_properties(&field(row, 2)?, &mut budget)?;
                nodes.push(Node { id, label, props });
            }
        }
        #[cfg(any(test, feature = "test-support"))]
        {
            let after_nodes = self.snapshot_after_nodes.borrow_mut().take();
            if let Some(after_nodes) = after_nodes {
                after_nodes()?;
            }
        }
        let mut edges = Vec::with_capacity(edge_count);
        {
            let mut statement = self
                .conn
                .prepare("SELECT src, dst, label, props FROM edges ORDER BY src, dst, label")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                budget.enter(3)?;
                budget.enter(4)?; // src
                budget.enter(4)?; // dst
                budget.enter(4)?; // label
                let src = field(row, 0)?;
                let dst = field(row, 1)?;
                let label = field(row, 2)?;
                let props = parse_properties(&field(row, 3)?, &mut budget)?;
                edges.push(Edge {
                    src,
                    dst,
                    label,
                    props,
                });
            }
        }
        let graph = (nodes, edges);
        let mut counter = ByteCounter {
            remaining: limits.max_bytes,
        };
        serde_json::to_writer(&mut counter, &CanonicalGraph(&graph))
            .map_err(|_| SnapshotBoundsError::CanonicalByteLimit)?;
        Ok(graph)
    }
}

fn field(row: &rusqlite::Row<'_>, index: usize) -> Result<String, GraphError> {
    row.get(index)
        .map_err(|_| SnapshotBoundsError::InvalidRow.into())
}

fn preflight_schema(connection: &Connection) -> Result<(), GraphError> {
    // A view or generated expression could change between length and body
    // evaluation despite one read transaction. Only ordinary stored columns are
    // allowed here. Bound schema text before PRAGMA exposes default expressions.
    for (table, expected) in [
        (
            "nodes",
            &[("id", 1, 1), ("label", 1, 0), ("props", 1, 0)][..],
        ),
        (
            "edges",
            &[
                ("src", 1, 1),
                ("dst", 1, 2),
                ("label", 1, 3),
                ("props", 1, 0),
            ][..],
        ),
    ] {
        let valid: bool = connection.query_row(
            "SELECT count(*) = 1 AND coalesce(min(type = 'table' AND typeof(sql) = 'text' AND length(CAST(sql AS BLOB)) <= 4096), 0) FROM sqlite_schema WHERE name = ?1",
            [table], |row| row.get(0),
        )?;
        if !valid {
            return Err(SnapshotBoundsError::InvalidSchema.into());
        }
        let mut statement = connection.prepare(&format!("PRAGMA table_xinfo({table})"))?;
        let mut rows = statement.query([])?;
        for (name, not_null, primary_key) in expected {
            let Some(row) = rows.next()? else {
                return Err(SnapshotBoundsError::InvalidSchema.into());
            };
            if row.get::<_, String>(1)? != *name
                || row.get::<_, String>(2)? != "TEXT"
                || row.get::<_, i64>(3)? != *not_null
                || row.get::<_, i64>(5)? != *primary_key
                || row.get::<_, i64>(6)? != 0
            {
                return Err(SnapshotBoundsError::InvalidSchema.into());
            }
        }
        if rows.next()?.is_some() {
            return Err(SnapshotBoundsError::InvalidSchema.into());
        }
    }
    Ok(())
}

fn preflight_rows(
    connection: &Connection,
    limits: SnapshotReadLimits,
) -> Result<(usize, usize), GraphError> {
    let counts: (i64, i64) = connection.query_row(
        "SELECT (SELECT count(*) FROM nodes), (SELECT count(*) FROM edges)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let node_count = usize::try_from(counts.0).map_err(|_| SnapshotBoundsError::InvalidRow)?;
    let edge_count = usize::try_from(counts.1).map_err(|_| SnapshotBoundsError::InvalidRow)?;
    if node_count
        .checked_add(edge_count)
        .is_none_or(|count| count > limits.max_rows)
    {
        return Err(SnapshotBoundsError::RowLimit.into());
    }
    let mut remaining = limits.max_bytes;
    for (table, columns) in [
        ("nodes", &["id", "label", "props"][..]),
        ("edges", &["src", "dst", "label", "props"][..]),
    ] {
        let projection = columns
            .iter()
            .map(|column| format!("typeof({column}) = 'text', length(CAST({column} AS BLOB))"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut statement = connection.prepare(&format!("SELECT {projection} FROM {table}"))?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            for index in 0..columns.len() {
                if !row.get::<_, bool>(2 * index)? {
                    return Err(SnapshotBoundsError::InvalidRow.into());
                }
                let length = row.get::<_, Option<i64>>(2 * index + 1)?;
                let length = length
                    .and_then(|length| usize::try_from(length).ok())
                    .ok_or(SnapshotBoundsError::InvalidRow)?;
                remaining = remaining
                    .checked_sub(length)
                    .ok_or(SnapshotBoundsError::RawByteLimit)?;
            }
        }
    }
    Ok((node_count, edge_count))
}

struct JsonBudget {
    limits: SnapshotReadLimits,
    remaining: usize,
    failure: Option<SnapshotBoundsError>,
}

impl JsonBudget {
    fn new(limits: SnapshotReadLimits) -> Self {
        Self {
            limits,
            remaining: limits.max_values,
            failure: None,
        }
    }

    fn enter(&mut self, depth: usize) -> Result<(), SnapshotBoundsError> {
        if depth > self.limits.max_depth {
            return Err(SnapshotBoundsError::JsonDepthLimit);
        }
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or(SnapshotBoundsError::JsonValueLimit)?;
        Ok(())
    }
}

fn parse_properties(input: &str, budget: &mut JsonBudget) -> Result<Value, GraphError> {
    let mut decoder = serde_json::Deserializer::from_str(input);
    let parsed = JsonSeed { budget, depth: 4 }.deserialize(&mut decoder);
    let value = parsed.map_err(|_| {
        budget
            .failure
            .take()
            .unwrap_or(SnapshotBoundsError::InvalidJson)
    })?;
    decoder
        .end()
        .map_err(|_| SnapshotBoundsError::InvalidJson)?;
    Ok(value)
}

struct JsonSeed<'a> {
    budget: &'a mut JsonBudget,
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for JsonSeed<'_> {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if let Err(error) = self.budget.enter(self.depth) {
            self.budget.failure = Some(error);
            return Err(serde::de::Error::custom("graph JSON bound exceeded"));
        }
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for JsonSeed<'_> {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded JSON value")
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_bool<E>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("invalid graph JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(JsonSeed {
            budget: self.budget,
            depth: self.depth + 1,
        })? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            let value = map.next_value_seed(JsonSeed {
                budget: self.budget,
                depth: self.depth + 1,
            })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}

struct ByteCounter {
    remaining: usize,
}

impl Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.remaining = self
            .remaining
            .checked_sub(bytes.len())
            .ok_or_else(|| io::Error::other("canonical graph byte limit exceeded"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Sorted object properties match the existing context/fact identity semantics,
/// without cloning all graph properties into a second serde_json::Value tree.
struct CanonicalValue<'a>(&'a Value);

impl Serialize for CanonicalValue<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Value::Object(values) => {
                let sorted: BTreeMap<_, _> = values.iter().collect();
                let mut map = serializer.serialize_map(Some(sorted.len()))?;
                for (key, value) in sorted {
                    map.serialize_entry(key, &CanonicalValue(value))?;
                }
                map.end()
            }
            Value::Array(values) => {
                let mut sequence = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    sequence.serialize_element(&CanonicalValue(value))?;
                }
                sequence.end()
            }
            scalar => scalar.serialize(serializer),
        }
    }
}

struct CanonicalNode<'a>(&'a Node);

impl Serialize for CanonicalNode<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("id", &self.0.id)?;
        map.serialize_entry("label", &self.0.label)?;
        map.serialize_entry("props", &CanonicalValue(&self.0.props))?;
        map.end()
    }
}

struct CanonicalEdge<'a>(&'a Edge);

impl Serialize for CanonicalEdge<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(4))?;
        map.serialize_entry("dst", &self.0.dst)?;
        map.serialize_entry("label", &self.0.label)?;
        map.serialize_entry("props", &CanonicalValue(&self.0.props))?;
        map.serialize_entry("src", &self.0.src)?;
        map.end()
    }
}

struct CanonicalGraph<'a>(&'a (Vec<Node>, Vec<Edge>));

impl Serialize for CanonicalGraph<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let nodes: Vec<_> = self.0.0.iter().map(CanonicalNode).collect();
        let edges: Vec<_> = self.0.1.iter().map(CanonicalEdge).collect();
        (&nodes, &edges).serialize(serializer)
    }
}

#[cfg(test)]
mod tests;
