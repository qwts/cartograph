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
    let plugin = host.load(OK_ADAPTER).expect("compiles");

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
    let plugin = host.load(BUSY_LOOP).expect("compiles");

    let limits = PluginLimits {
        max_fuel: 100_000,
        max_memory_bytes: 16 * 1024 * 1024,
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
    let plugin = host.load(BUSY_LOOP).expect("compiles");

    let limits = PluginLimits {
        max_fuel: u64::MAX,
        max_memory_bytes: 16 * 1024 * 1024,
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
    let plugin = host.load(MEMORY_HOG).expect("compiles");

    let limits = PluginLimits {
        max_fuel: u64::MAX,
        max_memory_bytes: 2 * 1024 * 1024,
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
    let plugin = host.load(NET_PROBE).expect("compiles");

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
