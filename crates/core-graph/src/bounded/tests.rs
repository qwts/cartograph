use super::*;
use crate::source::{FactKey, FactSourceState, SourceBinding, node_digest};
use crate::{GraphPatch, GraphStore};
use rusqlite::params;

fn node(id: &str, props: Value) -> Node {
    Node {
        id: id.into(),
        label: "Symbol".into(),
        props,
    }
}

fn binding(node: &Node, receipt: &str) -> SourceBinding {
    SourceBinding {
        fact: FactKey::from_node(node),
        repo_key: "repo/a".into(),
        receipt_id: format!(
            "ts-primary-v1:{}",
            core_prov::content_hash(receipt.as_bytes())
        ),
        emitted_fact_digest: node_digest(node).unwrap(),
    }
}

fn seed(store: &mut SqliteGraphStore) -> Vec<FactKey> {
    let a = node("a", serde_json::json!({"name":"original"}));
    store.put_node(&a).unwrap();
    store.put_node(&node("b", Value::Null)).unwrap();
    store
        .put_edge(&Edge {
            src: "a".into(),
            dst: "b".into(),
            label: "CALLS".into(),
            props: Value::Null,
        })
        .unwrap();
    let expected = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_with_source_bindings_if_snapshot_matches(
                &expected,
                &GraphPatch::default(),
                "repo/a",
                &[binding(&a, "original")],
            )
            .unwrap()
    );
    vec![FactKey::from_node(&a), FactKey::Node { id: "b".into() }]
}

fn assert_bounds<T: fmt::Debug>(result: Result<T, GraphError>, expected: SnapshotBoundsError) {
    assert!(matches!(result, Err(GraphError::SnapshotBounds(actual)) if actual == expected));
}

#[test]
fn bounded_snapshot_preserves_facts_order_and_exact_canonical_byte_boundary() {
    // AC-0182: admitted bytes are the existing complete canonical graph content,
    // including escaped identifiers and original property/provenance values.
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    for id in ["z", "a\"é\n", "b"] {
        store
            .put_node(&node(
                id,
                serde_json::json!({"z":[false,null,-1,18446744073709551615u64,1.25],"a":{"x":"é"}}),
            ))
            .unwrap();
    }
    for (dst, label) in [("z", "A"), ("b", "Z"), ("b", "A")] {
        store
            .put_edge(&Edge {
                src: "a\"é\n".into(),
                dst: dst.into(),
                label: label.into(),
                props: serde_json::json!({"arr":[2,1]}),
            })
            .unwrap();
    }
    let expected = store.read_snapshot().unwrap();
    // CanonicalValue recursively sorts the standard serialized graph fields,
    // independently of the row-specific CanonicalNode/CanonicalEdge writers.
    let canonical =
        serde_json::to_vec(&CanonicalValue(&serde_json::to_value(&expected).unwrap())).unwrap();
    assert_eq!(
        serde_json::to_vec(&CanonicalGraph(&expected)).unwrap(),
        canonical
    );
    let limits = SnapshotReadLimits {
        max_bytes: canonical.len(),
        ..SnapshotReadLimits::default()
    };
    assert_eq!(store.read_snapshot_bounded(limits).unwrap(), expected);
    assert_bounds(
        store.read_snapshot_bounded(SnapshotReadLimits {
            max_bytes: canonical.len() - 1,
            ..limits
        }),
        SnapshotBoundsError::CanonicalByteLimit,
    );
    assert!(store.conn.is_autocommit());
    assert_eq!(store.read_snapshot().unwrap(), expected);
}

#[test]
fn bounded_snapshot_preflights_raw_utf8_bytes_and_rows_before_property_decode() {
    // AC-0182: multibyte and malformed JSON bodies must be rejected by the raw
    // admission bound before decoding them, with no body in error diagnostics.
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    store.put_node(&node("a", Value::Null)).unwrap();
    store.put_node(&node("b", Value::Null)).unwrap();
    store
        .conn
        .execute(
            "UPDATE nodes SET props = ?1 WHERE id = 'a'",
            [format!("source-canary-invalid-json{}", "é".repeat(80))],
        )
        .unwrap();
    assert_bounds(
        store.read_snapshot_bounded(SnapshotReadLimits {
            max_rows: 1,
            max_bytes: 128,
            ..SnapshotReadLimits::default()
        }),
        SnapshotBoundsError::RowLimit,
    );
    let error = store
        .read_snapshot_bounded(SnapshotReadLimits {
            max_bytes: 128,
            ..SnapshotReadLimits::default()
        })
        .unwrap_err();
    assert!(matches!(
        error,
        GraphError::SnapshotBounds(SnapshotBoundsError::RawByteLimit)
    ));
    assert!(!error.to_string().contains("source-canary"));
    assert_bounds(
        store.read_snapshot_bounded(SnapshotReadLimits::default()),
        SnapshotBoundsError::InvalidJson,
    );
    assert!(store.conn.is_autocommit());
}

