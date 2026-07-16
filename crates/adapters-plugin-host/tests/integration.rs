//! Real wasmtime component execution against prebuilt guest fixtures
//! (see `tests/fixtures/README.md`). Proves AC-0070: fuel exhaustion,
//! memory-cap, and deadline bounds all fail the call closed with an
//! explicit `HostError` and no partial facts, and the guest gets no
//! ambient network capability.

use std::time::Duration;

use adapters_plugin_host::{HostError, PluginHost, PluginLimits, SourceId};

const OK_ADAPTER: &[u8] = include_bytes!("fixtures/compiled/ok-adapter.wasm");
const BUSY_LOOP: &[u8] = include_bytes!("fixtures/compiled/busy-loop.wasm");
const MEMORY_HOG: &[u8] = include_bytes!("fixtures/compiled/memory-hog.wasm");
const NET_PROBE: &[u8] = include_bytes!("fixtures/compiled/net-probe.wasm");
const CLOCK_PROBE: &[u8] = include_bytes!("fixtures/compiled/clock-probe.wasm");

fn source_id() -> SourceId {
    SourceId {
        repo: "owner/repo".to_string(),
        commit: "deadbeef".to_string(),
    }
}

/// T-0070 (AC-0070): a well-behaved plugin's facts round-trip through the
/// host exactly as it emitted them.
#[test]
fn ok_adapter_round_trips_facts() {
    let host = PluginHost::new().expect("engine");
    let plugin = host
        .load(OK_ADAPTER, "t0.plugin-fixture")
        .expect("compiles");

    let source = b"hello world";
    let extraction = host
        .call_extract(
            &plugin,
            source,
            "src/lib.rs",
            &source_id(),
            PluginLimits::default(),
        )
        .expect("well-behaved plugin succeeds");

    assert_eq!(extraction.nodes.len(), 1);
    assert_eq!(extraction.edges.len(), 1);
    let node = &extraction.nodes[0];
    assert_eq!(node.id, "owner/repo:src/lib.rs");
    assert_eq!(node.props["len"], source.len());
}

/// T-0070 (AC-0070): a plugin that never returns exhausts its fuel bound
/// and fails closed rather than hanging or partially completing.
#[test]
fn fuel_exhaustion_fails_closed() {
    let host = PluginHost::new().expect("engine");
    let plugin = host.load(BUSY_LOOP, "t0.plugin-busy").expect("compiles");

    let limits = PluginLimits {
        max_fuel: 100_000,
        max_memory_bytes: 16 * 1024 * 1024,
        max_table_elements: 10_000,
        deadline: Duration::from_secs(30),
    };
    let err = host
        .call_extract(&plugin, b"x", "loop.rs", &source_id(), limits)
        .expect_err("busy loop must not complete");

    assert!(
        matches!(err, HostError::FuelExhausted(_)),
        "expected FuelExhausted, got {err:?}"
    );
}

/// T-0070 (AC-0070): a plugin given ample fuel but a short wall-clock
/// deadline is interrupted and fails closed.
#[test]
fn deadline_exceeded_fails_closed() {
    let host = PluginHost::new().expect("engine");
    let plugin = host.load(BUSY_LOOP, "t0.plugin-busy").expect("compiles");

    let limits = PluginLimits {
        max_fuel: u64::MAX,
        max_memory_bytes: 16 * 1024 * 1024,
        max_table_elements: 10_000,
        deadline: Duration::from_millis(60),
    };
    let err = host
        .call_extract(&plugin, b"x", "loop.rs", &source_id(), limits)
        .expect_err("busy loop must not complete");

    assert!(
        matches!(err, HostError::DeadlineExceeded(_)),
        "expected DeadlineExceeded, got {err:?}"
    );
}

/// T-0070 (AC-0070): a plugin that grows memory without bound is denied
/// past the configured cap and fails closed instead of exhausting host
/// memory.
#[test]
fn memory_limit_exceeded_fails_closed() {
    let host = PluginHost::new().expect("engine");
    let plugin = host.load(MEMORY_HOG, "t0.plugin-memory").expect("compiles");

    let limits = PluginLimits {
        max_fuel: u64::MAX,
        max_memory_bytes: 2 * 1024 * 1024,
        max_table_elements: 10_000,
        deadline: Duration::from_secs(30),
    };
    let err = host
        .call_extract(&plugin, b"x", "hog.rs", &source_id(), limits)
        .expect_err("unbounded allocation must not complete");

    assert!(
        matches!(err, HostError::MemoryLimitExceeded(_)),
        "expected MemoryLimitExceeded, got {err:?}"
    );
}

