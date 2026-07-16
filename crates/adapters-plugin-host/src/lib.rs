//! WASM component host runtime for runtime-loadable adapter plugins
//! (SPEC-00 §8.1, ADR-0017, epic #147, issue #197).
//!
//! Loads a `cartograph:adapter` component and runs its `extract-source`
//! export under an epoch-based wall-clock deadline, a fuel bound, and a
//! memory cap. The guest gets no ambient capabilities: WASI is linked only
//! because the guest's Rust runtime needs it to function, but the
//! [`WasiCtx`] granted is empty (no preopened directories, no sockets, no
//! inherited env or args) — the only data the guest ever sees is the source
//! bytes passed as the call argument (host-mediated, read-only). Every
//! failure mode (fuel exhaustion, memory cap, deadline, trap, malformed
//! output) fails the call closed: callers get an [`Err`] and must treat the
//! call as having produced zero facts (AC-0070).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use core_graph::{Edge, Node};
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{
    Deterministic, HostMonotonicClock, HostWallClock, WasiCtx, WasiCtxBuilder, WasiCtxView,
    WasiView,
};

#[allow(missing_docs, reason = "generated bindings")]
pub mod discovery;
pub mod gate;

mod bindings {
    wasmtime::component::bindgen!({
        path: "wit",
        world: "adapter",
    });
}
use bindings::Adapter;

use bindings::exports::cartograph::adapter::extract::{
    ExtractError as WitExtractError, Extraction as WitExtraction, SourceId as WitSourceId,
};

/// How long one epoch tick represents; the background ticker increments the
/// engine's epoch counter at this cadence so per-call deadlines (in ticks)
/// approximate wall-clock time.
const EPOCH_TICK: Duration = Duration::from_millis(20);

/// Which repo/commit a source file came from, passed through to the plugin
/// so its emitted facts can carry accurate evidence spans.
#[derive(Debug, Clone)]
pub struct SourceId {
    /// Repo identity, e.g. `"owner/name"` or `"local"`.
    pub repo: String,
    /// Commit SHA, or `"workdir"`.
    pub commit: String,
}

/// Facts a plugin extracted from one source file, in the same shape the
/// compiled-in language adapters produce.
#[derive(Debug, Clone, Default)]
pub struct PluginExtraction {
    /// Extracted nodes.
    pub nodes: Vec<Node>,
    /// Extracted edges.
    pub edges: Vec<Edge>,
}

/// Resource bounds enforced on every plugin call. Exceeding any of them
/// fails the call closed with no partial facts (AC-0070).
#[derive(Debug, Clone, Copy)]
pub struct PluginLimits {
    /// Fuel units (roughly, executed wasm instructions) allowed per call.
    pub max_fuel: u64,
    /// Maximum linear-memory bytes the guest instance may grow to.
    pub max_memory_bytes: usize,
    /// Maximum element count any single wasm table may grow to. Table
    /// elements consume host memory too (e.g. `funcref`/`externref`
    /// entries), so this bounds the same host-memory-exhaustion risk as
    /// `max_memory_bytes` for a resource that isn't linear memory.
    pub max_table_elements: usize,
    /// Wall-clock deadline for one call.
    pub deadline: Duration,
}

impl Default for PluginLimits {
    fn default() -> Self {
        Self {
            max_fuel: 10_000_000_000,
            max_memory_bytes: 64 * 1024 * 1024,
            max_table_elements: 10_000,
            deadline: Duration::from_secs(5),
        }
    }
}

/// Why a plugin call failed. Every variant fails closed: callers must
/// treat the call as having produced zero facts.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// The plugin bytes did not compile as a valid component.
    #[error("plugin component failed to compile: {0}")]
    InvalidComponent(String),
    /// Linking/instantiating the component failed.
    #[error("plugin instantiation failed: {0}")]
    Instantiation(String),
    /// The plugin consumed its entire fuel bound without finishing.
    #[error("plugin exceeded its fuel bound ({0} units)")]
    FuelExhausted(u64),
    /// The plugin tried to grow linear memory past its cap.
    #[error("plugin exceeded its memory bound ({0} bytes)")]
    MemoryLimitExceeded(usize),
    /// The plugin tried to grow a wasm table past its element cap.
    #[error("plugin exceeded its table bound ({0} elements)")]
    TableLimitExceeded(usize),
    /// The plugin did not finish within its wall-clock deadline.
    #[error("plugin exceeded its deadline ({0:?})")]
    DeadlineExceeded(Duration),
    /// The plugin's `extract-source` export returned `Err`.
    #[error("plugin reported an extraction error: {0}")]
    GuestError(String),
    /// The plugin emitted a fact whose `props-json` did not parse.
    #[error("plugin emitted a malformed fact: {0}")]
    MalformedFact(String),
    /// The plugin trapped for a reason not covered by a bound above.
    #[error("plugin trapped: {0}")]
    Trap(String),
}