#[test]
fn bounded_snapshot_rejects_invalid_sql_types_and_utf8_without_body_disclosure() {
    // AC-0182: metadata-only type checks precede body copying even for corrupt
    // non-STRICT tables; malformed TEXT encoding is rejected before JSON decode.
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE nodes(id, label, props); CREATE TABLE edges(src, dst, label, props);
             INSERT INTO nodes VALUES ('a', 'Symbol', '{}');",
        )
        .unwrap();
    for expression in ["zeroblob(1048576)", "NULL", "42"] {
        connection
            .execute(&format!("UPDATE nodes SET props = {expression}"), [])
            .unwrap();
        assert_bounds(
            preflight_rows(&connection, SnapshotReadLimits::default()),
            SnapshotBoundsError::InvalidRow,
        );
    }
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    store.put_node(&node("a", Value::Null)).unwrap();
    store
        .conn
        .execute("UPDATE nodes SET props = CAST(x'80' AS TEXT)", [])
        .unwrap();
    assert_bounds(
        store.read_snapshot_bounded(SnapshotReadLimits::default()),
        SnapshotBoundsError::InvalidRow,
    );
    assert!(store.conn.is_autocommit());
}

#[test]
fn bounded_snapshot_enforces_cumulative_json_values_and_complete_graph_depth() {
    // AC-0182: budgets cover all rows, envelopes and scalar/container values;
    // duplicate keys cannot hide parser work behind a smaller final JSON object.
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    for id in ["a", "b"] {
        store
            .put_node(&node(id, serde_json::json!([null, true])))
            .unwrap();
    }
    // Graph + two arrays = 3; each node object/id/label/props/two items = 6.
    let limits = SnapshotReadLimits {
        max_depth: 5,
        max_values: 15,
        ..SnapshotReadLimits::default()
    };
    assert_eq!(store.read_snapshot_bounded(limits).unwrap().0.len(), 2);
    assert_bounds(
        store.read_snapshot_bounded(SnapshotReadLimits {
            max_values: 14,
            ..limits
        }),
        SnapshotBoundsError::JsonValueLimit,
    );
    assert_bounds(
        store.read_snapshot_bounded(SnapshotReadLimits {
            max_depth: 4,
            ..limits
        }),
        SnapshotBoundsError::JsonDepthLimit,
    );
    store.delete_node("b").unwrap();
    store
        .conn
        .execute("UPDATE nodes SET props = '{\"k\":0,\"k\":1,\"k\":2}'", [])
        .unwrap();
    assert_bounds(
        store.read_snapshot_bounded(SnapshotReadLimits {
            max_values: 9,
            ..limits
        }),
        SnapshotBoundsError::JsonValueLimit,
    );
    assert_eq!(
        store.read_snapshot().unwrap().0[0].props,
        serde_json::json!({"k":2})
    );
    assert!(store.conn.is_autocommit());
}

#[test]
fn bounded_snapshot_hard_limits_and_empty_graph_are_validated_before_access() {
    // AC-0182: callers can only narrow ceilings; bad limits never consult even
    // a missing table. A zero-row cap remains useful for admitting an empty graph.
    let store = SqliteGraphStore::open_in_memory().unwrap();
    assert_eq!(SnapshotReadLimits::default().max_bytes, 32 * 1024 * 1024);
    assert_eq!(SnapshotReadLimits::default().max_rows, 50_000);
    assert_eq!(SnapshotReadLimits::default().max_depth, 32);
    assert_eq!(SnapshotReadLimits::default().max_values, 1_000_000);
    assert_eq!(
        store
            .read_snapshot_bounded(SnapshotReadLimits {
                max_bytes: 7,
                max_rows: 0,
                max_depth: 2,
                max_values: 3,
            })
            .unwrap(),
        (vec![], vec![]),
    );
    store.conn.execute("DROP TABLE edges", []).unwrap();
    let defaults = SnapshotReadLimits::default();
    for limits in [
        SnapshotReadLimits {
            max_bytes: 0,
            ..defaults
        },
        SnapshotReadLimits {
            max_bytes: defaults.max_bytes + 1,
            ..defaults
        },
        SnapshotReadLimits {
            max_rows: defaults.max_rows + 1,
            ..defaults
        },
        SnapshotReadLimits {
            max_depth: 0,
            ..defaults
        },
        SnapshotReadLimits {
            max_depth: defaults.max_depth + 1,
            ..defaults
        },
        SnapshotReadLimits {
            max_values: 0,
            ..defaults
        },
        SnapshotReadLimits {
            max_values: defaults.max_values + 1,
            ..defaults
        },
    ] {
        assert_bounds(
            store.read_snapshot_bounded(limits),
            SnapshotBoundsError::InvalidLimits,
        );
        assert_bounds(
            store.read_source_selection_snapshot_bounded(&[], limits),
            SnapshotBoundsError::InvalidLimits,
        );
    }
    assert!(store.conn.is_autocommit());
}