/// T-0070 (AC-0070): the host grants no ambient network capability — a
/// plugin's TCP connect attempt is denied by the default (empty) WasiCtx
/// before any real connection is made.
#[test]
fn no_ambient_network_capability() {
    let host = PluginHost::new().expect("engine");
    let plugin = host.load(NET_PROBE, "t0.plugin-net").expect("compiles");

    let extraction = host
        .call_extract(
            &plugin,
            b"x",
            "probe.rs",
            &source_id(),
            PluginLimits::default(),
        )
        .expect("probe adapter completes and reports its outcome");

    let outcome = extraction.nodes[0].props["outcome"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        outcome.starts_with("denied:"),
        "expected the connect attempt to be denied, got outcome={outcome:?}"
    );
}

/// T-0070 (AC-0070): the host grants no ambient wall/monotonic clock —
/// ADR-0017's sandboxed determinism requires that a plugin reading the
/// clock still produces byte-identical facts run to run. Two calls
/// separated by a real sleep must report identical fixed clock readings,
/// not the host's actual elapsed wall-clock time.
#[test]
fn no_ambient_clock() {
    let host = PluginHost::new().expect("engine");
    let plugin = host.load(CLOCK_PROBE, "t0.plugin-clock").expect("compiles");

    let read = |host: &PluginHost| {
        let extraction = host
            .call_extract(
                &plugin,
                b"x",
                "clock.rs",
                &source_id(),
                PluginLimits::default(),
            )
            .expect("clock probe completes");
        extraction.nodes[0].props.clone()
    };

    let first = read(&host);
    std::thread::sleep(Duration::from_millis(50));
    let second = read(&host);

    assert_eq!(
        first, second,
        "clock readings must be fixed, not ambient (first call vs. after a real sleep)"
    );
    assert_eq!(
        first["wall_millis"], 0,
        "wall clock must be fixed at the epoch"
    );
    assert_eq!(
        first["elapsed_nanos"], 0,
        "monotonic clock must be fixed, so elapsed-within-a-call is always zero"
    );
}

/// T-0069 (AC-0069, #199): plugin facts are pinned to the exact artifact —
/// `extractor_id = "{plugin_id}@{hash12}"`, full BLAKE3 artifact hash on
/// every fact — the guest cannot impersonate a compiled-in extractor, and
/// repeat runs of the same artifact over the same source yield an
/// identical whole-extraction hash set (determinism with a plugin active).
#[test]
fn plugin_provenance_pins_artifact_and_repeats_deterministically() {
    use adapters_plugin_host::pin_extraction;
    use core_prov::content_hash;

    let host = PluginHost::new().expect("engine");
    let plugin = host
        .load(OK_ADAPTER, "t0.plugin-fixture")
        .expect("compiles");
    let artifact_hash = content_hash(OK_ADAPTER);
    let source = b"hello world";

    // call_extract pins automatically (#204 review): no manual step.
    let run = || {
        host.call_extract(
            &plugin,
            source,
            "src/lib.rs",
            &source_id(),
            PluginLimits::default(),
        )
        .expect("well-behaved plugin succeeds")
    };

    let first = run();
    let expected_id = format!("t0.plugin-fixture@{}", &artifact_hash[..12]);
    // Every fact carries the full artifact hash, prov or not; a fact
    // without provenance is the conformance gate's job to reject (#200).
    for props in first
        .nodes
        .iter()
        .map(|node| &node.props)
        .chain(first.edges.iter().map(|edge| &edge.props))
    {
        assert_eq!(props["plugin_artifact_hash"], artifact_hash.as_str());
    }

    // A guest-supplied extractor_id is overwritten — a plugin cannot
    // impersonate a compiled-in extractor.
    let mut spoofed = adapters_plugin_host::PluginExtraction {
        nodes: vec![core_graph::Node {
            id: "n1".into(),
            label: "File".into(),
            props: serde_json::json!({ "prov": { "extractor_id": "t0.adapter-ts" } }),
        }],
        edges: Vec::new(),
    };
    pin_extraction(&mut spoofed, "t0.plugin-fixture", &artifact_hash);
    assert_eq!(
        spoofed.nodes[0].props["prov"]["extractor_id"],
        expected_id.as_str()
    );
    assert_eq!(
        spoofed.nodes[0].props["plugin_artifact_hash"],
        artifact_hash.as_str()
    );

    // Order-independent whole-extraction hash: identical across runs.
    let canonical = |extraction: &adapters_plugin_host::PluginExtraction| {
        let mut hashes: Vec<String> = extraction
            .nodes
            .iter()
            .map(|node| format!("node:{}:{}", node.id, node.props))
            .chain(extraction.edges.iter().map(|edge| {
                format!(
                    "edge:{}:{}:{}:{}",
                    edge.src, edge.dst, edge.label, edge.props
                )
            }))
            .collect();
        hashes.sort();
        content_hash(hashes.join("\n").as_bytes())
    };
    assert_eq!(canonical(&first), canonical(&run()));

    // A different artifact is a different extractor identity: same id,
    // different bytes can never masquerade as the pinned adapter.
    let other_hash = content_hash(BUSY_LOOP);
    assert_ne!(
        format!("t0.plugin-fixture@{}", &other_hash[..12]),
        expected_id
    );
}

