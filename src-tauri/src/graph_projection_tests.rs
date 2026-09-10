//! Real legacy projection adapters over deterministically interleaved WAL reads.

use core_graph::{Edge, GraphError, GraphStore, Node, SqliteGraphStore};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use spec::ExportMode;
use std::path::Path;
use std::sync::Mutex;

type GraphFacts = (Vec<Node>, Vec<Edge>);

fn provenance(id: &str, tier: Tier, confidence: ConfidenceTier) -> Provenance {
    Provenance::new(
        tier,
        confidence,
        vec![EvidenceRef {
            repo: "fixture/snapshot".into(),
            path: "source.ts".into(),
            byte_start: 0,
            byte_end: 12,
            commit_sha: "workdir".into(),
        }],
        "snapshot.fixture",
        id.as_bytes(),
    )
    .unwrap()
}

fn fixture(revision: &str) -> GraphFacts {
    let node = |suffix: &str, label: &str, tier, confidence, mut props: Value| {
        let id = format!("{revision}:{suffix}");
        props["prov"] = serde_json::to_value(provenance(&id, tier, confidence)).unwrap();
        Node {
            id,
            label: label.into(),
            props,
        }
    };
    let resource_props = |suffix: &str| json!({"type": "aws_sqs_queue", "logical_id": format!("{revision}_{suffix}")});
    let nodes = vec![
        node(
            "resource",
            "Resource",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            resource_props("queue"),
        ),
        node(
            "channel",
            "Channel",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            json!({"kind": "sqs", "identity": format!("{revision}.orders")}),
        ),
        node(
            "endpoint",
            "Endpoint",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            json!({"method": "POST", "path": format!("/{revision}/orders")}),
        ),
        node(
            "handler",
            "Symbol",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            json!({"name": format!("{revision}_handler")}),
        ),
        node(
            "gap",
            "Gap",
            Tier::Deterministic,
            ConfidenceTier::Gap,
            json!({"reason": format!("{revision} unresolved call target")}),
        ),
        node(
            "weak",
            "Resource",
            Tier::Agentic,
            ConfidenceTier::InferredWeak,
            resource_props("weak"),
        ),
        node(
            "strong",
            "Resource",
            Tier::Semantic,
            ConfidenceTier::InferredStrong,
            resource_props("strong"),
        ),
        node(
            "rejected",
            "Resource",
            Tier::Semantic,
            ConfidenceTier::InferredStrong,
            resource_props("rejected"),
        ),
    ];
    let edges = [
        (
            "endpoint",
            "handler",
            "HANDLES",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        ),
        (
            "handler",
            "channel",
            "PUBLISHES",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        ),
        (
            "handler",
            "gap",
            "CALLS",
            Tier::Deterministic,
            ConfidenceTier::Gap,
        ),
        (
            "resource",
            "channel",
            "BACKS",
            Tier::Dynamic,
            ConfidenceTier::Confirmed,
        ),
    ]
    .into_iter()
    .map(|(src, dst, label, tier, confidence)| {
        let src = format!("{revision}:{src}");
        let dst = format!("{revision}:{dst}");
        Edge {
            props: json!({
                "prov": provenance(&format!("{src} {label} {dst}"), tier, confidence),
                "reason": format!("{revision} unresolved call target"),
            }),
            src,
            dst,
            label: label.into(),
        }
    })
    .collect();
    (nodes, edges)
}

