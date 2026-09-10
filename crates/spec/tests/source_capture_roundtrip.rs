//! Exercise immutable capture bytes with a real producer, without claiming
//! production ingestion or attaching verification to legacy provenance.

use adapters_lang_ts::{SourceId as ParserSourceId, extract_source};
use context_hub::{ContextSnapshot, FactReference, QueryRequest};
use core_graph::rules::{GuardedExitEvidence, InterpretationStatus};
use core_graph::{GraphStore, SqliteGraphStore};
use core_prov::EvidenceRef;
use source_capture::{
    CaptureLimits, CaptureStore, SourceId as CaptureSourceId, StoreLimits, capture_working_tree,
};
use spec::{ExportMode, compile_spec};
use std::collections::BTreeSet;

#[test]
fn captured_local_definitions_retain_original_conditions_citations_and_export_limits() {
    // AC-0169/AC-0170: the real captured producer's definition evidence crosses
    // receipt validation, durable capture/graph storage, context and both export
    // modes. This fixture does not establish complete production input closure.
    use adapters_lang_ts::captured::{Receipt, extract_file};
    use core_graph::GraphPatch;
    use core_graph::source::SourceBinding;
    use core_prov::{ConfidenceTier, Tier};

    let source = concat!(
        "// café — definition-source-comment-canary\n",
        "function decide(item: { enabled: boolean }, ready: boolean) {\n",
        "  const allowed = item.enabled !== false && ready;\n",
        "  if (allowed) return false;\n",
        "}\n",
        "function privateValue() {\n",
        "  const credential = \"\\x67hp_definitiontoken1234\";\n",
        "  if (credential) return false;\n",
        "}\n",
        "function markup() {\n",
        "  const label = \"<mark> harmless | text </mark>\";\n",
        "  if (label) return false;\n",
        "}\n",
        "function accumulated() {\n",
        "  const values = []; values.push(1);\n",
        "  if (values.length) return false;\n",
        "}\n",
    )
    .as_bytes();
    let checkout = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let source_path = checkout.path().join("source.ts");
    std::fs::write(&source_path, source).unwrap();
    let source_id = CaptureSourceId::new("src_11111111111111111111111111111111").unwrap();
    let repo = "local/src_11111111111111111111111111111111";
    let capture = capture_working_tree(
        checkout.path(),
        &source_id,
        &["source.ts".into()],
        CaptureLimits::default(),
    )
    .unwrap();
    std::fs::write(
        &source_path,
        "function replacement() { return 'later-definition-source-canary'; }",
    )
    .unwrap();
    let (extracted, receipts) = extract_file(
        capture.file("source.ts").unwrap(),
        &ParserSourceId {
            repo,
            commit: "workdir",
        },
    )
    .unwrap();
    let rules: Vec<_> = extracted
        .nodes
        .iter()
        .filter(|node| node.label == "BusinessRule")
        .collect();
    assert_eq!(rules.len(), 4);
    for node in &rules {
        let rule = GuardedExitEvidence::from_value(node.props["rule"].clone()).unwrap();
        assert_eq!(rule.schema_version, 2);
        assert_eq!(
            rule.interpretation.execution_predicate,
            InterpretationStatus::NotEstablished
        );
        assert_eq!(
            rule.interpretation.consumer_effect,
            InterpretationStatus::NotEstablished
        );
        let definitions = rule.local_definitions.as_ref().unwrap();
        assert!(!definitions.is_empty());
        let receipt = receipts
            .iter()
            .find(|receipt| receipt.matches_node(node))
            .expect("bounded direct rule retains a whole receipt");
        receipt.validate().unwrap();
        for definition in definitions {
            for evidence in std::iter::once(&definition.declaration)
                .chain(definition.uses.iter())
                .chain(std::iter::once(&definition.initializer.source))
                .chain(
                    definition
                        .expression
                        .nodes
                        .iter()
                        .map(|node| &node.expression.source),
                )
                .chain(
                    definition
                        .dependencies
                        .iter()
                        .map(|dependency| &dependency.source),
                )
            {
                assert!(
                    receipt
                        .ranges()
                        .iter()
                        .any(|range| &range.evidence == evidence),
                    "nested definition source is absent from producer receipt"
                );
            }
            assert!(
                receipt
                    .ranges()
                    .iter()
                    .filter(|range| range.evidence == definition.initializer.source)
                    .count()
                    >= 2,
                "initializer and arena-root occurrences must both remain in the inventory"
            );
        }
    }
    let observed = rules
        .iter()
        .find(|node| {
            node.props["rule"]["conditions"][0]["expression"]["display"]
                .as_str()
                .is_some_and(|display| display.contains("allowed"))
        })
        .unwrap();
    let observed = GuardedExitEvidence::from_value(observed.props["rule"].clone()).unwrap();
    let condition = observed.conditions[0].expression.display.as_str();
    assert!(condition.contains("allowed"));
    assert!(
        !condition.contains("item.enabled"),
        "initializer was substituted into the condition"
    );
    assert!(
        observed
            .local_definitions
            .as_ref()
            .unwrap()
            .iter()
            .any(|definition| definition.initializer.display.as_str()
                == "item.enabled !== false && ready")
    );

    let capture_path = storage.path().join("captures.sqlite");
    {
        let mut captures = CaptureStore::open(&capture_path, StoreLimits::default()).unwrap();
        captures.persist(&capture).unwrap();
    }
    // Persist immutable receipt wire independently; the app's private receipt
    // store and admission/retention guards have their own integration coverage.
    let receipt_path = storage.path().join("receipts.json");
    let wires: Vec<_> = receipts
        .iter()
        .map(|receipt| receipt.to_json().unwrap())
        .collect();
    std::fs::write(&receipt_path, serde_json::to_vec(&wires).unwrap()).unwrap();
    let bindings: Vec<_> = receipts
        .iter()
        .map(|receipt| SourceBinding {
            fact: receipt.fact_key().clone(),
            repo_key: repo.into(),
            receipt_id: receipt.id().into(),
            emitted_fact_digest: receipt.fact_digest().into(),
        })
        .collect();
    let graph_path = storage.path().join("graph.sqlite");
    {
        let mut graph = SqliteGraphStore::open(&graph_path).unwrap();
        let expected = graph.read_snapshot().unwrap();
        let patch = GraphPatch {
            upsert_nodes: extracted.nodes.clone(),
            upsert_edges: extracted.edges.clone(),
            ..GraphPatch::default()
        };
        assert!(
            graph
                .apply_patch_with_source_bindings_if_snapshot_matches(
                    &expected, &patch, repo, &bindings
                )
                .unwrap()
        );
    }
    drop(capture);
    std::fs::remove_file(&source_path).unwrap();
    let graph = SqliteGraphStore::open(&graph_path).unwrap();
    let captures = CaptureStore::open(&capture_path, StoreLimits::default()).unwrap();
    let wires: Vec<String> = serde_json::from_slice(&std::fs::read(receipt_path).unwrap()).unwrap();
    for wire in &wires {
        let receipt = Receipt::from_json(wire).unwrap();
        let binding = graph
            .current_source_binding(receipt.fact_key())
            .unwrap()
            .unwrap();
        assert_eq!(binding.receipt_id, receipt.id());
        assert_eq!(binding.emitted_fact_digest, receipt.fact_digest());
        assert!(wire.contains("primary_source_only"));
        assert!(wire.contains("input_closure_not_established"));
        for range in receipt.ranges() {
            assert_eq!(
                captures.read_text_span(&range.captured).unwrap().as_bytes(),
                &source[range.evidence.byte_start as usize..range.evidence.byte_end as usize]
            );
        }
    }
    let (nodes, edges) = graph.read_snapshot().unwrap();
    let snapshot = ContextSnapshot::new(nodes.clone(), edges.clone()).unwrap();
    let page = snapshot
        .query(QueryRequest {
            labels: vec!["BusinessRule".into()],
            ..QueryRequest::default()
        })
        .unwrap();
    assert_eq!(page.facts.len(), 4);
    assert!(page.next_cursor.is_none());
    for fact in &page.facts {
        let FactReference::Node { id } = &fact.reference else {
            panic!("the rule selection returned a non-node fact");
        };
        let original = nodes.iter().find(|node| &node.id == id).unwrap();
        assert_eq!(fact.properties["rule"], original.props["rule"]);
        assert_eq!(fact.provenance.as_ref().unwrap().tier, Tier::Deterministic);
        assert_eq!(fact.confidence_tier, ConfidenceTier::Confirmed);
    }
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let bundle = compile_spec(&nodes, &edges, &[], mode, &BTreeSet::new());
        let inventory = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.file_name == "rule-evidence.md")
            .unwrap();
        for phrase in [
            "Local const initializers",
            "Initializer as written; value at use and business meaning are not established.",
            "Use sources:",
            "Structured expression evidence",
            "runtime value unresolved",
            "Initializer dependencies:",
            "Consumer effect: not established",
            "&lt;mark&gt; harmless \\| text &lt;/mark&gt;",
            "Unsupported:",
        ] {
            assert!(
                inventory.content.contains(phrase),
                "inventory omitted {phrase}"
            );
        }
        assert!(!inventory.content.contains("<mark>"));
        assert!(
            inventory
                .content
                .contains("item.enabled \\!== false &amp;&amp; ready")
        );
        assert!(inventory.content.contains("values.length"));
        assert!(
            inventory
                .assertions
                .iter()
                .filter(|assertion| assertion.subject_kind == "BusinessRule")
                .all(|assertion| assertion.summary
                    == "Guarded local exit observation; behavioral interpretation not established")
        );
        let surfaces = serde_json::to_string(&(&nodes, &edges, &page, &bundle, &wires)).unwrap();
        for canary in [
            "definition-source-comment-canary",
            "definitiontoken1234",
            "later-definition-source-canary",
        ] {
            assert!(
                !surfaces.contains(canary),
                "withheld original source escaped a display or metadata surface"
            );
        }
    }
}