/// T-0068 (AC-0068, #200): the conformance gate passes a well-behaved
/// adapter with a correct golden corpus, and fails closed — with the
/// failing check named — on golden mismatch, an empty corpus, and a
/// bound-violating artifact. Double-run determinism is part of the gate.
#[test]
fn conformance_gate_passes_golden_corpus_and_fails_closed() {
    use adapters_plugin_host::gate::{
        ExpectedEdge, ExpectedNode, GoldenCase, GoldenCorpus, run_gate,
    };

    let host = PluginHost::new().expect("engine");
    let corpus = GoldenCorpus {
        extensions: vec!["rs".to_string()],
        cases: vec![GoldenCase {
            path: "src/lib.rs".to_string(),
            source: "hello world".to_string(),
            nodes: vec![ExpectedNode {
                id: "owner/repo:src/lib.rs".to_string(),
                label: "TestNode".to_string(),
                props: serde_json::json!({ "len": 11 }),
            }],
            edges: vec![ExpectedEdge {
                src: "owner/repo:src/lib.rs".to_string(),
                dst: "owner/repo:src/lib.rs".to_string(),
                label: "SELF".to_string(),
                props: serde_json::json!({}),
            }],
        }],
    };

    let report = run_gate(
        &host,
        "t0.plugin-fixture",
        OK_ADAPTER,
        &corpus,
        PluginLimits::default(),
        &source_id(),
    );
    assert!(report.passed, "checks: {:?}", report.checks);
    let names: Vec<&str> = report.checks.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"spi-compiles"));
    assert!(names.contains(&"golden:src/lib.rs"));
    assert!(names.contains(&"determinism-double-run"));

    // A wrong expectation fails the golden check, not the whole harness.
    let mut wrong = corpus.clone();
    wrong.cases[0].nodes[0].props = serde_json::json!({ "len": 999 });
    let report = run_gate(
        &host,
        "t0.plugin-fixture",
        OK_ADAPTER,
        &wrong,
        PluginLimits::default(),
        &source_id(),
    );
    assert!(!report.passed);
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.name == "golden:src/lib.rs" && !c.passed)
    );

    // An empty corpus proves nothing and fails closed.
    let empty = GoldenCorpus {
        extensions: vec![],
        cases: vec![],
    };
    let report = run_gate(
        &host,
        "t0.plugin-fixture",
        OK_ADAPTER,
        &empty,
        PluginLimits::default(),
        &source_id(),
    );
    assert!(!report.passed);
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.name == "corpus-nonempty" && !c.passed)
    );

    // A bound-violating artifact fails the contract check by name.
    let tight = PluginLimits {
        max_fuel: 100_000,
        max_memory_bytes: 16 * 1024 * 1024,
        max_table_elements: 10_000,
        deadline: Duration::from_secs(30),
    };
    let report = run_gate(
        &host,
        "t0.plugin-busy",
        BUSY_LOOP,
        &corpus,
        tight,
        &source_id(),
    );
    assert!(!report.passed);
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.name == "contract:src/lib.rs" && !c.passed)
    );
}