fn replace_graph(writer: &mut Connection, facts: &GraphFacts) -> Result<(), GraphError> {
    let tx = writer.transaction()?;
    tx.execute("DELETE FROM edges", [])?;
    tx.execute("DELETE FROM nodes", [])?;
    // Deliberately insert out of stable id order: adapters must retain the
    // store's deterministic ordering, including after the replacement.
    for node in facts.0.iter().rev() {
        tx.execute(
            "INSERT INTO nodes (id, label, props) VALUES (?1, ?2, ?3)",
            params![node.id, node.label, serde_json::to_string(&node.props)?],
        )?;
    }
    for edge in facts.1.iter().rev() {
        tx.execute(
            "INSERT INTO edges (src, dst, label, props) VALUES (?1, ?2, ?3, ?4)",
            params![
                edge.src,
                edge.dst,
                edge.label,
                serde_json::to_string(&edge.props)?
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn database(facts: &GraphFacts) -> (tempfile::TempDir, SqliteGraphStore, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.db");
    let reader = SqliteGraphStore::open(&path).unwrap();
    let mut writer = Connection::open(&path).unwrap();
    writer.pragma_update(None, "foreign_keys", "ON").unwrap();
    replace_graph(&mut writer, facts).unwrap();
    (dir, reader, writer)
}

fn memory_graph(facts: &GraphFacts) -> SqliteGraphStore {
    let mut graph = SqliteGraphStore::open_in_memory().unwrap();
    for node in &facts.0 {
        graph.put_node(node).unwrap();
    }
    for edge in &facts.1 {
        graph.put_edge(edge).unwrap();
    }
    graph
}

fn decisions(path: &Path, old: &GraphFacts, new: &GraphFacts) -> agents::DecisionLog {
    let mut decisions = agents::DecisionLog::open(path).unwrap();
    for node in old.0.iter().chain(&new.0) {
        if node.id.ends_with(":rejected") {
            decisions
                .record_assertion(
                    &agents::CuratableAssertion {
                        subject_id: node.id.clone(),
                        summary: "Rejected fixture inference".into(),
                        provenance: serde_json::from_value(node.props["prov"].clone()).unwrap(),
                    },
                    agents::AssertionDecision::Rejected,
                    None,
                )
                .unwrap();
        }
    }
    decisions
}

fn install_swap(reader: &SqliteGraphStore, mut writer: Connection, new: GraphFacts) {
    reader.set_snapshot_after_nodes_hook(move || replace_graph(&mut writer, &new));
}

fn assert_spec_policy(bundle: &spec::SpecBundle, revision: &str) {
    let has_subject = |suffix: &str| {
        bundle
            .artifacts
            .iter()
            .flat_map(|artifact| &artifact.assertions)
            .any(|assertion| assertion.subject_id == format!("{revision}:{suffix}"))
    };
    assert!(has_subject("resource"));
    assert!(has_subject("strong"));
    assert_eq!(has_subject("weak"), bundle.mode == ExportMode::BestEffort);
    assert!(!has_subject("rejected"));
    assert!(bundle.gap_count > 0);
    assert!(bundle.artifacts.iter().any(|artifact| {
        artifact
            .assertions
            .iter()
            .any(|assertion| assertion.provenance.tier == Tier::Dynamic)
    }));
}

#[test]
fn spec_bundle_uses_one_revision_across_another_connection_commit() {
    // AC-0138: exercise the real storage -> flow tracer -> compiler adapter,
    // including R-INT-5 and existing rejection filters. Review state is fixed;
    // this test makes no cross-database transaction or atomic-ingest claim.
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let old = fixture("old");
        let new = fixture("new");
        let (dir, reader, writer) = database(&old);
        let decisions = decisions(&dir.path().join("decisions.db"), &old, &new);
        let before = crate::build_spec_bundle(&reader, &decisions, mode).unwrap();
        let expected_after =
            crate::build_spec_bundle(&memory_graph(&new), &decisions, mode).unwrap();
        assert_ne!(before, expected_after);
        assert_spec_policy(&before, "old");
        assert_spec_policy(&expected_after, "new");

        install_swap(&reader, writer, new);
        let during = crate::build_spec_bundle(&reader, &decisions, mode).unwrap();
        assert_eq!(during, before);
        // This also fails if the adapter bypasses the snapshot hook entirely.
        let after = crate::build_spec_bundle(&reader, &decisions, mode).unwrap();
        assert_eq!(after, expected_after);
    }
}

#[test]
fn atlas_snapshot_uses_one_revision_across_another_connection_commit() {
    // AC-0138: the Atlas must return full old or full new facts, never old
    // endpoints with new edges. Equality covers ordering and full provenance.
    let old = fixture("old");
    let new = fixture("new");
    let (_dir, reader, writer) = database(&old);
    let before = crate::build_atlas_snapshot(&reader).unwrap();
    let expected_after = crate::build_atlas_snapshot(&memory_graph(&new)).unwrap();
    assert_ne!(before, expected_after);
    assert!(before.nodes.windows(2).all(|pair| pair[0].id < pair[1].id));
    assert!(before.edges.windows(2).all(|pair| {
        (&pair[0].src, &pair[0].dst, &pair[0].label) < (&pair[1].src, &pair[1].dst, &pair[1].label)
    }));

    install_swap(&reader, writer, new);
    assert_eq!(crate::build_atlas_snapshot(&reader).unwrap(), before);
    assert_eq!(
        crate::build_atlas_snapshot(&reader).unwrap(),
        expected_after
    );
}

#[derive(Clone, Copy, Debug)]
enum Projection {
    Atlas,
    Spec(ExportMode),
}

fn project(
    projection: Projection,
    graph: &impl GraphStore,
    decisions: &agents::DecisionLog,
) -> Result<Value, String> {
    match projection {
        Projection::Atlas => Ok(serde_json::to_value(crate::build_atlas_snapshot(graph)?).unwrap()),
        Projection::Spec(mode) => {
            Ok(serde_json::to_value(crate::build_spec_bundle(graph, decisions, mode)?).unwrap())
        }
    }
}

#[test]
fn spec_and_atlas_recover_after_snapshot_property_errors() {
    // AC-0139: both collection failures must propagate through real adapters.
    // Repair through the independent writer; the same reader must then start
    // a new transaction and observe the replacement, not a retained revision.
    for projection in [
        Projection::Atlas,
        Projection::Spec(ExportMode::VerifiedOnly),
        Projection::Spec(ExportMode::BestEffort),
    ] {
        for table in ["nodes", "edges"] {
            let old = fixture("old");
            let new = fixture("new");
            let (dir, reader, mut writer) = database(&old);
            let decisions = decisions(&dir.path().join("decisions.db"), &old, &new);
            let before = project(projection, &reader, &decisions).unwrap();
            let expected_after = project(projection, &memory_graph(&new), &decisions).unwrap();
            assert_ne!(before, expected_after);
            writer
                .execute(&format!("UPDATE {table} SET props = 'invalid'"), [])
                .unwrap();
            let error = project(projection, &reader, &decisions).unwrap_err();
            assert!(
                error.starts_with("props:"),
                "{projection:?}/{table}: {error}"
            );
            replace_graph(&mut writer, &new).unwrap();
            assert_eq!(
                project(projection, &reader, &decisions).unwrap(),
                expected_after,
                "{projection:?}/{table} retained a failed transaction"
            );
        }
    }
}

#[test]
fn flow_projection_preserves_selection_and_output_across_snapshot_swap() {
    // AC-0138/AC-0139: the shared reader used by flow export, flow listing and
    // anchor counts preserves their selected labels and skips malformed JSON
    // outside that selection. Compare actual consumer outputs to the previous
    // label-concatenated input order as well as across a committed replacement.
    let old = fixture("old");
    let new = fixture("new");
    let (_dir, reader, writer) = database(&old);
    writer
        .execute(
            "INSERT INTO nodes (id, label, props) VALUES ('ignored', 'Config', 'invalid')",
            [],
        )
        .unwrap();
    writer
        .execute(
            "INSERT INTO edges (src, dst, label, props)
             VALUES ('ignored', 'ignored', 'UNRELATED', 'invalid')",
            [],
        )
        .unwrap();
    assert!(reader.read_snapshot().is_err());
    let mut legacy_nodes = Vec::new();
    for label in flowtracer::FLOW_NODE_LABELS {
        legacy_nodes.extend(reader.nodes_with_label(label).unwrap());
    }
    let legacy_edges = reader
        .edges_with_labels(flowtracer::FLOW_EDGE_LABELS)
        .unwrap();
    let render = |nodes: &[Node], edges: &[Edge]| {
        let flows = flowtracer::trace(nodes, edges);
        json!({
            "dossier": spec::flow_dossier(&flows),
            "flows": flows,
            "anchors": flowtracer::anchor_probes(nodes, edges),
        })
    };
    let before = render(&legacy_nodes, &legacy_edges);
    let expected_after = render(&new.0, &new.1);
    assert_ne!(before, expected_after);
    install_swap(&reader, writer, new);
    let reader = Mutex::new(reader);
    let (nodes, edges) = crate::read_flow_graph(&reader).unwrap();
    assert_eq!(nodes, legacy_nodes);
    assert_eq!(edges, legacy_edges);
    assert!(
        nodes
            .iter()
            .all(|node| flowtracer::FLOW_NODE_LABELS.contains(&node.label.as_str()))
    );
    assert!(
        edges
            .iter()
            .all(|edge| flowtracer::FLOW_EDGE_LABELS.contains(&edge.label.as_str()))
    );
    assert_eq!(render(&nodes, &edges), before);
    let (nodes, edges) = crate::read_flow_graph(&reader).unwrap();
    assert_eq!(render(&nodes, &edges), expected_after);
}

#[test]
fn filtered_projection_preserves_topology_and_semantic_input_order() {
    // AC-0138/AC-0139: semantic candidates must retain the former flow-label
    // order followed by Resource; topology retains Resource then Channel.
    // SQL selects only those labels, so malformed unselected facts stay inert.
    let mut semantic_labels = flowtracer::FLOW_NODE_LABELS.to_vec();
    semantic_labels.push("Resource");
    for (node_labels, edge_labels) in [
        (&["Resource", "Channel"][..], spec::TOPOLOGY_EDGE_LABELS),
        (semantic_labels.as_slice(), flowtracer::FLOW_EDGE_LABELS),
    ] {
        let old = fixture("old");
        let new = fixture("new");
        let (_dir, reader, writer) = database(&old);
        writer
            .execute(
                "INSERT INTO nodes (id, label, props) VALUES ('ignored', 'Config', 'invalid')",
                [],
            )
            .unwrap();
        writer
            .execute(
                "INSERT INTO edges (src, dst, label, props)
                 VALUES ('ignored', 'ignored', 'UNRELATED', 'invalid')",
                [],
            )
            .unwrap();
        assert!(reader.read_snapshot().is_err());
        let legacy_selection = |graph: &SqliteGraphStore| {
            let mut nodes = Vec::new();
            for label in node_labels {
                nodes.extend(graph.nodes_with_label(label).unwrap());
            }
            (nodes, graph.edges_with_labels(edge_labels).unwrap())
        };
        let before = legacy_selection(&reader);
        let expected_after = legacy_selection(&memory_graph(&new));
        assert_ne!(before, expected_after);
        install_swap(&reader, writer, new);
        assert_eq!(
            crate::read_filtered_graph(&reader, node_labels, edge_labels).unwrap(),
            before
        );
        assert_eq!(
            crate::read_filtered_graph(&reader, node_labels, edge_labels).unwrap(),
            expected_after
        );
    }
}