#[test]
fn bounded_snapshot_rejects_views_and_hidden_generated_columns() {
    // AC-0182: length preflight is valid only for stored ordinary table fields,
    // not a view/expression that could yield different bodies when reevaluated.
    for replacement in [
        "CREATE VIEW nodes AS SELECT 'a' AS id, 'Symbol' AS label, '{}' AS props",
        "CREATE TABLE nodes(id TEXT PRIMARY KEY NOT NULL, label TEXT NOT NULL, props TEXT NOT NULL, extra TEXT GENERATED ALWAYS AS (props) VIRTUAL) STRICT",
    ] {
        let store = SqliteGraphStore::open_in_memory().unwrap();
        store.conn.execute("DROP TABLE nodes", []).unwrap();
        store.conn.execute(replacement, []).unwrap();
        assert_bounds(
            store.read_snapshot_bounded(SnapshotReadLimits::default()),
            SnapshotBoundsError::InvalidSchema,
        );
        assert!(store.conn.is_autocommit());
    }
}

#[test]
fn bounded_source_selection_is_coherent_across_receipt_and_graph_publication() {
    // AC-0182: actual WAL publication occurs within the production transaction,
    // once after nodes and once after binding lookup, including equal-fact cases.
    for after_nodes in [true, false] {
        for change_graph in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("graph.db");
            let mut reader = SqliteGraphStore::open(&path).unwrap();
            let keys = seed(&mut reader);
            let limits = SnapshotReadLimits::default();
            let before = reader
                .read_source_selection_snapshot_bounded(&keys, limits)
                .unwrap();
            let mut writer = SqliteGraphStore::open(&path).unwrap();
            let expected = before.graph.clone();
            let mut changed = expected.0[0].clone();
            let mut changed_edge = expected.1[0].clone();
            if change_graph {
                changed.props["name"] = "changed".into();
                changed_edge.props = serde_json::json!({"changed":true});
            }
            let new_binding = binding(&changed, "replacement");
            let publish = move || {
                let patch = if change_graph {
                    GraphPatch {
                        upsert_nodes: vec![changed],
                        upsert_edges: vec![changed_edge],
                        ..GraphPatch::default()
                    }
                } else {
                    GraphPatch::default()
                };
                assert!(writer.apply_patch_with_source_bindings_if_snapshot_matches(
                    &expected,
                    &patch,
                    "repo/a",
                    &[new_binding]
                )?);
                Ok(())
            };
            if after_nodes {
                reader.set_snapshot_after_nodes_hook(publish);
            } else {
                reader.set_source_binding_after_lookup_hook(publish);
            }
            assert_eq!(
                reader
                    .read_source_selection_snapshot_bounded(&keys, limits)
                    .unwrap(),
                before
            );
            let after = reader
                .read_source_selection_snapshot_bounded(&keys, limits)
                .unwrap();
            assert_eq!(after.graph == before.graph, !change_graph);
            assert_ne!(after.selections, before.selections);
            assert!(reader.conn.is_autocommit());
        }
    }
}