#[test]
fn retained_capture_drives_real_ts_rules_and_restart_reads_without_surface_disclosure() {
    // AC-0136: capture precedes mutation; the real parser consumes the retained
    // buffer, and its actual emitted spans select those bytes after restart.
    // Capture references remain a separate fixture seam, not verified provenance.
    let source = concat!(
        "// café — 🧭\n",
        "function retained(enabled: boolean) {\n",
        "  if (enabled /* capture-comment-canary */) return { ",
        "password: \"capture-short-secret-canary\", token: \"\\x67hp_capturetoken1234\" };\n",
        "  if (!enabled) return false;\n",
        "}\n",
        "// capture-trailing-source-canary\n",
    )
    .as_bytes();
    let checkout = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let source_path = checkout.path().join("source.ts");
    std::fs::write(&source_path, source).unwrap();
    let capture = capture_working_tree(
        checkout.path(),
        &CaptureSourceId::new("host-owned-source-one").unwrap(),
        &["source.ts".into()],
        CaptureLimits::default(),
    )
    .unwrap();
    let capture_id = capture.id().to_string();
    let file = capture.file("source.ts").unwrap();
    assert_eq!(file.bytes(), source);

    std::fs::write(
        &source_path,
        "function replacement() { return 'live-replacement-canary'; }\n",
    )
    .unwrap();
    let extracted = extract_source(
        file.bytes(),
        "source.ts",
        &ParserSourceId {
            repo: "fixture/capture",
            commit: "workdir",
        },
    )
    .unwrap();
    assert!(
        extracted
            .nodes
            .iter()
            .any(|node| node.label == "Symbol" && node.props["name"] == "retained")
    );
    let rules: Vec<_> = extracted
        .nodes
        .iter()
        .filter(|node| node.label == "BusinessRule")
        .map(|node| GuardedExitEvidence::from_value(node.props["rule"].clone()).unwrap())
        .collect();
    assert_eq!(rules.len(), 2);
    let spans: Vec<_> = rules
        .iter()
        .map(|rule| {
            let cited = &rule.exit_source;
            assert_eq!(cited.repo, "fixture/capture");
            assert_eq!(cited.path, "source.ts");
            assert_eq!(cited.commit_sha, "workdir");
            assert_eq!(
                rule.interpretation.consumer_effect,
                InterpretationStatus::NotEstablished
            );
            let expected = source[cited.byte_start as usize..cited.byte_end as usize].to_vec();
            assert!(expected.starts_with(b"return "));
            (
                file.span(cited.byte_start, cited.byte_end).unwrap(),
                expected,
            )
        })
        .collect();

    // Raw sources are explicitly retained only in the capture store. They must
    // not appear in manifest/debug metadata or the existing graph read surfaces.
    let metadata = serde_json::to_string(&(capture.manifest(), file.reference())).unwrap();
    let diagnostics = format!("{capture:?} {file:?}");
    let graph_path = storage.path().join("graph.db");
    {
        let mut graph = SqliteGraphStore::open(&graph_path).unwrap();
        for node in &extracted.nodes {
            graph.put_node(node).unwrap();
        }
        for edge in &extracted.edges {
            graph.put_edge(edge).unwrap();
        }
    }
    let store_path = storage.path().join("captures.sqlite");
    {
        let mut store = CaptureStore::open(&store_path, StoreLimits::default()).unwrap();
        store.persist(&capture).unwrap();
    }
    drop(capture);
    std::fs::remove_file(&source_path).unwrap();

    let reopened = CaptureStore::open(&store_path, StoreLimits::default()).unwrap();
    let restored = reopened.load(&capture_id).unwrap();
    assert_eq!(restored.file("source.ts").unwrap().bytes(), source);
    for (span, expected) in &spans {
        assert_eq!(reopened.read_span(span).unwrap(), *expected);
        assert_eq!(
            reopened.read_text_span(span).unwrap().as_bytes(),
            expected.as_slice()
        );
    }
    // Strict text reads cannot turn a split UTF-8 sequence into replacement text,
    // even though the raw capture supports byte-aware consumers.
    let multibyte = source
        .windows(2)
        .position(|bytes| bytes == "é".as_bytes())
        .unwrap();
    let split = restored
        .file("source.ts")
        .unwrap()
        .span(multibyte as u64 + 1, multibyte as u64 + 2)
        .unwrap();
    assert_eq!(
        reopened.read_span(&split).unwrap(),
        vec![source[multibyte + 1]]
    );
    assert!(reopened.read_text_span(&split).is_err());

    let graph = SqliteGraphStore::open(&graph_path).unwrap();
    let nodes = graph.all_nodes().unwrap();
    let edges = graph.all_edges().unwrap();
    let snapshot = ContextSnapshot::new(nodes.clone(), edges.clone()).unwrap();
    let page = snapshot.query(QueryRequest::default()).unwrap();
    assert!(page.next_cursor.is_none());
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let bundle = compile_spec(&nodes, &edges, &[], mode, &BTreeSet::new());
        let inventory = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.file_name == "rule-evidence.md")
            .unwrap();
        assert!(inventory.content.contains("boolean false"));
        assert!(
            inventory
                .content
                .contains("Consumer effect: not established")
        );
        let surfaces = serde_json::to_string(&(&nodes, &edges, &page, &bundle)).unwrap();
        for canary in [
            "capture-comment-canary",
            "capture-short-secret-canary",
            "capturetoken1234",
            "capture-trailing-source-canary",
            "live-replacement-canary",
        ] {
            assert!(
                !surfaces.contains(canary),
                "raw source reached a normal read surface"
            );
            assert!(
                !metadata.contains(canary),
                "raw source reached capture metadata"
            );
            assert!(
                !diagnostics.contains(canary),
                "raw source reached capture diagnostics"
            );
        }
    }
}

