use super::*;
use crate::{GRAPH_SCHEMA_VERSION, GraphStore};

fn node(id: &str) -> Node {
    Node {
        id: id.into(),
        label: "Symbol".into(),
        props: serde_json::json!({"prov": {"content_hash": "semantic-only"}, "name": id}),
    }
}

fn edge(id: &str) -> Edge {
    Edge {
        src: id.into(),
        dst: id.into(),
        label: "GOVERNS".into(),
        props: serde_json::json!({"observation": "lexical_owner"}),
    }
}

fn node_binding(node: &Node, repo: &str, receipt: &str) -> SourceBinding {
    SourceBinding {
        fact: FactKey::from_node(node),
        repo_key: repo.into(),
        receipt_id: format!(
            "ts-primary-v1:{}",
            core_prov::content_hash(receipt.as_bytes())
        ),
        emitted_fact_digest: node_digest(node).unwrap(),
    }
}

fn edge_binding(edge: &Edge, repo: &str) -> SourceBinding {
    SourceBinding {
        fact: FactKey::from_edge(edge),
        repo_key: repo.into(),
        receipt_id: format!("ts-primary-v1:{}", core_prov::content_hash(repo.as_bytes())),
        emitted_fact_digest: edge_digest(edge).unwrap(),
    }
}

fn publish(store: &mut SqliteGraphStore, repo: &str, bindings: &[SourceBinding]) {
    let snapshot = store.read_snapshot().unwrap();
    assert!(
        store
            .apply_patch_with_source_bindings_if_snapshot_matches(
                &snapshot,
                &GraphPatch::default(),
                repo,
                bindings,
            )
            .unwrap()
    );
}

fn seed(
    store: &mut SqliteGraphStore,
) -> (SourceBinding, SourceBinding, SourceBinding, SourceBinding) {
    for id in ["a", "b"] {
        store.put_node(&node(id)).unwrap();
        store.put_edge(&edge(id)).unwrap();
    }
    let a = node_binding(&node("a"), "repo/a", "a-original");
    let ae = edge_binding(&edge("a"), "repo/a");
    let b = node_binding(&node("b"), "repo/b", "b-original");
    let be = edge_binding(&edge("b"), "repo/b");
    publish(store, "repo/a", &[a.clone(), ae.clone()]);
    publish(store, "repo/b", &[b.clone(), be.clone()]);
    (a, ae, b, be)
}

#[test]
fn complete_fact_digests_bind_type_all_properties_and_array_order() {
    // AC-0149, AC-0150: this digest includes all emitted fields, independently
    // of the producer's often narrower prov.content_hash.
    let mut original = node("same");
    original.props = serde_json::from_str(
        r#"{"z":[1,2],"nested":{"b":2,"a":1},"prov":{"content_hash":"unchanged"}}"#,
    )
    .unwrap();
    let mut reordered = original.clone();
    reordered.props = serde_json::from_str(
        r#"{"prov":{"content_hash":"unchanged"},"nested":{"a":1,"b":2},"z":[1,2]}"#,
    )
    .unwrap();
    let digest = node_digest(&original).unwrap();
    assert_eq!(node_digest(&reordered).unwrap(), digest);
    for field in ["id", "label", "property", "array", "provenance"] {
        let mut changed = original.clone();
        match field {
            "id" => changed.id = "different".into(),
            "label" => changed.label = "Component".into(),
            "property" => changed.props["nested"]["a"] = 3.into(),
            "array" => changed.props["z"] = serde_json::json!([2, 1]),
            "provenance" => changed.props["prov"]["tier"] = "Agentic".into(),
            _ => unreachable!(),
        }
        assert_ne!(node_digest(&changed).unwrap(), digest, "{field}");
    }
    let original_edge = edge("same");
    let edge_hash = edge_digest(&original_edge).unwrap();
    assert_ne!(digest, edge_hash);
    for field in ["source", "destination", "label", "property"] {
        let mut changed = original_edge.clone();
        match field {
            "source" => changed.src = "other".into(),
            "destination" => changed.dst = "other".into(),
            "label" => changed.label = "CALLS".into(),
            "property" => changed.props["extra"] = true.into(),
            _ => unreachable!(),
        }
        assert_ne!(edge_digest(&changed).unwrap(), edge_hash, "{field}");
    }
    assert_eq!(
        serde_json::to_value(FactKey::from_node(&original)).unwrap(),
        serde_json::json!({"kind":"node", "id":"same"})
    );
    assert_eq!(
        serde_json::to_value(FactKey::from_edge(&original_edge)).unwrap(),
        serde_json::json!({"kind":"edge", "source":"same", "label":"GOVERNS", "destination":"same"})
    );
}