#[test]
fn bounded_source_union_rechecks_live_graph_limits_and_preserves_selection_states() {
    // AC-0182: a later oversized graph fails before unbounded graph copy, even
    // when the selected fact remains identical. Missing/absent/invalid states
    // retain existing per-key semantics and raw duplicate keys still cost slots.
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    let mut keys = seed(&mut store);
    keys.push(FactKey::Node {
        id: "missing".into(),
    });
    keys.push(keys[0].clone());
    keys.reverse();
    store
        .conn
        .execute_batch("PRAGMA ignore_check_constraints = ON")
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE source_node_bindings SET receipt_id = ?1",
            ["é".repeat(200)],
        )
        .unwrap();
    let limits = SnapshotReadLimits {
        max_rows: 3,
        ..SnapshotReadLimits::default()
    };
    let before = store
        .read_source_selection_snapshot_bounded(&keys, limits)
        .unwrap();
    assert_eq!(before, store.read_source_selection_snapshot(&keys).unwrap());
    assert_eq!(before.selections.len(), 3);
    assert!(matches!(
        before.selections[0].state,
        FactSourceState::Invalid { .. }
    ));
    assert!(matches!(
        before.selections[1].state,
        FactSourceState::Absent { .. }
    ));
    assert_eq!(before.selections[2].state, FactSourceState::Missing);
    store
        .put_node(&node("new-unselected", Value::Null))
        .unwrap();
    assert_bounds(
        store.read_source_selection_snapshot_bounded(&keys, limits),
        SnapshotBoundsError::RowLimit,
    );
    store.conn.execute("DROP TABLE edges", []).unwrap();
    assert!(matches!(
        store.read_source_selection_snapshot_bounded(&vec![keys[0].clone(); 66], limits),
        Err(GraphError::SourceBinding("too many source selection keys"))
    ));
    assert!(store.conn.is_autocommit());
}

#[test]
fn bounded_snapshot_and_selection_release_transactions_after_interleaving_failures() {
    // AC-0182: both hooks propagate errors once, and rollback releases the read
    // revision so subsequent publication/read operations can proceed normally.
    for source_selection in [false, true] {
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        let keys = seed(&mut store);
        let limits = SnapshotReadLimits::default();
        store.set_snapshot_after_nodes_hook(|| Err(GraphError::SourceBinding("injected")));
        let result = if source_selection {
            store
                .read_source_selection_snapshot_bounded(&keys, limits)
                .map(|snapshot| snapshot.graph)
        } else {
            store.read_snapshot_bounded(limits)
        };
        assert!(matches!(result, Err(GraphError::SourceBinding("injected"))));
        assert!(store.conn.is_autocommit());
        assert_eq!(
            store.read_snapshot_bounded(limits).unwrap(),
            store.read_snapshot().unwrap()
        );
        store.set_source_binding_after_lookup_hook(|| Err(GraphError::SourceBinding("injected")));
        assert!(matches!(
            store.read_source_selection_snapshot_bounded(&keys, limits),
            Err(GraphError::SourceBinding("injected"))
        ));
        assert!(store.conn.is_autocommit());
        assert_eq!(
            store
                .read_source_selection_snapshot_bounded(&keys, limits)
                .unwrap(),
            store.read_source_selection_snapshot(&keys).unwrap()
        );
    }
}

#[test]
fn bounded_source_selection_preflights_schema_bodies_before_exact_schema_decode() {
    // AC-0182: a corrupt oversized owned SQL definition never reaches the
    // ordinary decoder; the legacy path is not relaxed or rewritten.
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    let keys = seed(&mut store);
    let padding = " ".repeat(5000);
    store.conn.execute(&format!("CREATE TRIGGER source_binding_extra AFTER INSERT ON nodes BEGIN SELECT 1;{padding} END"), []).unwrap();
    assert_bounds(
        store.read_source_selection_snapshot_bounded(&keys, SnapshotReadLimits::default()),
        SnapshotBoundsError::InvalidSchema,
    );
    assert!(store.conn.is_autocommit());
}

#[test]
fn bounded_snapshot_default_depth_stops_deep_properties_before_recursive_allocation() {
    // AC-0182: depth is measured from the graph root; depth 32 is admitted and
    // depth 33 is rejected even though serde_json's ordinary limit is higher.
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    store.put_node(&node("a", Value::Null)).unwrap();
    for (arrays, admitted) in [(28, true), (29, false), (200, false)] {
        let raw = format!("{}null{}", "[".repeat(arrays), "]".repeat(arrays));
        store
            .conn
            .execute("UPDATE nodes SET props = ?1", params![raw])
            .unwrap();
        let result = store.read_snapshot_bounded(SnapshotReadLimits::default());
        if admitted {
            assert!(result.is_ok());
        } else {
            assert_bounds(result, SnapshotBoundsError::JsonDepthLimit);
        }
        assert!(store.conn.is_autocommit());
    }
}