/// Marker error returned by the memory [`wasmtime::ResourceLimiter`] so the
/// host can distinguish "denied by our cap" from any other guest abort.
#[derive(Debug)]
struct MemoryCapExceeded {
    max_bytes: usize,
}

impl std::fmt::Display for MemoryCapExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "memory cap of {} bytes exceeded", self.max_bytes)
    }
}

impl std::error::Error for MemoryCapExceeded {}

/// Marker error returned by the table [`wasmtime::ResourceLimiter`], for
/// the same reason as [`MemoryCapExceeded`].
#[derive(Debug)]
struct TableCapExceeded {
    max_elements: usize,
}

impl std::fmt::Display for TableCapExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "table cap of {} elements exceeded", self.max_elements)
    }
}

impl std::error::Error for TableCapExceeded {}

/// A wall clock fixed at the Unix epoch. Plugins get no ambient clock
/// (ADR-0017 §2, sandboxed determinism): every read returns the same
/// value, so a plugin that reads the clock still produces byte-identical
/// facts run to run.
struct FixedWallClock;

impl HostWallClock for FixedWallClock {
    fn resolution(&self) -> Duration {
        Duration::from_secs(1)
    }

    fn now(&self) -> Duration {
        Duration::ZERO
    }
}

/// A monotonic clock fixed at zero, for the same reason as [`FixedWallClock`].
struct FixedMonotonicClock;

impl HostMonotonicClock for FixedMonotonicClock {
    fn resolution(&self) -> u64 {
        1
    }

    fn now(&self) -> u64 {
        0
    }
}

struct HostState {
    wasi: WasiCtx,
    table: ResourceTable,
    max_memory_bytes: usize,
    max_table_elements: usize,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// A compiled plugin, ready to be called any number of times. Compilation
/// (the expensive step) happens once in [`PluginHost::load`]; each call in
/// [`PluginHost::call_extract`] gets a fresh [`Store`] so calls never share
/// state and a bound-exceeding call can't corrupt a later one.
pub struct LoadedPlugin {
    component: Component,
    /// Adapter id used for provenance pinning.
    plugin_id: String,
    /// BLAKE3 hash of the artifact bytes — pinned onto every fact.
    artifact_hash: String,
}

impl LoadedPlugin {
    /// BLAKE3 hash of the exact artifact bytes this plugin compiled from.
    pub fn artifact_hash(&self) -> &str {
        &self.artifact_hash
    }
}

/// Owns the wasmtime [`Engine`] and the background epoch ticker. One host
/// serves any number of loaded plugins and calls.
pub struct PluginHost {
    engine: Engine,
    ticker_stop: Arc<AtomicBool>,
    ticker: Option<std::thread::JoinHandle<()>>,
}

impl PluginHost {
    /// Build a host runtime: a wasmtime engine configured for the component
    /// model with fuel and epoch interruption enabled, plus a background
    /// thread ticking the epoch counter so per-call deadlines are
    /// wall-clock accurate regardless of how many calls run concurrently.
    pub fn new() -> Result<Self, HostError> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.consume_fuel(true);
        config.epoch_interruption(true);
        let engine = Engine::new(&config).map_err(|e| HostError::Instantiation(e.to_string()))?;