#[test]
fn identical_facts_switch_receipts_atomically_without_changing_other_repos() {
    // AC-0150: different captures can emit identical full facts. The explicit
    // current receipt changes, while graph hashes and other repos stay intact.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("graph.db");
    let mut store = SqliteGraphStore::open(&path).unwrap();
    let (a, ae, b, be) = seed(&mut store);
    let snapshot = store.read_snapshot().unwrap();
    let replacement = node_binding(&node("a"), "repo/a", "a-new-capture");
    assert_eq!(a.emitted_fact_digest, replacement.emitted_fact_digest);
    assert_ne!(a.receipt_id, replacement.receipt_id);
    publish(&mut store, "repo/a", std::slice::from_ref(&replacement));
    assert_eq!(store.read_snapshot().unwrap(), snapshot);
    assert_eq!(
        store.current_source_binding(&a.fact).unwrap(),
        Some(replacement.clone())
    );
    assert_eq!(
        store.current_source_binding(&ae.fact).unwrap(),
        None,
        "new nonparticipating facts lose old associations"
    );
    assert_eq!(
        store.source_bindings_for_repo("repo/b").unwrap(),
        vec![b.clone(), be.clone()]
    );
    drop(store);
    let mut store = SqliteGraphStore::open(&path).unwrap();
    assert_eq!(
        store.current_source_binding(&a.fact).unwrap(),
        Some(replacement)
    );
    publish(&mut store, "repo/a", &[]);
    assert!(store.source_bindings_for_repo("repo/a").unwrap().is_empty());
    assert_eq!(
        store.source_bindings_for_repo("repo/b").unwrap(),
        vec![b, be]
    );
    assert_eq!(store.read_snapshot().unwrap(), snapshot);
}

#[test]
fn ordinary_graph_mutations_invalidate_touched_source_associations() {
    // AC-0150: even an identical ordinary upsert is a new unbound publication;
    // unrelated facts keep their bindings and graph clear removes only current metadata.
    for action in [
        "node",
        "edge",
        "delete_edge",
        "delete_label",
        "delete_node",
        "patch",
        "clear",
    ] {
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        let (a, ae, b, be) = seed(&mut store);
        match action {
            "node" => store.put_node(&node("a")).unwrap(),
            "edge" => store.put_edge(&edge("a")).unwrap(),
            "delete_edge" => store.delete_edge("a", "a", "GOVERNS").unwrap(),
            "delete_label" => store.delete_edges_from_with_label("a", "GOVERNS").unwrap(),
            "delete_node" => store.delete_node("a").unwrap(),
            "patch" => {
                let snapshot = store.read_snapshot().unwrap();
                assert!(
                    store
                        .apply_patch_if_snapshot_matches(
                            &snapshot,
                            &GraphPatch {
                                upsert_nodes: vec![node("a")],
                                ..GraphPatch::default()
                            }
                        )
                        .unwrap()
                );
            }
            "clear" => store.clear().unwrap(),
            _ => unreachable!(),
        }
        let node_survives = matches!(action, "edge" | "delete_edge" | "delete_label");
        let edge_survives = matches!(action, "node" | "patch");
        assert_eq!(
            store.current_source_binding(&a.fact).unwrap().is_some(),
            node_survives,
            "{action}"
        );
        assert_eq!(
            store.current_source_binding(&ae.fact).unwrap().is_some(),
            edge_survives,
            "{action}"
        );
        assert_eq!(
            store.source_bindings_for_repo("repo/b").unwrap(),
            if action == "clear" {
                vec![]
            } else {
                vec![b, be]
            }
        );
    }
}

#[test]
fn raw_second_connection_writes_invalidate_without_foreign_key_pragmas() {
    // AC-0150: triggers cover independent connections, including REPLACE with
    // recursive_triggers/foreign_keys disabled and node deletion leaving an edge.
    for (sql, node_survives, edge_survives) in [
        ("UPDATE nodes SET props = props WHERE id = 'a'", false, true),
        (
            "INSERT OR REPLACE INTO nodes(id,label,props) VALUES ('a','Symbol','{}')",
            false,
            false,
        ),
        (
            "UPDATE edges SET props = props WHERE src = 'a'",
            true,
            false,
        ),
        (
            "INSERT OR REPLACE INTO edges(src,dst,label,props) VALUES ('a','a','GOVERNS','{}')",
            true,
            false,
        ),
        ("DELETE FROM nodes WHERE id = 'a'", false, false),
        ("DELETE FROM edges WHERE src = 'a'", true, false),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("graph.db");
        let mut store = SqliteGraphStore::open(&path).unwrap();
        let (a, ae, b, be) = seed(&mut store);
        let writer = Connection::open(&path).unwrap();
        writer.pragma_update(None, "foreign_keys", "OFF").unwrap();
        writer
            .pragma_update(None, "recursive_triggers", "OFF")
            .unwrap();
        writer.execute_batch(sql).unwrap();
        assert_eq!(
            store.current_source_binding(&a.fact).unwrap().is_some(),
            node_survives,
            "{sql}"
        );
        assert_eq!(
            store.current_source_binding(&ae.fact).unwrap().is_some(),
            edge_survives,
            "{sql}"
        );
        assert_eq!(
            store.source_bindings_for_repo("repo/b").unwrap(),
            vec![b, be]
        );
    }
}