#[test]
fn trailing_comment_changes_capture_identity_without_changing_recovered_context() {
    // AC-0132/AC-0136: graph/semantic equality is not raw-source equality.
    // Same-length comments outside the callable preserve all fact spans/hashes.
    let original =
        b"function stable(enabled: boolean) { if (enabled) return false; }\n// trailing-A\n";
    let changed =
        b"function stable(enabled: boolean) { if (enabled) return false; }\n// trailing-B\n";
    assert_eq!(original.len(), changed.len());
    let checkout = tempfile::tempdir().unwrap();
    let path = checkout.path().join("source.ts");
    let source_id = CaptureSourceId::new("host-owned-source-one").unwrap();
    std::fs::write(&path, original).unwrap();
    let first = capture_working_tree(
        checkout.path(),
        &source_id,
        &["source.ts".into()],
        CaptureLimits::default(),
    )
    .unwrap();
    std::fs::write(&path, changed).unwrap();
    let second = capture_working_tree(
        checkout.path(),
        &source_id,
        &["source.ts".into()],
        CaptureLimits::default(),
    )
    .unwrap();
    assert_ne!(first.id(), second.id());
    let first_file = first.file("source.ts").unwrap();
    let second_file = second.file("source.ts").unwrap();
    assert_eq!(first_file.bytes(), original);
    assert_eq!(second_file.bytes(), changed);
    assert_ne!(
        core_prov::content_hash(first_file.bytes()),
        core_prov::content_hash(second_file.bytes())
    );
    let parser_id = ParserSourceId {
        repo: "fixture/capture",
        commit: "workdir",
    };
    let before = extract_source(first_file.bytes(), "source.ts", &parser_id).unwrap();
    let after = extract_source(second_file.bytes(), "source.ts", &parser_id).unwrap();
    assert_eq!(before.nodes, after.nodes);
    assert_eq!(before.edges, after.edges);
    let before_snapshot = ContextSnapshot::new(before.nodes, before.edges).unwrap();
    let after_snapshot = ContextSnapshot::new(after.nodes, after.edges).unwrap();
    assert_eq!(before_snapshot.id(), after_snapshot.id());

    // Identical code bytes at the cited span cannot transplant a reference from
    // the original capture to a different captured file, even with equal graphs.
    let span = first_file.span(0, 8).unwrap();
    assert_eq!(first.read_span(&span).unwrap(), b"function");
    assert!(second.read_span(&span).is_err());
}

