//! Cross-surface source-rule persistence and disclosure regression.

use adapters_lang_ts::{SourceId, extract_source};
use context_hub::{ContextSnapshot, FactKind, QueryRequest};
use core_graph::rules::{
    GuardedExitEvidence, InterpretationStatus, KnownLiteral, LiteralEvidence, LocalExit,
};
use core_graph::{GraphStore, SqliteGraphStore};
use spec::{ExportMode, compile_spec};
use std::collections::BTreeSet;

#[test]
fn source_rules_survive_storage_context_and_exports_without_secret_disclosure() {
    // AC-0124/AC-0125: the actual producer's sanitized facts cross all three
    // consumer boundaries; consumers must not reload raw evidence text.
    let source = br#"function decide(enabled: boolean) {
      if (enabled /* private-comment-canary */) return { password: "tiny-secret-canary", key: "\x67hp_abcdefgh5678" };
      if (!enabled) return false;
    }"#;
    let extracted = extract_source(
        source,
        "source.ts",
        &SourceId {
            repo: "fixture",
            commit: "abc123",
        },
    )
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("graph.db");
    {
        let mut graph = SqliteGraphStore::open(&db).unwrap();
        for node in &extracted.nodes {
            graph.put_node(node).unwrap();
        }
        for edge in &extracted.edges {
            graph.put_edge(edge).unwrap();
        }
    }
    let graph = SqliteGraphStore::open(&db).unwrap();
    let nodes = graph.all_nodes().unwrap();
    let edges = graph.all_edges().unwrap();
    let snapshot = ContextSnapshot::new(nodes.clone(), edges.clone()).unwrap();
    let page = snapshot
        .query(QueryRequest {
            kind: Some(FactKind::Node),
            labels: vec!["BusinessRule".into()],
            ..QueryRequest::default()
        })
        .unwrap();
    assert_eq!(page.facts.len(), 2);
    assert!(page.next_cursor.is_none());
    let mut false_seen = false;
    for fact in &page.facts {
        let rule = GuardedExitEvidence::from_value(fact.properties["rule"].clone()).unwrap();
        assert_eq!(
            rule.interpretation.consumer_effect,
            InterpretationStatus::NotEstablished
        );
        assert!(rule.exit_source.commit_sha == "abc123");
        false_seen |= matches!(rule.effect, LocalExit::Return { value: Some(value) }
            if value.literal == Some(LiteralEvidence::Known { value: KnownLiteral::Boolean(false) }));
    }
    assert!(false_seen);
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let bundle = compile_spec(&nodes, &edges, &[], mode, &BTreeSet::new());
        let inventory = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.file_name == "rule-evidence.md")
            .unwrap();
        assert!(
            inventory
                .content
                .contains("Consumer effect: not established")
        );
        assert!(inventory.content.contains("boolean false"));
        assert!(inventory.content.contains("\\[REDACTED\\]"));
        let all = serde_json::to_string(&(&nodes, &edges, &page, &bundle)).unwrap();
        for secret in [
            "private-comment-canary",
            "tiny-secret-canary",
            "ghp_abcdefgh5678",
            "\\x67hp_abcdefgh5678",
        ] {
            assert!(!all.contains(secret), "leaked synthetic source canary");
        }
    }
}
