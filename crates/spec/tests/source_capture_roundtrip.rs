//! Exercise immutable capture bytes with a real producer, without claiming
//! production ingestion or attaching verification to legacy provenance.

use adapters_lang_ts::{SourceId as ParserSourceId, extract_source};
use context_hub::{ContextSnapshot, QueryRequest};
use core_graph::rules::{GuardedExitEvidence, InterpretationStatus};
use core_graph::{GraphStore, SqliteGraphStore};
use core_prov::EvidenceRef;
use source_capture::{
    CaptureLimits, CaptureStore, SourceId as CaptureSourceId, StoreLimits, capture_working_tree,
};
use spec::{ExportMode, compile_spec};
use std::collections::BTreeSet;

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