#[test]
fn source_capture_keeps_legacy_evidence_ref_wire_shape_byte_identical() {
    // AC-0136: new capture references are separate from the exact version-1
    // EvidenceRef JSON embedded in immutable legacy proposal/task identities.
    // This wire fixture does not retroactively verify an old citation.
    const LEGACY: &str = r#"{"repo":"fixture/capture","path":"source.ts","byte_start":12,"byte_end":34,"commit_sha":"workdir"}"#;
    let legacy: EvidenceRef = serde_json::from_str(LEGACY).unwrap();
    assert_eq!(serde_json::to_string(&legacy).unwrap(), LEGACY);
    let checkout = tempfile::tempdir().unwrap();
    std::fs::write(checkout.path().join("source.ts"), b"x".repeat(40)).unwrap();
    let capture = capture_working_tree(
        checkout.path(),
        &CaptureSourceId::new("host-owned-source-one").unwrap(),
        std::slice::from_ref(&legacy.path),
        CaptureLimits::default(),
    )
    .unwrap();
    let span = capture
        .file(&legacy.path)
        .unwrap()
        .span(legacy.byte_start, legacy.byte_end)
        .unwrap();
    assert_eq!(capture.read_span(&span).unwrap(), b"x".repeat(22));
    assert_eq!(serde_json::to_string(&legacy).unwrap(), LEGACY);
    let wire = serde_json::to_value(&legacy).unwrap();
    assert_eq!(wire.as_object().unwrap().len(), 5);
    assert!(wire.get("capture_id").is_none());
    assert!(wire.get("verified").is_none());
}