        let ticker_stop = Arc::new(AtomicBool::new(false));
        let ticker_engine = engine.clone();
        let stop_flag = ticker_stop.clone();
        let ticker = std::thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                std::thread::sleep(EPOCH_TICK);
                ticker_engine.increment_epoch();
            }
        });

        Ok(Self {
            engine,
            ticker_stop,
            ticker: Some(ticker),
        })
    }

    /// Compile a `cartograph:adapter` component from bytes. Does not run
    /// any guest code. The `plugin_id` and the bytes' BLAKE3 hash are
    /// carried on the handle so every extraction is provenance-pinned
    /// automatically — no caller can forget it (#204 review).
    pub fn load(&self, wasm_bytes: &[u8], plugin_id: &str) -> Result<LoadedPlugin, HostError> {
        let component = Component::new(&self.engine, wasm_bytes)
            .map_err(|e| HostError::InvalidComponent(e.to_string()))?;
        Ok(LoadedPlugin {
            component,
            plugin_id: plugin_id.to_string(),
            artifact_hash: core_prov::content_hash(wasm_bytes),
        })
    }

    /// Run one `extract-source` call against a loaded plugin, under
    /// `limits`. The guest receives `source`/`path`/`id` as call arguments
    /// and nothing else — no filesystem, no network, no environment.
    pub fn call_extract(
        &self,
        plugin: &LoadedPlugin,
        source: &[u8],
        path: &str,
        id: &SourceId,
        limits: PluginLimits,
    ) -> Result<PluginExtraction, HostError> {
        let mut linker: Linker<HostState> = Linker::new(&self.engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|e| HostError::Instantiation(e.to_string()))?;

        let wasi = WasiCtxBuilder::new()
            .wall_clock(FixedWallClock)
            .monotonic_clock(FixedMonotonicClock)
            .secure_random(Deterministic::new(vec![0u8; 32]))
            .insecure_random(Deterministic::new(vec![0u8; 32]))
            .insecure_random_seed(0)
            .build();
        let mut store = Store::new(
            &self.engine,
            HostState {
                wasi,
                table: ResourceTable::new(),
                max_memory_bytes: limits.max_memory_bytes,
                max_table_elements: limits.max_table_elements,
            },
        );
        store.limiter(|state| state);
        store
            .set_fuel(limits.max_fuel)
            .map_err(|e| HostError::Instantiation(e.to_string()))?;

        let ticks = ((limits.deadline.as_millis() / EPOCH_TICK.as_millis()).max(1)) as u64;
        store.set_epoch_deadline(ticks);

        let bindings = Adapter::instantiate(&mut store, &plugin.component, &linker)
            .map_err(|e| HostError::Instantiation(e.to_string()))?;

        let wit_id = WitSourceId {
            repo: id.repo.clone(),
            commit: id.commit.clone(),
        };

        let call_result = bindings
            .cartograph_adapter_extract()
            .call_extract_source(&mut store, source, path, &wit_id);

        let remaining_fuel = store.get_fuel().unwrap_or(0);

        match call_result {
            Ok(Ok(extraction)) => {
                let mut extraction = convert_extraction(extraction)?;
                // Non-optional pinning (#204 review): every fact leaving the
                // host carries the pinned extractor identity and artifact
                // hash — there is no unpinned production path.
                pin_extraction(&mut extraction, &plugin.plugin_id, &plugin.artifact_hash);
                Ok(extraction)
            }
            Ok(Err(WitExtractError { message })) => Err(HostError::GuestError(message)),
            Err(err) => Err(classify_call_error(err, limits, remaining_fuel)),
        }
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        self.ticker_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.ticker.take() {
            let _ = handle.join();
        }
    }
}

fn classify_call_error(
    err: wasmtime::Error,
    limits: PluginLimits,
    remaining_fuel: u64,
) -> HostError {
    if err.downcast_ref::<MemoryCapExceeded>().is_some() {
        return HostError::MemoryLimitExceeded(limits.max_memory_bytes);
    }
    if err.downcast_ref::<TableCapExceeded>().is_some() {
        return HostError::TableLimitExceeded(limits.max_table_elements);
    }
    if let Some(trap) = err.downcast_ref::<wasmtime::Trap>() {
        return match trap {
            wasmtime::Trap::OutOfFuel => HostError::FuelExhausted(limits.max_fuel),
            wasmtime::Trap::Interrupt => HostError::DeadlineExceeded(limits.deadline),
            other => HostError::Trap(other.to_string()),
        };
    }
    // Fuel can also be exhausted without wasmtime reporting the OutOfFuel
    // trap variant depending on where in the call it ran out; treat "no
    // fuel left" as authoritative when nothing more specific matched.
    if remaining_fuel == 0 {
        return HostError::FuelExhausted(limits.max_fuel);
    }
    HostError::Trap(err.to_string())
}