#[test]
fn source_publication_rolls_back_facts_and_bindings_on_validation_or_write_failure() {
    // AC-0150: clearing old bindings, patch writes and final binding validation
    // are one transaction, including late missing-fact/digest/FK failures.
    for failure in ["missing", "digest", "edge"] {
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        let (a, ae, b, be) = seed(&mut store);
        let snapshot = store.read_snapshot().unwrap();
        let mut changed = node("a");
        changed.props["name"] = "changed".into();
        let mut patch = GraphPatch {
            upsert_nodes: vec![changed.clone()],
            delete_edges: vec![("a".into(), "a".into(), "GOVERNS".into())],
            ..GraphPatch::default()
        };
        let binding = match failure {
            "missing" => node_binding(&node("missing"), "repo/a", "missing"),
            "digest" => a.clone(),
            "edge" => {
                patch.upsert_edges.push(Edge {
                    dst: "missing".into(),
                    ..edge("a")
                });
                node_binding(&changed, "repo/a", "changed")
            }
            _ => unreachable!(),
        };
        assert!(
            store
                .apply_patch_with_source_bindings_if_snapshot_matches(
                    &snapshot,
                    &patch,
                    "repo/a",
                    &[binding]
                )
                .is_err()
        );
        assert!(store.conn.is_autocommit());
        assert_eq!(store.read_snapshot().unwrap(), snapshot);
        assert_eq!(
            store.source_bindings_for_repo("repo/a").unwrap(),
            vec![a, ae]
        );
        assert_eq!(
            store.source_bindings_for_repo("repo/b").unwrap(),
            vec![b, be]
        );
    }
}

#[test]
fn source_publication_rejects_stale_duplicate_and_cross_repo_bindings() {
    // AC-0150: stale input is a no-write false result; malformed ownership and
    // attempted takeover are errors, including when a patch would erase old metadata.
    let mut store = SqliteGraphStore::open_in_memory().unwrap();
    let (a, ae, b, be) = seed(&mut store);
    let stale = store.read_snapshot().unwrap();
    store.put_node(&node("unrelated")).unwrap();
    let current = store.read_snapshot().unwrap();
    assert!(
        !store
            .apply_patch_with_source_bindings_if_snapshot_matches(
                &stale,
                &GraphPatch::default(),
                "repo/a",
                &[]
            )
            .unwrap()
    );
    for bindings in [vec![a.clone(), a.clone()], vec![b.clone()]] {
        assert!(
            store
                .apply_patch_with_source_bindings_if_snapshot_matches(
                    &current,
                    &GraphPatch::default(),
                    "repo/a",
                    &bindings
                )
                .is_err()
        );
    }
    let takeover = SourceBinding {
        repo_key: "repo/a".into(),
        ..b.clone()
    };
    let patch = GraphPatch {
        upsert_nodes: vec![node("b")],
        ..GraphPatch::default()
    };
    assert!(
        store
            .apply_patch_with_source_bindings_if_snapshot_matches(
                &current,
                &patch,
                "repo/a",
                &[takeover]
            )
            .is_err()
    );
    assert_eq!(store.read_snapshot().unwrap(), current);
    assert_eq!(
        store.source_bindings_for_repo("repo/a").unwrap(),
        vec![a, ae]
    );
    assert_eq!(
        store.source_bindings_for_repo("repo/b").unwrap(),
        vec![b, be]
    );
}

#[test]
fn current_binding_read_is_coherent_across_another_connection_publication() {
    // AC-0150, AC-0152: a second connection can publish between association and
    // fact lookup. The result must be wholly old or wholly new, including an
    // identical-fact receipt replacement that a graph snapshot hash cannot see.
    for change_fact in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("graph.db");
        let mut store = SqliteGraphStore::open(&path).unwrap();
        let (a, _, _, _) = seed(&mut store);
        let mut writer = SqliteGraphStore::open(&path).unwrap();
        let mut changed = node("a");
        if change_fact {
            changed.props["name"] = "new contents".into();
        }
        let replacement = node_binding(&changed, "repo/a", "new capture");
        let after = replacement.clone();
        store.set_source_binding_after_lookup_hook(move || {
            let expected = writer.read_snapshot()?;
            let patch = if change_fact {
                GraphPatch {
                    upsert_nodes: vec![changed],
                    ..GraphPatch::default()
                }
            } else {
                GraphPatch::default()
            };
            assert!(writer.apply_patch_with_source_bindings_if_snapshot_matches(
                &expected,
                &patch,
                "repo/a",
                &[replacement]
            )?);
            Ok(())
        });
        assert_eq!(
            store.current_source_binding(&a.fact).unwrap(),
            Some(a.clone())
        );
        assert!(store.conn.is_autocommit());
        assert_eq!(store.current_source_binding(&a.fact).unwrap(), Some(after));
    }
}

