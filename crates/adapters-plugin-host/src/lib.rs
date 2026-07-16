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
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

#[allow(missing_docs, reason = "generated bindings")]
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
    /// Wall-clock deadline for one call.
    pub deadline: Duration,
}

impl Default for PluginLimits {
    fn default() -> Self {
        Self {
            max_fuel: 10_000_000_000,
            max_memory_bytes: 64 * 1024 * 1024,
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

struct HostState {
    wasi: WasiCtx,
    table: ResourceTable,
    max_memory_bytes: usize,
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
    /// any guest code.
    pub fn load(&self, wasm_bytes: &[u8]) -> Result<LoadedPlugin, HostError> {
        let component = Component::new(&self.engine, wasm_bytes)
            .map_err(|e| HostError::InvalidComponent(e.to_string()))?;
        Ok(LoadedPlugin { component })
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

        let wasi = WasiCtxBuilder::new().build();
        let mut store = Store::new(
            &self.engine,
            HostState {
                wasi,
                table: ResourceTable::new(),
                max_memory_bytes: limits.max_memory_bytes,
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
            Ok(Ok(extraction)) => convert_extraction(extraction),
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
        _desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(true)
    }
}
