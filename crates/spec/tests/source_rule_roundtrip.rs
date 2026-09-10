//! Cross-surface source-rule persistence and disclosure regression.

use adapters_lang_ts::{SourceId, extract_source};
use context_hub::{ContextSnapshot, FactKind, FactReference, QueryRequest};
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
    // Literal and computed callable keys stay out of stored names and owner
    // references; computed placeholders disclose their unresolved runtime keys.
    let source = br#"function decide(enabled: boolean) {
      if (enabled /* private-comment-canary */) return { password: "tiny-secret-canary", key: "\x67hp_abcdefgh5678" };
      if (!enabled) return false;
    }
    class Actions {
      "ghp_classkey1234"(enabled: boolean) { if (enabled) return false; }
      "\x67hp_classkey5678"(enabled: boolean) { if (enabled) return null; }
      [runtimeClassKeyCanary](enabled: boolean) { if (enabled) return false; }
    }
    const actions = {
      "sk-objectkey1234"(enabled: boolean) { if (enabled) return false; },
      "\x73k-objectkey5678"(enabled: boolean) { if (enabled) return null; },
      "ghp_callback1234": (enabled: boolean) => { if (enabled) return false; },
      "\x67hp_callback5678": function(enabled: boolean) { if (enabled) return null; },
      ["ghp_computedobject1234"](enabled: boolean) { if (enabled) return false; },
      ["\x67hp_computedarrow5678"]: (enabled: boolean) => { if (enabled) return false; },
      [runtimeCallbackKeyCanary]: function(enabled: boolean) { if (enabled) return null; }
    };"#;
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
    assert_eq!(page.facts.len(), 12);
    assert!(page.next_cursor.is_none());
    let computed_ids: BTreeSet<_> = nodes
        .iter()
        .filter(|node| node.label == "Symbol" && node.props["computed_name"] == true)
        .map(|node| node.id.clone())
        .collect();
    assert_eq!(computed_ids.len(), 4);
    let symbols = snapshot
        .query(QueryRequest {
            kind: Some(FactKind::Node),
            labels: vec!["Symbol".into()],
            ..QueryRequest::default()
        })
        .unwrap();
    assert!(symbols.next_cursor.is_none());
    let context_computed: BTreeSet<_> = symbols
        .facts
        .iter()
        .filter(|fact| fact.properties["computed_name"] == true)
        .map(|fact| fact.reference.clone())
        .collect();
    assert_eq!(
        context_computed,
        computed_ids
            .iter()
            .map(|id| FactReference::Node { id: id.clone() })
            .collect()
    );
    let mut false_seen = false;
    let mut computed_rule_owners = BTreeSet::new();
    for fact in &page.facts {
        let rule = GuardedExitEvidence::from_value(fact.properties["rule"].clone()).unwrap();
        assert_eq!(
            rule.interpretation.consumer_effect,
            InterpretationStatus::NotEstablished
        );
        assert!(rule.exit_source.commit_sha == "abc123");
        if computed_ids.contains(&rule.owner_id) {
            computed_rule_owners.insert(rule.owner_id.clone());
        }
        false_seen |= matches!(rule.effect, LocalExit::Return { value: Some(value) }
            if value.literal == Some(LiteralEvidence::Known { value: KnownLiteral::Boolean(false) }));
    }
    assert!(false_seen);
    assert_eq!(computed_rule_owners, computed_ids);
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
        assert_eq!(
            inventory
                .content
                .lines()
                .filter(|line| line.starts_with("Owner:")
                    && line.ends_with("(computed source name omitted; runtime key unresolved)"))
                .count(),
            4
        );
        assert_eq!(
            inventory
                .content
                .lines()
                .filter(|line| line.starts_with("Owner:") && line.ends_with("(source name omitted)"))
                .count(),
            6
        );
        assert!(inventory.content.contains("\\[REDACTED\\]"));
        let all = serde_json::to_string(&(&nodes, &edges, &page, &symbols, &bundle)).unwrap();
        for secret in [
            "private-comment-canary",
            "tiny-secret-canary",
            "ghp_abcdefgh5678",
            "\\x67hp_abcdefgh5678",
            "ghp_classkey1234",
            "classkey5678",
            "sk-objectkey1234",
            "objectkey5678",
            "ghp_callback1234",
            "callback5678",
            "runtimeClassKeyCanary",
            "ghp_computedobject1234",
            "computedarrow5678",
            "runtimeCallbackKeyCanary",
        ] {
            assert!(!all.contains(secret), "leaked synthetic source canary");
        }
    }
}