#[test]
fn current_binding_rejects_corrupt_digest_or_missing_fact_and_releases_transaction() {
    // AC-0150, AC-0152: valid-looking stored metadata cannot certify changed or
    // missing facts. Failure releases its read transaction for later operations.
    for missing in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("graph.db");
        let mut store = SqliteGraphStore::open(&path).unwrap();
        let (a, _, _, _) = seed(&mut store);
        let writer = Connection::open(&path).unwrap();
        writer.pragma_update(None, "foreign_keys", "OFF").unwrap();
        if missing {
            writer
                .execute("DELETE FROM nodes WHERE id = 'a'", [])
                .unwrap();
            writer.execute("INSERT INTO source_node_bindings(node_id,repo_key,receipt_id,emitted_fact_digest) VALUES ('a',?1,?2,?3)", params![a.repo_key, a.receipt_id, a.emitted_fact_digest]).unwrap();
        } else {
            writer
                .execute(
                    "UPDATE source_node_bindings SET emitted_fact_digest = ?1 WHERE node_id = 'a'",
                    [node_digest(&node("different")).unwrap()],
                )
                .unwrap();
        }
        assert!(matches!(
            store.current_source_binding(&a.fact),
            Err(GraphError::SourceBinding(_))
        ));
        assert!(store.conn.is_autocommit());
        store.set_source_binding_after_lookup_hook(|| {
            Err(GraphError::SourceBinding("test hook failure"))
        });
        assert!(
            store
                .current_source_binding(&FactKey::from_node(&node("b")))
                .is_err()
        );
        assert!(store.conn.is_autocommit());
        assert!(
            store
                .current_source_binding(&FactKey::from_node(&node("b")))
                .unwrap()
                .is_some()
        );
    }
}

#[test]
fn association_schema_migrates_without_clearing_schema_four_facts_and_rejects_incompatibility() {
    // AC-0150: private-table migration preserves existing version-four facts;
    // partial/future schemas and missing invalidation triggers fail, not repair.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-graph.db");
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TABLE nodes (id TEXT PRIMARY KEY, label TEXT NOT NULL, props TEXT NOT NULL DEFAULT '{}') STRICT; CREATE TABLE edges (src TEXT NOT NULL REFERENCES nodes(id), dst TEXT NOT NULL REFERENCES nodes(id), label TEXT NOT NULL, props TEXT NOT NULL DEFAULT '{}', PRIMARY KEY (src,dst,label)) STRICT; INSERT INTO nodes(id,label) VALUES ('retained','Symbol'); INSERT INTO edges(src,dst,label) VALUES ('retained','retained','CALLS');").unwrap();
    connection
        .pragma_update(None, "user_version", GRAPH_SCHEMA_VERSION)
        .unwrap();
    drop(connection);
    let store = SqliteGraphStore::open(&path).unwrap();
    assert_eq!(store.fact_counts().unwrap(), (1, 1));
    assert!(store.source_bindings_for_repo("repo/a").unwrap().is_empty());
    drop(store);
    for corruption in [
        "DROP TABLE source_binding_meta",
        "PRAGMA ignore_check_constraints=ON; UPDATE source_binding_meta SET version=2",
        "DROP TRIGGER source_binding_node_update",
        "CREATE VIEW source_binding_unknown AS SELECT 1",
        "CREATE UNIQUE INDEX unrelated_unique_repo ON source_node_bindings(repo_key)",
        "CREATE TRIGGER unrelated_binding_write AFTER INSERT ON source_node_bindings BEGIN DELETE FROM source_node_bindings WHERE node_id=NEW.node_id; END",
    ] {
        let path = directory.path().join(format!(
            "corrupt-{}.db",
            core_prov::content_hash(corruption.as_bytes())
        ));
        let mut store = SqliteGraphStore::open(&path).unwrap();
        seed(&mut store);
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(corruption).unwrap();
        assert!(
            store
                .current_source_binding(&FactKey::from_node(&node("a")))
                .is_err()
        );
        assert!(SqliteGraphStore::open(&path).is_err(), "{corruption}");
        assert_eq!(
            store.fact_counts().unwrap(),
            (2, 2),
            "failed migration must not clear facts"
        );
    }
}