fn convert_extraction(extraction: WitExtraction) -> Result<PluginExtraction, HostError> {
    let nodes = extraction
        .nodes
        .into_iter()
        .map(|n| {
            let props = serde_json::from_str(&n.props_json)
                .map_err(|e| HostError::MalformedFact(format!("node {}: {e}", n.id)))?;
            Ok(Node {
                id: n.id,
                label: n.label,
                props,
            })
        })
        .collect::<Result<Vec<_>, HostError>>()?;

    let edges = extraction
        .edges
        .into_iter()
        .map(|e| {
            let props = serde_json::from_str(&e.props_json).map_err(|err| {
                HostError::MalformedFact(format!("edge {}->{}: {err}", e.src, e.dst))
            })?;
            Ok(Edge {
                src: e.src,
                dst: e.dst,
                label: e.label,
                props,
            })
        })
        .collect::<Result<Vec<_>, HostError>>()?;

    Ok(PluginExtraction { nodes, edges })
}

impl wasmtime::ResourceLimiter for HostState {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > self.max_memory_bytes {
            return Err(MemoryCapExceeded {
                max_bytes: self.max_memory_bytes,
            }
            .into());
        }
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > self.max_table_elements {
            return Err(TableCapExceeded {
                max_elements: self.max_table_elements,
            }
            .into());
        }
        Ok(true)
    }
}

/// Pin a plugin extraction's provenance to the exact artifact that produced
/// it (#199, AC-0069). Host-authoritative: whatever `extractor_id` the
/// guest wrote is overwritten with `{plugin_id}@{hash12}` — a plugin cannot
/// impersonate a compiled-in extractor, and a rebuilt artifact (different
/// bytes, different hash) is a different extractor identity. The full
/// artifact hash rides on every fact as `plugin_artifact_hash`.
pub fn pin_extraction(extraction: &mut PluginExtraction, plugin_id: &str, artifact_hash: &str) {
    let short = &artifact_hash[..artifact_hash.len().min(12)];
    let pinned_id = format!("{plugin_id}@{short}");
    let props = extraction
        .nodes
        .iter_mut()
        .map(|node| &mut node.props)
        .chain(extraction.edges.iter_mut().map(|edge| &mut edge.props));
    for value in props {
        if let Some(prov) = value.get_mut("prov").and_then(|prov| prov.as_object_mut()) {
            prov.insert(
                "extractor_id".to_string(),
                serde_json::Value::String(pinned_id.clone()),
            );
        }
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "plugin_artifact_hash".to_string(),
                serde_json::Value::String(artifact_hash.to_string()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_state(max_memory_bytes: usize, max_table_elements: usize) -> HostState {
        HostState {
            wasi: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
            max_memory_bytes,
            max_table_elements,
        }
    }

    /// Regression test for a Codex review finding on PR #202: the
    /// ResourceLimiter used to allow unbounded table growth even though
    /// table elements consume host memory, letting a plugin bypass the
    /// advertised memory cap.
    #[test]
    fn table_growth_within_cap_is_allowed() {
        let mut state = host_state(1024, 10);
        let allowed = wasmtime::ResourceLimiter::table_growing(&mut state, 0, 10, None)
            .expect("within cap does not error");
        assert!(allowed);
    }

    #[test]
    fn table_growth_past_cap_fails_closed() {
        let mut state = host_state(1024, 10);
        let err = wasmtime::ResourceLimiter::table_growing(&mut state, 0, 11, None)
            .expect_err("past cap must be denied");
        assert!(err.downcast_ref::<TableCapExceeded>().is_some());
    }

    #[test]
    fn memory_growth_past_cap_fails_closed() {
        let mut state = host_state(1024, 10);
        let err = wasmtime::ResourceLimiter::memory_growing(&mut state, 0, 2048, None)
            .expect_err("past cap must be denied");
        assert!(err.downcast_ref::<MemoryCapExceeded>().is_some());
    }
}
