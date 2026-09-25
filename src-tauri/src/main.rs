//! Cartograph desktop shell (M0): boots the webview, owns the graph store and
//! the durable job spine, and exposes the first Tauri commands.

// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod context;
mod escalation;
mod evidence;
mod findings;
#[cfg(test)]
mod graph_projection_tests;
mod investigations;
mod job_execution;
#[cfg(test)]
mod job_execution_host_tests;
mod jobs;
mod metrics;
mod paths;
mod primary_source;
#[cfg(test)]
mod primary_source_tests;
mod proposals;
#[cfg(test)]
mod registered_source_tests;
mod settings;
mod source_access;
mod sources;
mod task_evidence;

use core_graph::{Edge, GraphStore, Node, SqliteGraphStore};
use findings::{Finding, FindingStore, NewFinding};
use job_execution::{JobExecution, JobExecutionLocks};
use jobs::{
    ClaimMode, EvalResult, ExecutionCheck, ExecutionUpdate, Job, JobStore, JobTransitionError,
};
use llm::LlmProvider;
use serde::Serialize;
use source_access::SourceOperation;
use sources::{RegisteredSource, SourceRegistry};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager, State};

/// Stores managed by the Tauri runtime. Graph and state spine are separate
/// databases (ADR-0008): the graph is a disposable ingest artifact, the spine
/// holds durable state.
struct AppState {
    graph: Mutex<SqliteGraphStore>,
    jobs: Mutex<JobStore>,
    job_execution_locks: JobExecutionLocks,
    investigations: investigations::InvestigationRuntime,
    findings: Mutex<FindingStore>,
    settings: Mutex<settings::SettingsStore>,
    decisions: Mutex<agents::DecisionLog>,
    proposals: Mutex<agents::ProposalStore>,
    extraction_caches: Mutex<ExtractionCaches>,
    /// Durable, host-owned authority for operational source locations.
    sources: Arc<Mutex<SourceRegistry>>,
    primary_sources: primary_source::PrimarySourceStore,
    metrics: Mutex<metrics::MetricsStore>,
}

#[derive(Default)]
struct RepoExtractionCache {
    ts: adapters_lang_ts::IncrementalCache,
    python: adapters_lang_python::IncrementalCache,
    go: adapters_lang_go::IncrementalCache,
    java: adapters_lang_java::IncrementalCache,
    kotlin: adapters_lang_kotlin::IncrementalCache,
    tf: iac::IncrementalCache,
}

#[derive(Default)]
struct ExtractionCaches {
    repos: std::collections::BTreeMap<String, RepoExtractionCache>,
}

#[derive(Serialize)]
struct GraphStats {
    nodes: u64,
    edges: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct LayerSummary {
    files: u64,
    nodes: u64,
    edges: u64,
}

impl LayerSummary {
    fn add(&mut self, other: Self) {
        self.files += other.files;
        self.nodes += other.nodes;
        self.edges += other.edges;
    }
}

/// The store keeps one fact per node id and per `(src, dst, label)` relation
/// (SPEC-00 §4.4), so counts quote distinct keys, never raw occurrences —
/// otherwise the ingest summary and the Workspace graph counts disagree
/// (AC-0195, #242).
fn distinct_node_count(nodes: &[Node]) -> u64 {
    nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len() as u64
}

fn distinct_edge_count(edges: &[Edge]) -> u64 {
    edges
        .iter()
        .map(|edge| (edge.src.as_str(), edge.dst.as_str(), edge.label.as_str()))
        .collect::<std::collections::BTreeSet<_>>()
        .len() as u64
}

/// Occurrences an extraction emitted beyond the one fact the store keeps per
/// key. Stated in the summary so collapsing is never silent (AC-0195).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct MergedFacts {
    /// Repeated node ids beyond the first occurrence.
    nodes: u64,
    /// Repeated relations beyond the first — typically one call or import
    /// relation cited from several sites, whose evidence the stored edge
    /// unions (AC-0203).
    edges: u64,
    /// Node ids emitted with differing facts: distinct declarations sharing
    /// one identity, of which only the last occurrence is stored.
    node_collisions: u64,
}

impl MergedFacts {
    fn add(&mut self, other: Self) {
        self.nodes += other.nodes;
        self.edges += other.edges;
        self.node_collisions += other.node_collisions;
    }
}

type EdgeKey = (String, String, String);

/// The distinct fact keys one load wrote (its Repo node included) and what
/// it collapsed to get there.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PublishedFacts {
    nodes: std::collections::BTreeSet<String>,
    edges: std::collections::BTreeSet<EdgeKey>,
    merged: MergedFacts,
}

/// Every fact key one recovery operation published, across each repo it
/// loaded and each whole-graph stage it ran afterwards (BACKS stitching,
/// found-ADR relinking). It is the single source every ingest summary quotes
/// (AC-0195): a key published by two repos is one stored fact, so the
/// cross-repo repeat is reported as merged rather than counted twice.
#[derive(Debug, Default)]
struct OperationFacts {
    nodes: std::collections::BTreeSet<String>,
    edges: std::collections::BTreeSet<EdgeKey>,
    merged: MergedFacts,
}

impl OperationFacts {
    fn record_load(&mut self, loaded: PublishedFacts) {
        self.merged.add(loaded.merged);
        for id in loaded.nodes {
            if !self.nodes.insert(id) {
                self.merged.nodes += 1;
            }
        }
        for key in loaded.edges {
            if !self.edges.insert(key) {
                self.merged.edges += 1;
            }
        }
    }

    /// A later whole-graph stage re-publishing or retracting facts, in the
    /// store's own patch order: edge deletes, node deletes (with their
    /// incident edges), then upserts. Re-publishing a key is not a merge.
    fn record_patch(&mut self, patch: &core_graph::GraphPatch) {
        for key in &patch.delete_edges {
            self.edges.remove(key);
        }
        for id in &patch.delete_node_ids {
            self.nodes.remove(id);
            self.edges.retain(|(src, dst, _)| src != id && dst != id);
        }
        self.nodes
            .extend(patch.upsert_nodes.iter().map(|node| node.id.clone()));
        self.edges.extend(patch.upsert_edges.iter().map(edge_key));
    }

    fn nodes(&self) -> u64 {
        self.nodes.len() as u64
    }

    fn edges(&self) -> u64 {
        self.edges.len() as u64
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct LayerBreakdown {
    ts: LayerSummary,
    python: LayerSummary,
    go: LayerSummary,
    java: LayerSummary,
    kotlin: LayerSummary,
    tf: LayerSummary,
    webext: LayerSummary,
    tools: LayerSummary,
}

impl LayerBreakdown {
    fn add(&mut self, other: Self) {
        self.ts.add(other.ts);
        self.python.add(other.python);
        self.go.add(other.go);
        self.java.add(other.java);
        self.kotlin.add(other.kotlin);
        self.tf.add(other.tf);
        self.webext.add(other.webext);
        self.tools.add(other.tools);
    }

    fn files(self) -> u64 {
        self.ts.files
            + self.python.files
            + self.go.files
            + self.java.files
            + self.kotlin.files
            + self.tf.files
    }
}

#[derive(Serialize)]
struct PingReply {
    app: &'static str,
    version: &'static str,
}

#[tauri::command]
fn ping() -> PingReply {
    PingReply {
        app: "cartograph",
        version: env!("CARGO_PKG_VERSION"),
    }
}

#[tauri::command]
fn graph_stats(state: State<'_, AppState>) -> Result<GraphStats, String> {
    let graph = state.graph.lock().map_err(|e| e.to_string())?;
    let (nodes, edges) = graph.fact_counts().map_err(|e| e.to_string())?;
    Ok(GraphStats { nodes, edges })
}

/// One discovered plugin with its per-project lifecycle state (#198) and
/// its conformance-gate verdict for these exact bytes (#200).
#[derive(Serialize)]
struct PluginStatus {
    #[serde(flatten)]
    plugin: adapters_plugin_host::discovery::DiscoveredPlugin,
    /// Explicit per-project opt-in; absent rows are disabled (fail closed).
    enabled: bool,
    /// `passed`, `failed`, or `ungated` — the proposed state every artifact
    /// starts in. Keyed by content hash: replaced bytes are `ungated` again.
    gate: &'static str,
    /// First failing check as `name: detail` when the gate failed.
    gate_detail: Option<String>,
}

/// The lifecycle key for a plugin's enablement rows: the resolved project
/// root that supplied a project copy, or `"user"` for user-level copies.
fn plugin_settings_root(plugin: &adapters_plugin_host::discovery::DiscoveredPlugin) -> String {
    plugin
        .project_root
        .as_ref()
        .map(|root| root.display().to_string())
        .unwrap_or_else(|| "user".to_string())
}

/// Discovery metadata stays coupled to the managed roots that supplied it.
/// Keep this value alive through every dependent source read and publication;
/// a discovered path alone cannot prevent participating clone replacement.
struct SessionPluginDiscovery {
    plugins: Vec<adapters_plugin_host::discovery::DiscoveredPlugin>,
    _source_operation: SourceOperation,
}

/// Discover plugin artifacts: `.cartograph/adapters/` inside every resolved
/// registered ingest root (never the raw Connect input — a GitHub URL or
/// manifest path is not a directory, #203 review), then the user-level
/// adapters directory. Project wins on id conflict. Enablement joins on the
/// exact artifact hash, so replaced bytes are disabled again. Discovery
/// never runs guest code.
fn discover_session_plugins<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
) -> Result<SessionPluginDiscovery, String> {
    let user_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("adapters");
    let registered = state.sources.lock().map_err(|e| e.to_string())?.list()?;
    let available = registered
        .into_iter()
        .filter(RegisteredSource::is_ready)
        .collect::<Vec<_>>();
    let operation = SourceOperation::acquire(
        &state.sources,
        available.iter().cloned().map(|source| (source, false)),
    )?;
    let roots = available
        .iter()
        .map(|source| {
            operation
                .root(&source.repo_key)
                .map(std::path::Path::to_path_buf)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SessionPluginDiscovery {
        plugins: adapters_plugin_host::discovery::discover(&roots, &user_dir),
        _source_operation: operation,
    })
}

/// A plugin cleared for extraction on one project root (#201): discovered,
/// explicitly enabled for these exact bytes, and gate-passed for these
/// exact bytes. `extensions` is the coverage claim from its golden corpus
/// — the gate already proved the corpus, so the claim is trusted as far as
/// routing; the facts themselves are still bounded and pinned per call.
struct ActivePlugin {
    plugin_id: String,
    path: std::path::PathBuf,
    /// The gated hash — extraction re-verifies the bytes on disk still
    /// match before running them (fail closed on a swap).
    content_hash: String,
    extensions: Vec<String>,
}

/// The plugins allowed to extract for `root` right now. Everything about
/// this is fail-closed: no enablement row, a different artifact hash, a
/// missing/failed gate, or an unreadable/empty corpus each drop the plugin
/// from the active set silently — extraction never guesses.
fn active_plugins_for_root<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    root: &std::path::Path,
) -> Result<Vec<ActivePlugin>, String> {
    let user_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("adapters");
    active_plugins_in(state, root, &user_dir)
}

/// The scan behind [`active_plugins_for_root`], with the user directory
/// injected. Discovery runs for *this root only* (#208 review): a project
/// copy in some other session root must not shadow the user-level copy
/// this root's coverage relies on.
fn active_plugins_in(
    state: &AppState,
    root: &std::path::Path,
    user_dir: &std::path::Path,
) -> Result<Vec<ActivePlugin>, String> {
    let root_key = root.display().to_string();
    let discovered = adapters_plugin_host::discovery::discover(&[root.to_path_buf()], user_dir);
    let settings_store = state.settings.lock().map_err(|e| e.to_string())?;
    let mut active = Vec::new();
    for plugin in discovered {
        let settings_root = plugin_settings_root(&plugin);
        // A project copy only ever extracts for its own project; user-level
        // copies apply wherever their (user-scoped) enablement says so.
        if settings_root != root_key && settings_root != "user" {
            continue;
        }
        let enabled = settings_store
            .enabled_plugins(&settings_root)
            .map_err(|e| e.to_string())?
            .iter()
            .any(|(id, hash)| *id == plugin.id && *hash == plugin.content_hash);
        if !enabled {
            continue;
        }
        let gate_passed = matches!(
            settings_store
                .plugin_gate(&plugin.id, &plugin.content_hash)
                .map_err(|e| e.to_string())?,
            Some((true, _))
        );
        if !gate_passed {
            continue;
        }
        let corpus_path = plugin.path.with_extension("golden.json");
        let Ok(text) = std::fs::read_to_string(&corpus_path) else {
            continue;
        };
        let Ok(corpus) = serde_json::from_str::<adapters_plugin_host::gate::GoldenCorpus>(&text)
        else {
            continue;
        };
        if corpus.extensions.is_empty() {
            continue;
        }
        active.push(ActivePlugin {
            plugin_id: plugin.id,
            path: plugin.path,
            content_hash: plugin.content_hash,
            extensions: corpus.extensions,
        });
    }
    Ok(active)
}

#[tauri::command]
fn list_plugins(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<PluginStatus>, String> {
    let discovered = discover_session_plugins(&app, &state)?;
    let settings_store = state.settings.lock().map_err(|e| e.to_string())?;
    let mut statuses = Vec::with_capacity(discovered.plugins.len());
    for plugin in discovered.plugins {
        let enabled = settings_store
            .enabled_plugins(&plugin_settings_root(&plugin))
            .map_err(|e| e.to_string())?
            .iter()
            .any(|(id, hash)| *id == plugin.id && *hash == plugin.content_hash);
        let (gate, gate_detail) = match settings_store
            .plugin_gate(&plugin.id, &plugin.content_hash)
            .map_err(|e| e.to_string())?
        {
            Some((true, _)) => ("passed", None),
            Some((false, report_json)) => ("failed", first_failing_check(&report_json)),
            None => ("ungated", None),
        };
        statuses.push(PluginStatus {
            enabled,
            gate,
            gate_detail,
            plugin,
        });
    }
    Ok(statuses)
}

/// `name: detail` of the first failing check in a stored gate report — the
/// one line a user needs to see next to a `failed` chip.
fn first_failing_check(report_json: &str) -> Option<String> {
    let report: serde_json::Value = serde_json::from_str(report_json).ok()?;
    report["checks"].as_array()?.iter().find_map(|check| {
        if check["passed"].as_bool() == Some(false) {
            Some(format!(
                "{}: {}",
                check["name"].as_str().unwrap_or("check"),
                check["detail"].as_str().unwrap_or("failed")
            ))
        } else {
            None
        }
    })
}

/// The fixed source identity conformance corpora are authored against:
/// golden node/edge ids that embed the repo must use `golden` (the host
/// hands the plugin this exact repo/commit during the gate, and only then).
fn gate_source_id() -> adapters_plugin_host::SourceId {
    adapters_plugin_host::SourceId {
        repo: "golden".to_string(),
        commit: "golden".to_string(),
    }
}

/// Run the conformance gate for one discovered plugin as a durable job
/// (#200, AC-0068): SPI contract under the standard bounds, the
/// generator-supplied golden corpus (`{plugin-id}.golden.json` next to the
/// artifact), and a double-run determinism check. The verdict persists per
/// (plugin id, content hash) — a missing or unreadable corpus records a
/// failed gate, never a skipped one, and the plugin stays proposed until a
/// recorded pass for these exact bytes.
#[tauri::command]
async fn run_plugin_gate(
    plugin_id: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let (running, execution) = start_job(&state, &format!("plugin-gate:{plugin_id}"))?;
    emit_job(&app, &running);
    off_ui_thread_for_job(execution, move |execution| {
        plugin_gate_blocking(&plugin_id, execution, &app)
    })
    .await
}

/// The gate pipeline behind one already-running `plugin-gate:{id}` job —
/// shared by the command above and the Jobs retry path, so an interrupted
/// or failed gate reruns instead of dead-ending. Cancellation is honored
/// before the verdict persists: a cancelled job never changes the trusted
/// artifact state.
fn plugin_gate_blocking<R: tauri::Runtime>(
    plugin_id: &str,
    execution: &JobExecution,
    app: &tauri::AppHandle<R>,
) -> Result<serde_json::Value, String> {
    let state = app.state::<AppState>();
    let fail = |error: String| -> String {
        report_failure(app, &state, execution, &error);
        error
    };

    if job_cancelled(&state, execution)? {
        return Err("cancelled".into());
    }

    report_progress(app, &state, execution, "discover", 10.0).map_err(&fail)?;
    // Retain the discovery's shared source guards through both filesystem
    // reads, gate execution and verdict publication. This coordinates managed
    // checkout replacement, not external edits or user-level plugin writes.
    let discovery = discover_session_plugins(app, &state).map_err(&fail)?;
    let plugin = discovery
        .plugins
        .iter()
        .find(|plugin| plugin.id == plugin_id)
        .ok_or_else(|| fail(format!("no discovered plugin with id {plugin_id}")))?;
    // Hash the bytes actually gated, not the discovery-time snapshot: the
    // verdict must bind to what ran even if the file changed in between.
    ensure_running_job(&state, execution)?;
    let wasm_bytes = std::fs::read(&plugin.path).map_err(|e| fail(e.to_string()))?;
    let content_hash = core_prov::content_hash(&wasm_bytes);

    report_progress(app, &state, execution, "gate", 30.0).map_err(&fail)?;
    let corpus_path = plugin.path.with_extension("golden.json");
    let report = match std::fs::read_to_string(&corpus_path)
        .map_err(|e| e.to_string())
        .and_then(|text| {
            serde_json::from_str::<adapters_plugin_host::gate::GoldenCorpus>(&text)
                .map_err(|e| e.to_string())
        }) {
        Ok(corpus) => {
            ensure_running_job(&state, execution)?;
            let host = adapters_plugin_host::PluginHost::new().map_err(|e| fail(e.to_string()))?;
            adapters_plugin_host::gate::run_gate(
                &host,
                plugin_id,
                &wasm_bytes,
                &corpus,
                adapters_plugin_host::PluginLimits::default(),
                &gate_source_id(),
            )
        }
        // No corpus, no proof: record the failure instead of erroring, so
        // the artifact is provably `failed`, not indefinitely `ungated`.
        Err(error) => adapters_plugin_host::gate::GateReport {
            passed: false,
            checks: vec![adapters_plugin_host::gate::GateCheck {
                name: "corpus-present".to_string(),
                passed: false,
                detail: format!("{}: {error}", corpus_path.display()),
            }],
        },
    };

    // A cancel that landed while the gate ran wins outright: the job row
    // stays cancelled and the verdict is discarded, so the visible job
    // outcome and the trusted artifact state never diverge (#206 review).
    if job_cancelled(&state, execution)? {
        return Err("cancelled".to_string());
    }

    report_progress(app, &state, execution, "record", 90.0).map_err(&fail)?;
    let report_json = serde_json::to_value(&report).map_err(|e| fail(e.to_string()))?;
    {
        let mut settings_store = state.settings.lock().map_err(|e| e.to_string())?;
        ensure_running_job(&state, execution)?;
        settings_store
            .record_plugin_gate(
                plugin_id,
                &content_hash,
                report.passed,
                &report_json.to_string(),
            )
            .map_err(|e| fail(e.to_string()))?;
    }

    let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    let job = updated_job(
        jobs.finish_execution(execution, &[format!("gate:{plugin_id}@{content_hash}")])
            .map_err(|e| e.to_string())?,
    );
    emit_job(app, &job);
    completed_job(&job)?;
    Ok(report_json)
}

/// Persist a per-project plugin opt-in/out (#198), bound to the exact
/// artifact hash (#203 review). Enabling never runs the plugin here —
/// extraction happens only behind the conformance gate.
#[tauri::command]
fn set_plugin_enabled(
    project_root: String,
    plugin_id: String,
    content_hash: String,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut settings_store = state.settings.lock().map_err(|e| e.to_string())?;
    settings_store
        .set_plugin_enabled(&project_root, &plugin_id, &content_hash, enabled)
        .map_err(|e| e.to_string())
}

/// The adapter inventory (#163): the same registry Preflight consults, so
/// Settings and coverage can never disagree. Static per build.
#[derive(Serialize)]
struct AdapterInventory {
    installed: &'static [ingest::preflight::AdapterInfo],
    planned: &'static [ingest::preflight::PlannedAdapter],
    detector: &'static str,
}

#[tauri::command]
fn adapter_inventory() -> AdapterInventory {
    AdapterInventory {
        installed: ingest::preflight::INSTALLED_ADAPTERS,
        planned: ingest::preflight::PLANNED_ADAPTERS,
        detector: ingest::preflight::DETECTOR_ID,
    }
}

/// One repo currently contributing facts to the unified graph (#162).
#[derive(Debug, Serialize, PartialEq, Eq)]
struct SystemRepo {
    repo: String,
    commit: String,
    display_name: Option<String>,
}

/// What the current system contains, derived from the graph's own facts
/// (never from history logs, which survive a clear): each distinct repo
/// any node's evidence cites, with its recorded commit identity. Evidence
/// is the source so infra-only (Resource) and manifest-only (Extension)
/// repos count too, not just ones with File nodes (#187 review).
/// Deterministic: sorted by repo.
#[tauri::command]
fn system_contents(state: State<'_, AppState>) -> Result<Vec<SystemRepo>, String> {
    let nodes = state
        .graph
        .lock()
        .map_err(|e| e.to_string())?
        .all_nodes()
        .map_err(|e| e.to_string())?;
    let mut contents = system_contents_of(&nodes);
    let registry = state.sources.lock().map_err(|e| e.to_string())?;
    for entry in &mut contents {
        entry.display_name = registry
            .get_by_repo(&entry.repo)?
            .map(|source| source.display_name);
    }
    Ok(contents)
}

fn system_contents_of(nodes: &[Node]) -> Vec<SystemRepo> {
    let mut repos: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for node in nodes {
        let evidence = &node.props["prov"]["evidence"][0];
        let Some(repo) = evidence["repo"].as_str().filter(|repo| !repo.is_empty()) else {
            continue;
        };
        let commit = evidence["commit_sha"].as_str().unwrap_or("workdir");
        repos.entry(repo.to_string()).or_insert(commit.to_string());
    }
    repos
        .into_iter()
        .map(|(repo, commit)| SystemRepo {
            repo,
            commit,
            display_name: None,
        })
        .collect()
}

fn clear_graph_store(graph: &mut SqliteGraphStore) -> Result<GraphStats, String> {
    graph.clear().map_err(|e| e.to_string())?;
    Ok(GraphStats { nodes: 0, edges: 0 })
}

#[tauri::command]
fn clear_graph(state: State<'_, AppState>) -> Result<GraphStats, String> {
    let mut graph = state.graph.lock().map_err(|e| e.to_string())?;
    let stats = clear_graph_store(&mut graph)?;
    state
        .extraction_caches
        .lock()
        .map_err(|e| e.to_string())?
        .repos
        .clear();
    Ok(stats)
}

#[tauri::command]
fn clear_finished_jobs(state: State<'_, AppState>) -> Result<usize, String> {
    let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    jobs.clear_finished().map_err(|e| e.to_string())
}

#[tauri::command]
fn list_jobs(state: State<'_, AppState>) -> Result<Vec<Job>, String> {
    let jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    jobs.list().map_err(|e| e.to_string())
}

#[tauri::command]
fn list_evals(state: State<'_, AppState>) -> Result<Vec<EvalResult>, String> {
    let jobs = state.jobs.lock().map_err(|error| error.to_string())?;
    jobs.list_evals().map_err(|error| error.to_string())
}

/// Legacy decision history; these caller-body records are not staged proposals.
#[tauri::command]
fn list_agent_decisions(state: State<'_, AppState>) -> Result<Vec<agents::DecisionRecord>, String> {
    let decisions = state.decisions.lock().map_err(|error| error.to_string())?;
    decisions.list().map_err(|error| error.to_string())
}

/// Historical decisions matching a legacy basis digest. This lookup cannot
/// establish source freshness or activate a proposal in curated context.
#[tauri::command]
fn reapply_agent_decisions(
    basis_hash: String,
    state: State<'_, AppState>,
) -> Result<Vec<agents::DecisionRecord>, String> {
    let decisions = state.decisions.lock().map_err(|error| error.to_string())?;
    decisions
        .reapply(&basis_hash)
        .map_err(|error| error.to_string())
}

/// Persist accept/reject/annotate for one cited T2/T3 Workbench assertion.
#[tauri::command]
fn record_assertion_decision(
    assertion: agents::CuratableAssertion,
    decision: agents::AssertionDecision,
    note: Option<String>,
    state: State<'_, AppState>,
) -> Result<agents::AssertionDecisionRecord, String> {
    let mut decisions = state.decisions.lock().map_err(|error| error.to_string())?;
    decisions
        .record_assertion(&assertion, decision, note.as_deref())
        .map_err(|error| error.to_string())
}

/// All content-addressed Workbench decisions, newest first.
#[tauri::command]
fn list_assertion_decisions(
    state: State<'_, AppState>,
) -> Result<Vec<agents::AssertionDecisionRecord>, String> {
    let decisions = state.decisions.lock().map_err(|error| error.to_string())?;
    decisions
        .list_assertions()
        .map_err(|error| error.to_string())
}

#[derive(Serialize)]
struct IngestSummary {
    job_id: i64,
    files: u64,
    nodes: u64,
    edges: u64,
    merged: MergedFacts,
    layers: LayerBreakdown,
    delta: DeltaSummary,
    /// The preflight report reconciled with this recovery's AST proof
    /// (AC-0200), so the Preflight surface can replace its pending view.
    preflight: ingest::preflight::PreflightReport,
    /// Where that report stands in the register order (#458); `None` when a
    /// newer preflight's findings were already there, so it wrote nothing.
    preflight_register: Option<RegisterStamp>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct DeltaSummary {
    recomputed_files: u64,
    reused_files: u64,
    deleted_files: u64,
}

impl DeltaSummary {
    fn add(&mut self, recomputed_files: u64, reused_files: u64, deleted_files: u64) {
        self.recomputed_files += recomputed_files;
        self.reused_files += reused_files;
        self.deleted_files += deleted_files;
    }
}

/// The cross-layer T0 pipeline over one tree: TypeScript, Python, Go, Terraform,
/// channel stitching, client fetch resolution — closed over so the
/// FK-enforcing store never sees a dangling endpoint.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn extract_tree_with_summary(
    root: &std::path::Path,
    repo: &str,
    commit: &str,
    layers: &[String],
    manifest_env: &std::collections::BTreeMap<String, String>,
    state_json: Option<&std::path::Path>,
    pulumi_json: Option<&std::path::Path>,
    otel_jsonl: &[std::path::PathBuf],
) -> Result<(adapters_lang_ts::Extraction, LayerBreakdown), String> {
    let mut cache = RepoExtractionCache::default();
    extract_tree_incremental(
        root,
        repo,
        commit,
        layers,
        manifest_env,
        state_json,
        pulumi_json,
        otel_jsonl,
        &mut cache,
        &[],
        &mut |_| {},
    )
    .map(|(extraction, layers, _)| (extraction, layers))
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn extract_tree_incremental(
    root: &std::path::Path,
    repo: &str,
    commit: &str,
    layers: &[String],
    manifest_env: &std::collections::BTreeMap<String, String>,
    state_json: Option<&std::path::Path>,
    pulumi_json: Option<&std::path::Path>,
    otel_jsonl: &[std::path::PathBuf],
    cache: &mut RepoExtractionCache,
    plugins: &[ActivePlugin],
    on_file: &mut dyn FnMut(&str),
) -> Result<(adapters_lang_ts::Extraction, LayerBreakdown, DeltaSummary), String> {
    extract_tree_with_primary(
        root,
        repo,
        commit,
        layers,
        manifest_env,
        state_json,
        pulumi_json,
        otel_jsonl,
        cache,
        plugins,
        on_file,
        None,
        &mut Vec::new(),
    )
}

#[allow(clippy::too_many_arguments)]
fn extract_tree_with_primary(
    root: &std::path::Path,
    repo: &str,
    commit: &str,
    layers: &[String],
    manifest_env: &std::collections::BTreeMap<String, String>,
    state_json: Option<&std::path::Path>,
    pulumi_json: Option<&std::path::Path>,
    otel_jsonl: &[std::path::PathBuf],
    cache: &mut RepoExtractionCache,
    plugins: &[ActivePlugin],
    on_file: &mut dyn FnMut(&str),
    primary: Option<&source_capture::Capture>,
    receipts: &mut Vec<adapters_lang_ts::captured::Receipt>,
) -> Result<(adapters_lang_ts::Extraction, LayerBreakdown, DeltaSummary), String> {
    // Layer hints gate extractors (AC-0002): empty means everything; the
    // The TS pass covers server/events/client plus Pulumi infra/cloud; the HCL
    // pass covers Terraform infra/cloud.
    let wants =
        |names: &[&str]| layers.is_empty() || names.iter().any(|n| layers.iter().any(|l| l == n));
    let wants_application = wants(&["server", "events", "client"]);
    let wants_server = wants(&["server"]);
    let wants_infra = wants(&["infra", "cloud"]);
    let ts_id = adapters_lang_ts::SourceId { repo, commit };
    let mut layers = LayerBreakdown::default();
    let mut delta = DeltaSummary::default();
    let mut extraction = if wants_application || wants_infra {
        // #209 live detail: the TS pass covers application code when that
        // layer is wanted, otherwise it's only running for Pulumi infra
        // bindings — say which one is actually happening.
        let ts_phase = if wants_application {
            "Reading application code"
        } else {
            "Reading infrastructure (Pulumi)"
        };
        let mut ts_progress = |path: &str| on_file(&format!("{ts_phase} — {path}"));
        let (mut extraction, stats) = if let Some(capture) = primary {
            let (extraction, produced, stats) = adapters_lang_ts::captured::extract_captured_dir(
                root,
                &ts_id,
                capture,
                &mut ts_progress,
            )
            .map_err(|e| e.to_string())?;
            receipts.extend(produced);
            (extraction, stats)
        } else {
            adapters_lang_ts::extract_dir_incremental_with_progress(
                root,
                &ts_id,
                &mut cache.ts,
                &mut ts_progress,
            )
            .map_err(|e| e.to_string())?
        };
        delta.add(
            stats.recomputed_files,
            stats.reused_files,
            stats.deleted_files,
        );
        if !wants_application {
            extraction.retain_only_pulumi();
        }
        extraction
    } else {
        adapters_lang_ts::Extraction::default()
    };
    layers.ts = LayerSummary {
        files: extraction
            .nodes
            .iter()
            .filter(|node| node.label == "File" && node.props.get("placeholder").is_none())
            .count() as u64,
        nodes: distinct_node_count(&extraction.nodes),
        edges: distinct_edge_count(&extraction.edges),
    };
    if wants_application {
        // WebExtension manifests (US-0016): topology + permission facts.
        // Runs after the TS pass so entry bindings reuse its File nodes.
        let known_files: std::collections::BTreeSet<String> = extraction
            .nodes
            .iter()
            .filter(|node| node.label == "File")
            .map(|node| node.id.clone())
            .collect();
        let (webext, manifests) =
            adapters_lang_ts::webextension::extract_manifests(root, &ts_id, &known_files)
                .map_err(|e| e.to_string())?;
        layers.webext = LayerSummary {
            files: manifests,
            nodes: distinct_node_count(&webext.nodes),
            edges: distinct_edge_count(&webext.edges),
        };
        extraction.nodes.extend(webext.nodes);
        extraction.edges.extend(webext.edges);
        // Chrome runtime messaging sites join the same channel stitch as
        // SDK event sites: literals confirm, computed identities gap.
        extraction.event_sites.extend(
            adapters_lang_ts::chrome_messaging::extract_dir(root, &ts_id)
                .map_err(|e| e.to_string())?,
        );
        // IndexedDB schema/store declarations and repository operations
        // become the cited data model (DataEntity + READS/WRITES).
        let idb =
            adapters_lang_ts::indexeddb::extract_dir(root, &ts_id).map_err(|e| e.to_string())?;
        layers.webext.nodes += distinct_node_count(&idb.nodes);
        layers.webext.edges += distinct_edge_count(&idb.edges);
        extraction.nodes.extend(idb.nodes);
        extraction.edges.extend(idb.edges);
    }
    if wants_server {
        let python_id = adapters_lang_python::SourceId { repo, commit };
        let mut python_progress = |path: &str| on_file(&format!("Reading Python sources — {path}"));
        let (python, stats) = adapters_lang_python::extract_dir_incremental_with_progress(
            root,
            &python_id,
            &mut cache.python,
            &mut python_progress,
        )
        .map_err(|error| error.to_string())?;
        delta.add(
            stats.recomputed_files,
            stats.reused_files,
            stats.deleted_files,
        );
        layers.python = LayerSummary {
            files: python
                .nodes
                .iter()
                .filter(|node| node.label == "File" && node.props.get("placeholder").is_none())
                .count() as u64,
            nodes: distinct_node_count(&python.nodes),
            edges: distinct_edge_count(&python.edges),
        };
        extraction.nodes.extend(python.nodes);
        extraction.edges.extend(python.edges);

        let go_id = adapters_lang_go::SourceId { repo, commit };
        let mut go_progress = |path: &str| on_file(&format!("Reading Go sources — {path}"));
        let (go, stats) = adapters_lang_go::extract_dir_incremental_with_progress(
            root,
            &go_id,
            &mut cache.go,
            &mut go_progress,
        )
        .map_err(|error| error.to_string())?;
        delta.add(
            stats.recomputed_files,
            stats.reused_files,
            stats.deleted_files,
        );
        layers.go = LayerSummary {
            files: go
                .nodes
                .iter()
                .filter(|node| node.label == "File" && node.props.get("placeholder").is_none())
                .count() as u64,
            nodes: distinct_node_count(&go.nodes),
            edges: distinct_edge_count(&go.edges),
        };
        extraction.nodes.extend(go.nodes);
        extraction.edges.extend(go.edges);

        let java_id = adapters_lang_java::SourceId { repo, commit };
        let mut java_progress = |path: &str| on_file(&format!("Reading Java sources — {path}"));
        let (java, stats) = adapters_lang_java::extract_dir_incremental_with_progress(
            root,
            &java_id,
            &mut cache.java,
            &mut java_progress,
        )
        .map_err(|error| error.to_string())?;
        delta.add(
            stats.recomputed_files,
            stats.reused_files,
            stats.deleted_files,
        );
        layers.java = LayerSummary {
            files: java
                .nodes
                .iter()
                .filter(|node| node.label == "File" && node.props.get("placeholder").is_none())
                .count() as u64,
            nodes: distinct_node_count(&java.nodes),
            edges: distinct_edge_count(&java.edges),
        };
        extraction.nodes.extend(java.nodes);
        extraction.edges.extend(java.edges);

        let kotlin_id = adapters_lang_kotlin::SourceId { repo, commit };
        let mut kotlin_progress = |path: &str| on_file(&format!("Reading Kotlin sources — {path}"));
        let (kotlin, stats) = adapters_lang_kotlin::extract_dir_incremental_with_progress(
            root,
            &kotlin_id,
            &mut cache.kotlin,
            &mut kotlin_progress,
        )
        .map_err(|error| error.to_string())?;
        delta.add(
            stats.recomputed_files,
            stats.reused_files,
            stats.deleted_files,
        );
        layers.kotlin = LayerSummary {
            files: kotlin
                .nodes
                .iter()
                .filter(|node| node.label == "File" && node.props.get("placeholder").is_none())
                .count() as u64,
            nodes: distinct_node_count(&kotlin.nodes),
            edges: distinct_edge_count(&kotlin.edges),
        };
        extraction.nodes.extend(kotlin.nodes);
        extraction.edges.extend(kotlin.edges);
    }
    if wants_infra {
        let tf_id = iac::SourceId { repo, commit };
        let mut tf_progress =
            |path: &str| on_file(&format!("Reading infrastructure (Terraform) — {path}"));
        let (tf, stats) = iac::extract_dir_incremental_with_progress(
            root,
            &tf_id,
            &mut cache.tf,
            &mut tf_progress,
        )
        .map_err(|e| e.to_string())?;
        delta.add(
            stats.recomputed_files,
            stats.reused_files,
            stats.deleted_files,
        );
        layers.tf = LayerSummary {
            // The file contexts this run actually parsed or reused, which
            // already includes every local-module instantiation's files
            // (an explicit `source` reference overrides `.gitignore`, #468,
            // ADR-0034) — matching extraction file-for-file, unlike a
            // separate filesystem walk that doesn't know which directories
            // module declarations reference.
            files: stats.recomputed_files + stats.reused_files,
            nodes: distinct_node_count(&tf.nodes),
            edges: distinct_edge_count(&tf.edges),
        };
        extraction.nodes.extend(tf.nodes);
        extraction.edges.extend(tf.edges);
        // T1: observed state supersedes ambiguous T0 refs (AC-0009).
        if let Some(state_path) = state_json {
            let raw = std::fs::read_to_string(state_path)
                .map_err(|e| format!("state_json {}: {e}", state_path.display()))?;
            let observed = dynamic::parse_state(&raw).map_err(|e| e.to_string())?;
            dynamic::enrich_resources(
                &mut extraction.nodes,
                repo,
                &observed,
                &state_path.to_string_lossy(),
                &raw,
            );
        }
        if let Some(pulumi_path) = pulumi_json {
            let raw = std::fs::read_to_string(pulumi_path)
                .map_err(|error| format!("pulumi_json {}: {error}", pulumi_path.display()))?;
            let deployment = dynamic::parse_pulumi_json(&raw).map_err(|error| error.to_string())?;
            dynamic::enrich_pulumi_resources(
                &mut extraction.nodes,
                &deployment,
                &pulumi_path.to_string_lossy(),
            );
        }
    }
    let mut cfg = events::ConfigIndex::from_dir(root).map_err(|e| e.to_string())?;
    cfg.apply_manifest(manifest_env, ingest::manifest::MANIFEST_NAME);
    let ev_id = events::SourceId { repo, commit };
    let stitched = events::stitch(&extraction.event_sites, &cfg, &ev_id);
    layers.ts.nodes += distinct_node_count(&stitched.nodes);
    layers.ts.edges += distinct_edge_count(&stitched.edges);
    extraction.nodes.extend(stitched.nodes);
    extraction.edges.extend(stitched.edges);
    let endpoint_ids: Vec<String> = extraction
        .nodes
        .iter()
        .filter(|n| n.label == "Endpoint")
        .map(|n| n.id.clone())
        .collect();
    let fetched = events::stitch_fetches(&extraction.fetch_sites, &endpoint_ids, &cfg, &ev_id);
    layers.ts.nodes += distinct_node_count(&fetched.nodes);
    layers.ts.edges += distinct_edge_count(&fetched.edges);
    extraction.nodes.extend(fetched.nodes);
    extraction.edges.extend(fetched.edges);
    // T1: observed messaging identities fill only explicit channel Gaps;
    // observed HTTP attributes enrich T0 endpoints beside their provenance.
    for trace_path in otel_jsonl {
        let raw = std::fs::read_to_string(trace_path)
            .map_err(|e| format!("otel_jsonl {}: {e}", trace_path.display()))?;
        let trace = dynamic::parse_otlp_jsonl(&raw).map_err(|e| e.to_string())?;
        dynamic::apply_trace(
            &mut extraction.nodes,
            &mut extraction.edges,
            &trace,
            &trace_path.to_string_lossy(),
        );
    }
    // Found ADR/RFC files are T0 facts. They are parsed after all code/infra
    // layers so explicit `Governs:` ids can link to existing graph targets.
    let adr_facts = spec::extract_found_adrs(root, repo, commit, &extraction.nodes)
        .map_err(|error| error.to_string())?;
    extraction.nodes.extend(adr_facts.nodes);
    extraction.edges.extend(adr_facts.edges);
    // Toolchain facts (#215): config files become Tool nodes with cited
    // settings, DEFINED_IN the config File that proves them. Runs for every
    // layer selection — the toolchain is cross-cutting evidence.
    {
        let tool_id = ingest::toolchain::SourceId { repo, commit };
        let mut tool_progress = |path: &str| on_file(&format!("Reading configuration — {path}"));
        let tool_facts = ingest::toolchain::extract_dir(root, &tool_id, &mut tool_progress)
            .map_err(|e| e.to_string())?;
        layers.tools = LayerSummary {
            files: tool_facts.files,
            nodes: distinct_node_count(&tool_facts.nodes),
            edges: distinct_edge_count(&tool_facts.edges),
        };
        // Config files an adapter already owns (a `.ts`-authored vite
        // config, a webext manifest) keep the adapter's richer File node;
        // the DEFINED_IN edge targets the same id either way. An import
        // placeholder (an imported `package.json`, #237) owns nothing: the
        // parsed config's File node supersedes it (last occurrence wins).
        let known_files: std::collections::BTreeSet<String> = extraction
            .nodes
            .iter()
            .filter(|node| node.label == "File" && node.props.get("placeholder").is_none())
            .map(|node| node.id.clone())
            .collect();
        extraction.nodes.extend(
            tool_facts
                .nodes
                .into_iter()
                .filter(|node| node.label != "File" || !known_files.contains(&node.id)),
        );
        extraction.edges.extend(tool_facts.edges);
    }
    // Gated plugin adapters (#201): route the files each active plugin
    // claims via its golden-corpus extensions. Every fact arrives pinned to
    // the exact artifact by the host; a failure fails the whole plugin pass
    // closed (zero partial facts, AC-0070).
    if !plugins.is_empty() {
        let host = adapters_plugin_host::PluginHost::new().map_err(|e| e.to_string())?;
        let plugin_source = adapters_plugin_host::SourceId {
            repo: repo.to_string(),
            commit: commit.to_string(),
        };
        for plugin in plugins {
            let wasm_bytes = std::fs::read(&plugin.path).map_err(|e| e.to_string())?;
            // The gate verdict binds to exact bytes: a swap since gating
            // fails closed rather than running ungated code.
            if core_prov::content_hash(&wasm_bytes) != plugin.content_hash {
                return Err(format!(
                    "plugin {} changed on disk since its gate passed — re-run the \
                     conformance gate",
                    plugin.plugin_id
                ));
            }
            let loaded = host
                .load(&wasm_bytes, &plugin.plugin_id)
                .map_err(|e| e.to_string())?;
            let facts = adapters_plugin_host::route::extract_claimed(
                &host,
                &loaded,
                root,
                &plugin.extensions,
                &plugin_source,
                adapters_plugin_host::PluginLimits::default(),
            )
            .map_err(|e| e.to_string())?;
            extraction.nodes.extend(facts.nodes);
            extraction.edges.extend(facts.edges);
        }
    }
    extraction.close_over_endpoints();
    // Each language adapter closed over its own extraction; a shared id
    // (`mod:foo` imported from TS and Go) can now carry one adapter's Gap
    // and another's Confirmed boundary. The store keeps the last duplicate,
    // so reconcile first: a Gap is never shown as Confirmed (#237 review).
    core_graph::placeholder::reconcile_placeholders(&mut extraction.nodes);
    Ok((extraction, layers, delta))
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn extract_tree(
    root: &std::path::Path,
    repo: &str,
    commit: &str,
    layers: &[String],
    manifest_env: &std::collections::BTreeMap<String, String>,
    state_json: Option<&std::path::Path>,
    pulumi_json: Option<&std::path::Path>,
    otel_jsonl: &[std::path::PathBuf],
) -> Result<adapters_lang_ts::Extraction, String> {
    extract_tree_with_summary(
        root,
        repo,
        commit,
        layers,
        manifest_env,
        state_json,
        pulumi_json,
        otel_jsonl,
    )
    .map(|(extraction, _)| extraction)
}

/// Load an extraction plus its `Repo` node (`repo:{identity}`, carrying the
/// tree root and commit so evidence reads resolve per repo).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ReconcileStats {
    inserted_or_updated: u64,
    unchanged: u64,
    deleted: u64,
    published: PublishedFacts,
}

fn fact_owned_by_repo(props: &serde_json::Value, repo: &str) -> bool {
    serde_json::from_value::<core_prov::Provenance>(props.get("prov").cloned().unwrap_or_default())
        .ok()
        .is_some_and(|provenance| {
            !provenance.evidence.is_empty()
                && provenance
                    .evidence
                    .iter()
                    .all(|evidence| evidence.repo == repo)
        })
}

fn id_explicitly_owned_by_repo(id: &str, repo: &str) -> bool {
    id == format!("repo:{repo}")
        || id.starts_with(&format!("file:{repo}@"))
        || id.starts_with(&format!("sym:{repo}@"))
        || id.starts_with(&format!("ep:{repo}@"))
        || id.starts_with(&format!("res:{repo}@"))
        || id.starts_with(&format!("screen:{repo}@"))
        || id.starts_with(&format!("adr:{repo}@"))
        || id.starts_with(&format!("tool:{repo}@"))
        || id.starts_with(&format!("gap:call:{repo}@"))
        || id.starts_with(&format!("gap:chan:{repo}@"))
        || id.starts_with(&format!("gap:fetch:{repo}@"))
}

fn edge_key(edge: &Edge) -> (String, String, String) {
    (edge.src.clone(), edge.dst.clone(), edge.label.clone())
}

/// Evidence spans one collapsed relation keeps. Truncation happens after the
/// deterministic sort and is recorded as `evidence_omitted` on the edge.
const MAX_MERGED_EDGE_EVIDENCE: usize = 32;

/// Collapse repeated `(src, dst, label)` occurrences to one edge per key
/// (AC-0203, #436). The last occurrence in extraction order still supplies
/// the props, tier and confidence; the evidence of every occurrence with the
/// same tier, confidence and extractor is unioned, sorted, de-duplicated and
/// bounded, so no call or import site loses its citation. When more than one
/// occurrence is unioned, the edge's hash is derived from their sorted
/// distinct hashes and the canonical span list (spans beyond the bound and
/// their count included), so adding, removing or moving any site changes it.
fn merge_edge_occurrences(edges: &[Edge]) -> std::collections::BTreeMap<EdgeKey, Edge> {
    let mut groups = std::collections::BTreeMap::<EdgeKey, Vec<&Edge>>::new();
    for edge in edges {
        groups.entry(edge_key(edge)).or_default().push(edge);
    }
    groups
        .into_iter()
        .map(|(key, occurrences)| (key, merge_edge_group(&occurrences)))
        .collect()
}

fn merge_edge_group(occurrences: &[&Edge]) -> Edge {
    let last = *occurrences.last().expect("groups are non-empty");
    let mut merged = last.clone();
    if occurrences.len() == 1 {
        return merged;
    }
    let parse = |edge: &Edge| {
        serde_json::from_value::<core_prov::Provenance>(edge.props.get("prov")?.clone()).ok()
    };
    let Some(mut prov) = parse(last) else {
        return merged;
    };
    let mut evidence = Vec::new();
    let mut hashes = std::collections::BTreeSet::new();
    let mut unioned = 0usize;
    for occurrence in occurrences
        .iter()
        .filter_map(|edge| parse(edge))
        .filter(|other| {
            other.tier == prov.tier
                && other.confidence_tier == prov.confidence_tier
                && other.extractor_id == prov.extractor_id
        })
    {
        evidence.extend(occurrence.evidence);
        hashes.insert(occurrence.content_hash);
        unioned += 1;
    }
    evidence.sort_by(|a, b| {
        (&a.repo, &a.path, a.byte_start, a.byte_end, &a.commit_sha).cmp(&(
            &b.repo,
            &b.path,
            b.byte_start,
            b.byte_end,
            &b.commit_sha,
        ))
    });
    evidence.dedup();
    let omitted = evidence.len().saturating_sub(MAX_MERGED_EDGE_EVIDENCE);
    if unioned > 1 {
        // Occurrences can share one fact hash while citing different sites
        // (duplicate imports), so the canonical span list — omitted spans
        // included — is part of the merged hash alongside the source hashes.
        let canonical = serde_json::to_vec(&("merged-edge-v1", &hashes, &evidence, omitted))
            .expect("merged evidence serializes");
        prov.content_hash = core_prov::content_hash(&canonical);
    }
    evidence.truncate(MAX_MERGED_EDGE_EVIDENCE);
    prov.evidence = evidence;
    merged.props["prov"] = serde_json::to_value(prov).expect("provenance serializes");
    if omitted > 0 {
        merged.props["evidence_omitted"] = serde_json::json!(omitted);
    }
    merged
}

#[cfg(test)]
fn load_into_graph(
    graph: &mut SqliteGraphStore,
    extraction: &adapters_lang_ts::Extraction,
    repo: &str,
    _root: &std::path::Path,
    commit: &str,
) -> Result<ReconcileStats, String> {
    load_into_graph_with_bindings(graph, extraction, repo, _root, commit, &[])
}

fn load_into_graph_with_bindings(
    graph: &mut SqliteGraphStore,
    extraction: &adapters_lang_ts::Extraction,
    repo: &str,
    _root: &std::path::Path,
    commit: &str,
    bindings: &[core_graph::source::SourceBinding],
) -> Result<ReconcileStats, String> {
    let repo_prov = core_prov::Provenance::new(
        core_prov::Tier::Deterministic,
        core_prov::ConfidenceTier::Confirmed,
        vec![],
        "app.ingest",
        &serde_json::to_vec(&("registered-repo-v1", repo, commit)).expect("serializes"),
    )
    .expect("within ceiling");
    let repo_node = Node {
        id: format!("repo:{repo}"),
        label: "Repo".into(),
        props: serde_json::json!({
            "commit": commit,
            "prov": serde_json::to_value(repo_prov).expect("serializes"),
        }),
    };
    // Last occurrence wins, in deterministic extraction order (SPEC-00 §4.4).
    let mut current_nodes = std::collections::BTreeMap::new();
    let mut colliding_ids = std::collections::BTreeSet::new();
    for node in &extraction.nodes {
        if let Some(previous) = current_nodes.insert(node.id.clone(), node.clone())
            && previous != *node
        {
            colliding_ids.insert(node.id.clone());
        }
    }
    let merged = MergedFacts {
        nodes: (extraction.nodes.len() - current_nodes.len()) as u64,
        edges: extraction.edges.len() as u64 - distinct_edge_count(&extraction.edges),
        node_collisions: colliding_ids.len() as u64,
    };
    current_nodes.insert(repo_node.id.clone(), repo_node);
    let current_edges = merge_edge_occurrences(&extraction.edges);
    let expected = graph.read_snapshot().map_err(|error| error.to_string())?;
    let existing_nodes = expected
        .0
        .iter()
        .cloned()
        .map(|node| (node.id.clone(), node))
        .collect::<std::collections::BTreeMap<_, _>>();
    let existing_edges = expected
        .1
        .iter()
        .cloned()
        .map(|edge| (edge_key(&edge), edge))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut patch = core_graph::GraphPatch::default();
    let mut stats = ReconcileStats {
        published: PublishedFacts {
            nodes: current_nodes.keys().cloned().collect(),
            edges: current_edges.keys().cloned().collect(),
            merged,
        },
        ..ReconcileStats::default()
    };
    let mut remaining_edge_keys = std::collections::BTreeSet::new();
    for (key, edge) in &existing_edges {
        if fact_owned_by_repo(&edge.props, repo) && !current_edges.contains_key(key) {
            patch
                .delete_edges
                .push((edge.src.clone(), edge.dst.clone(), edge.label.clone()));
            stats.deleted += 1;
        } else {
            remaining_edge_keys.insert(key.clone());
        }
    }
    remaining_edge_keys.extend(current_edges.keys().cloned());
    for (id, node) in &existing_nodes {
        if current_nodes.contains_key(id) {
            continue;
        }
        let has_remaining_incident = remaining_edge_keys
            .iter()
            .any(|(src, dst, _)| src == id || dst == id);
        let owned = fact_owned_by_repo(&node.props, repo) || id_explicitly_owned_by_repo(id, repo);
        let orphan_placeholder =
            node.props["placeholder"].as_bool() == Some(true) && !has_remaining_incident;
        if (owned && (id_explicitly_owned_by_repo(id, repo) || !has_remaining_incident))
            || orphan_placeholder
        {
            patch.delete_node_ids.push(id.clone());
            stats.deleted += 1;
        }
    }
    for node in current_nodes.values() {
        if existing_nodes.get(&node.id) == Some(node) {
            stats.unchanged += 1;
        } else {
            patch.upsert_nodes.push(node.clone());
            stats.inserted_or_updated += 1;
        }
    }
    for (key, edge) in &current_edges {
        if existing_edges.get(key) == Some(edge) {
            stats.unchanged += 1;
        } else {
            patch.upsert_edges.push(edge.clone());
            stats.inserted_or_updated += 1;
        }
    }
    if !graph
        .apply_patch_with_source_bindings_if_snapshot_matches(&expected, &patch, repo, bindings)
        .map_err(|error| error.to_string())?
    {
        return Err("Graph changed during recovery publication; recover the source again.".into());
    }
    Ok(stats)
}

#[cfg(test)]
fn deterministic_graph_hashes(graph: &impl GraphStore) -> Result<Vec<String>, String> {
    fn hash(props: &serde_json::Value) -> Option<&str> {
        (props["prov"]["tier"].as_str() == Some("Deterministic"))
            .then(|| props["prov"]["content_hash"].as_str())
            .flatten()
    }
    let (nodes, edges) = graph.read_snapshot().map_err(|error| error.to_string())?;
    let mut hashes = nodes
        .into_iter()
        .filter_map(|node| hash(&node.props).map(|hash| format!("node:{}:{hash}", node.id)))
        .chain(edges.into_iter().filter_map(|edge| {
            hash(&edge.props)
                .map(|hash| format!("edge:{}:{}:{}:{hash}", edge.src, edge.dst, edge.label))
        }))
        .collect::<Vec<_>>();
    hashes.sort();
    Ok(hashes)
}

/// Join observed infra to the event layer: insert a `BACKS` edge wherever
/// an enriched `Resource`'s observed identity names a `Channel` that code
/// actually publishes or subscribes (SPEC-00 §4.1, M6). Runs over the
/// whole graph after every load. Existing state-derived edges are reconciled
/// against the current candidates before `put_edge` upserts, so removed or
/// changed observations cannot leave stale cross-layer topology behind.
fn stitch_backings(
    graph: &mut SqliteGraphStore,
    facts: &mut OperationFacts,
) -> Result<u64, String> {
    let resources = graph
        .nodes_with_label("Resource")
        .map_err(|e| e.to_string())?;
    let mut candidates = std::collections::BTreeMap::new();
    for edge in dynamic::backing_candidates(&resources) {
        let channel_exists = graph
            .get_node(&edge.dst)
            .map_err(|e| e.to_string())?
            .is_some();
        if channel_exists {
            candidates.insert(edge_key(&edge), edge);
        }
    }
    for edge in graph
        .edges_with_labels(&["BACKS"])
        .map_err(|error| error.to_string())?
    {
        let extractor_id = edge.props["prov"]["extractor_id"].as_str();
        let is_observed_backing = matches!(
            extractor_id,
            Some(dynamic::EXTRACTOR_ID | dynamic::PULUMI_EXTRACTOR_ID)
        );
        if is_observed_backing && !candidates.contains_key(&edge_key(&edge)) {
            graph
                .delete_edge(&edge.src, &edge.dst, &edge.label)
                .map_err(|error| error.to_string())?;
            facts.edges.remove(&edge_key(&edge));
        }
    }
    for (key, edge) in &candidates {
        graph.put_edge(edge).map_err(|error| error.to_string())?;
        facts.edges.insert(key.clone());
    }
    Ok(candidates.len() as u64)
}

/// Reconcile explicit found-ADR target ids against the complete graph.
/// Each repo is initially extracted in isolation, but decisions in a docs
/// repo may govern facts loaded later from another repo in the same system.
/// A rescan first drops links previously owned by that repo's found ADRs and
/// removes ADR nodes whose source file disappeared, so re-ingest cannot retain
/// declarations that are no longer present.
fn relink_found_adrs(
    state: &AppState,
    operation: &SourceOperation,
    execution: &JobExecution,
    facts: &mut OperationFacts,
) -> Result<u64, String> {
    ensure_running_job(state, execution)?;
    let snapshot = state
        .graph
        .lock()
        .map_err(|e| e.to_string())?
        .read_snapshot()
        .map_err(|e| e.to_string())?;
    // Source guards were acquired before recovery. Root-dependent reads run
    // without a graph mutex and cannot recursively lock an exclusive owner.
    let updates = collect_found_adrs(&snapshot.0, |repo| {
        ensure_running_job(state, execution)?;
        operation.root(repo)
    })?;
    let (patch, linked) = found_adr_patch(&snapshot.0, &snapshot.1, updates);
    let mut graph = state.graph.lock().map_err(|e| e.to_string())?;
    ensure_running_job(state, execution)?;
    if !graph
        .apply_patch_if_snapshot_matches(&snapshot, &patch)
        .map_err(|e| e.to_string())?
    {
        return Err("graph context changed during ADR recovery; retry recovery".into());
    }
    facts.record_patch(&patch);
    Ok(linked)
}

fn collect_found_adrs<'a>(
    nodes: &[Node],
    root_for: impl Fn(&str) -> Result<&'a std::path::Path, String>,
) -> Result<Vec<(String, spec::AdrFacts)>, String> {
    let candidates = nodes
        .iter()
        .filter(|node| node.label != "ADR")
        .cloned()
        .collect::<Vec<_>>();
    nodes
        .iter()
        .filter(|node| node.label == "Repo")
        .map(|node| {
            let repo = node
                .id
                .strip_prefix("repo:")
                .ok_or("invalid repository fact identity")?;
            let root = root_for(repo)?;
            let commit = node.props["commit"].as_str().unwrap_or("workdir");
            let facts = spec::extract_found_adrs(root, repo, commit, &candidates)
                .map_err(|e| e.to_string())?;
            Ok((repo.to_owned(), facts))
        })
        .collect()
}

fn found_adr_patch(
    nodes: &[Node],
    edges: &[Edge],
    updates: Vec<(String, spec::AdrFacts)>,
) -> (core_graph::GraphPatch, u64) {
    let mut patch = core_graph::GraphPatch::default();
    let mut linked = 0;
    for (repo, facts) in updates {
        let adr_prefix = format!("adr:{repo}@");
        let existing_ids = nodes
            .iter()
            .filter(|node| {
                node.label == "ADR"
                    && node.id.starts_with(&adr_prefix)
                    && node.props["origin"].as_str() == Some("found")
            })
            .map(|node| node.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let current_ids = facts
            .nodes
            .iter()
            .map(|node| node.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        for edge in edges.iter().filter(|edge| {
            edge.label == "DECIDES"
                && (existing_ids.contains(&edge.src) || current_ids.contains(&edge.src))
        }) {
            patch.delete_edges.push(edge_key(edge));
        }
        patch
            .delete_node_ids
            .extend(existing_ids.difference(&current_ids).cloned());
        linked += facts.edges.len() as u64;
        patch.upsert_nodes.extend(facts.nodes);
        patch.upsert_edges.extend(facts.edges);
    }
    (patch, linked)
}

#[cfg(test)]
fn apply_found_adrs(
    graph: &mut SqliteGraphStore,
    updates: Vec<(String, spec::AdrFacts)>,
) -> Result<u64, String> {
    let snapshot = graph.read_snapshot().map_err(|e| e.to_string())?;
    let (patch, linked) = found_adr_patch(&snapshot.0, &snapshot.1, updates);
    if !graph
        .apply_patch_if_snapshot_matches(&snapshot, &patch)
        .map_err(|e| e.to_string())?
    {
        return Err("graph context changed during ADR recovery; retry recovery".into());
    }
    Ok(linked)
}

/// The single source of truth for register counts (#116): every surface —
/// Workspace outcome, Gaps & Drift, Provenance & Eval — reads these numbers
/// from this one query so they always reconcile (handoff §Interactions #3).
#[derive(Debug, Serialize, PartialEq, Eq)]
struct FindingsSummary {
    /// Explicit System Gaps in the graph (spec-register definition).
    gaps: u64,
    /// Unsupported patterns — tool limitations, never Gaps.
    unsupported: u64,
    /// Questions recovery found no evidence for.
    no_evidence: u64,
    /// ADR/code drift findings.
    drift: u64,
    /// gaps + unsupported + no_evidence (the register headline).
    open_findings: u64,
    /// Total graph facts (nodes + edges).
    graph_facts: u64,
}

/// Count register entries from the graph using the spec register's own
/// predicates — one definition, every surface.
fn summarize_register(
    nodes: &[Node],
    edges: &[Edge],
    unsupported: u64,
    no_evidence: u64,
) -> FindingsSummary {
    // One finding per unresolved fact (#241): an unresolved call emits a Gap
    // node plus a supporting Gap CALLS edge — spec::count_gap_findings folds
    // the pair into one finding while keeping an edge-only gap (no gap node
    // on either end) as its own. Shared definition, every surface (#116).
    let gaps = spec::count_gap_findings(nodes.iter(), edges.iter()) as u64;
    let drift = nodes
        .iter()
        .filter(|node| spec::is_drift_node(node))
        .count() as u64;
    FindingsSummary {
        gaps,
        unsupported,
        no_evidence,
        drift,
        open_findings: gaps + unsupported + no_evidence,
        graph_facts: (nodes.len() + edges.len()) as u64,
    }
}

/// Register tallies for every surface (#116): gap/drift counts from the
/// graph, unsupported/no-evidence from the findings store.
#[tauri::command]
fn findings_summary(state: State<'_, AppState>) -> Result<FindingsSummary, String> {
    let (nodes, edges) = {
        let graph = state.graph.lock().map_err(|e| e.to_string())?;
        graph.read_snapshot().map_err(|e| e.to_string())?
    };
    let (unsupported, no_evidence) = {
        let findings = state.findings.lock().map_err(|e| e.to_string())?;
        findings.counts().map_err(|e| e.to_string())?
    };
    Ok(summarize_register(&nodes, &edges, unsupported, no_evidence))
}

/// All persisted register findings (unsupported / no-evidence lanes).
#[tauri::command]
fn list_findings(state: State<'_, AppState>) -> Result<Vec<Finding>, String> {
    let findings = state.findings.lock().map_err(|e| e.to_string())?;
    findings.list().map_err(|e| e.to_string())
}

/// Local-only preflight over a directory (#116, AC-0055/AC-0059): detect
/// languages/frameworks/adapter coverage and classify constructs into the
/// three-way split — potential gaps vs unsupported patterns — before any
/// recovery runs. Zero egress; never invokes an LLM (T0 discipline).
/// Unsupported findings persist to the register so post-recovery surfaces
/// reconcile with what preflight predicted.
///
/// The scan parses every TS/JS file for eval-site claims, which takes tens of
/// minutes on a large repo, so it runs on a blocking worker like recovery
/// (AC-0078, AC-0197): the window stays interactive, `preflight://progress`
/// reports the file being read, and `cancel_preflight` stops it (#235).
#[tauri::command]
async fn preflight(
    path: String,
    run: u64,
    app: tauri::AppHandle,
) -> Result<PreflightResult, String> {
    start_preflight(path, run, app).await
}

/// The `preflight` command body, generic over the runtime so tests can prove
/// where it runs. The run is registered (superseding any earlier one) when
/// this is called — before the worker is queued — so a Cancel or a newer
/// preflight that arrives while the blocking pool is busy still reaches it,
/// and supersession follows invocation order (#434 review). `run` is the
/// UI's id for this scan, echoed on every progress ping so a superseded
/// scan's last ping can't overwrite the current one's display.
fn start_preflight<R: tauri::Runtime>(
    path: String,
    run: u64,
    app: tauri::AppHandle<R>,
) -> impl std::future::Future<Output = Result<PreflightResult, String>> {
    let token = app.state::<PreflightRuns>().begin();
    async move {
        let token = token?;
        off_ui_thread(move || {
            let state = app.state::<AppState>();
            let runs = app.state::<PreflightRuns>();
            let mut progress = preflight_progress_throttle(&app, run);
            preflight_blocking(&path, &app, &state, &runs, &token, &mut progress)
        })
        .await
    }
}

/// Stop the preflight in flight, if any (AC-0198). It stops before the next
/// file is parsed and persists no findings.
#[tauri::command]
fn cancel_preflight(runs: State<'_, PreflightRuns>) -> Result<(), String> {
    runs.cancel().map(drop)
}

/// The error a stopped preflight returns — the same word a cancelled job uses.
const PREFLIGHT_CANCELLED: &str = "cancelled";

/// The preflight in flight, if any (#235). Preflight is not a durable job (its
/// only output is the findings it persists on success), so cancellation is an
/// in-memory flag, not a job row. Starting a preflight cancels the previous
/// one, so an abandoned scan never keeps a core busy for its full duration.
///
/// Two locks: `current` is held only for a flag swap, so `begin` and the
/// synchronous `cancel_preflight` command never wait on the database
/// (#434 review); `writes` serializes register writes.
#[derive(Default)]
struct PreflightRuns {
    current: Mutex<RunsState>,
    writes: Mutex<()>,
}

#[derive(Default)]
struct RunsState {
    run: Option<Arc<RunState>>,
    /// Orders preflight starts against recovery reconciliations.
    epoch: u64,
    /// Each repo's latest recovery reconciliation (#439 review): a preflight
    /// begun before it holds only pending findings and must not overwrite the
    /// proven classification; it is answered with the reconciled report the
    /// register holds instead (#458).
    reconciled: std::collections::BTreeMap<String, Reconciled>,
    /// The epoch of each repo's latest persisted preflight (#497): a
    /// reconciliation whose fence is older must not overwrite it.
    persisted: std::collections::BTreeMap<String, u64>,
}

/// One preflight's lifecycle (#493 review). A run leaves `RUNNING` exactly
/// once, by compare-and-swap: to `CANCELLED` when the user cancels or a newer
/// preflight supersedes it, or to `COMMITTING` immediately before its register
/// write commits. Whichever lands first wins, so a commit never follows a
/// successful cancel, and a cancel that finds the run committing lost the
/// race — the run completes and reports its result. Neither side waits.
#[derive(Default)]
struct RunState(std::sync::atomic::AtomicU8);

impl RunState {
    const RUNNING: u8 = 0;
    const CANCELLED: u8 = 1;
    const COMMITTING: u8 = 2;

    fn leave_running(&self, to: u8) -> bool {
        self.0
            .compare_exchange(Self::RUNNING, to, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Cancel the run unless it already began committing; `false` means the
    /// cancel lost (or the run had already been cancelled).
    fn cancel(&self) -> bool {
        self.leave_running(Self::CANCELLED)
    }

    /// Claim the commit unless the run was cancelled first.
    fn claim_commit(&self) -> bool {
        self.leave_running(Self::COMMITTING)
    }

    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst) == Self::CANCELLED
    }
}

/// One registered preflight: cancelled when the user cancels or a newer
/// preflight supersedes it, unless its register write already began
/// committing.
struct PreflightToken {
    state: Arc<RunState>,
    epoch: u64,
}

/// A recovery's reconciled report and its epoch in the register order.
#[derive(Debug)]
struct Reconciled {
    epoch: u64,
    report: Arc<ingest::preflight::PreflightReport>,
}

/// What a preflight's register write amounted to.
#[derive(Debug)]
enum Persisted<T> {
    /// This run's findings are in the register.
    Written(T),
    /// A recovery reconciled the repo after this run began, so the register
    /// keeps that newer classification (#458).
    Reconciled {
        epoch: u64,
        report: Arc<ingest::preflight::PreflightReport>,
    },
}

/// A report's place in its repo's register order (#458). The register holds
/// the write with the highest epoch, so a surface that keeps, per repo, the
/// report with the highest epoch it has seen always matches the register.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RegisterStamp {
    repo: String,
    epoch: u64,
}

/// The `preflight` command's result: the report the register now holds for
/// the scanned repo, stamped with its place in the register order.
#[derive(Debug, Serialize)]
struct PreflightResult {
    #[serde(flatten)]
    report: ingest::preflight::PreflightReport,
    register: RegisterStamp,
}

impl std::ops::Deref for PreflightResult {
    type Target = ingest::preflight::PreflightReport;

    fn deref(&self) -> &Self::Target {
        &self.report
    }
}

/// A recovery's place in the preflight order, reserved before the recovery
/// publishes `done` (#497): once the UI can see the job finished, any
/// preflight the user starts is newer than the reconciliation it precedes.
struct ReconcileFence {
    epoch: u64,
}

impl PreflightToken {
    fn is_cancelled(&self) -> bool {
        self.state.is_cancelled()
    }
}

impl PreflightRuns {
    fn current(&self) -> Result<std::sync::MutexGuard<'_, RunsState>, String> {
        self.current.lock().map_err(|e| e.to_string())
    }

    fn begin(&self) -> Result<PreflightToken, String> {
        let state = Arc::new(RunState::default());
        let mut current = self.current()?;
        current.epoch += 1;
        if let Some(previous) = current.run.replace(state.clone()) {
            previous.cancel();
        }
        Ok(PreflightToken {
            state,
            epoch: current.epoch,
        })
    }

    /// Cancel the run in flight, if any. Returns whether a running scan was
    /// stopped: `false` when there is none, or when its register write had
    /// already begun committing — that run completes (#493 review).
    fn cancel(&self) -> Result<bool, String> {
        Ok(self.current()?.run.as_ref().is_some_and(|run| run.cancel()))
    }

    /// Run `persist` only if `token` is still live and no recovery has
    /// reconciled `repo` since the token began, holding the write lock
    /// across the check and the write, so register writes never interleave.
    /// A superseding preflight's write therefore always lands after an older
    /// one it raced (#434 review). A cancel never waits here: one that lands
    /// before the check writes nothing and returns `cancelled`. `persist` is
    /// handed the commit claim to take inside its transaction immediately
    /// before committing (#493): a cancel that landed during the write wins
    /// the claim and rolls the write back, and one that arrives once the
    /// claim is taken loses — the scan completes and reports its result. A run
    /// that a recovery reconciled past writes nothing and is answered with
    /// the reconciled report the register keeps (#458).
    fn persist_if_current<T>(
        &self,
        token: &PreflightToken,
        repo: &str,
        persist: impl FnOnce(&dyn Fn() -> bool) -> Result<T, String>,
    ) -> Result<Persisted<T>, String> {
        let _writes = self.writes.lock().map_err(|e| e.to_string())?;
        if token.is_cancelled() {
            return Err(PREFLIGHT_CANCELLED.into());
        }
        if let Some(reconciled) = self
            .current()?
            .reconciled
            .get(repo)
            .filter(|reconciled| reconciled.epoch > token.epoch)
        {
            return Ok(Persisted::Reconciled {
                epoch: reconciled.epoch,
                report: reconciled.report.clone(),
            });
        }
        let written = persist(&|| token.state.claim_commit())?;
        let mut current = self.current()?;
        let latest = current.persisted.entry(repo.to_string()).or_default();
        *latest = (*latest).max(token.epoch);
        Ok(Persisted::Written(written))
    }

    /// Reserve a recovery's reconciliation epoch. Take it before the job is
    /// published as completed, and commit the write through `reconcile` after
    /// (#497): the cancel race still settles first, while a preflight started
    /// once the job shows `done` is always newer than the reconciliation.
    fn reserve_reconcile(&self) -> Result<ReconcileFence, String> {
        let mut current = self.current()?;
        current.epoch += 1;
        Ok(ReconcileFence {
            epoch: current.epoch,
        })
    }

    /// Recovery's reconciliation write (AC-0200): serialized with preflight
    /// writes and recorded at `fence`'s epoch, so a preflight begun before
    /// the fence can never replace the proven classification with its pending
    /// one (#439 review). A preflight begun after the fence is newer and wins
    /// either way (#497): if it already persisted, the reconciliation writes
    /// nothing (`None`); if it persists later, it is not refused. A recovery
    /// whose fence is older than another recovery's that already reconciled
    /// the repo writes nothing either. A written reconciliation returns its
    /// register stamp.
    fn reconcile(
        &self,
        fence: &ReconcileFence,
        repo: &str,
        report: &ingest::preflight::PreflightReport,
        persist: impl FnOnce() -> Result<(), String>,
    ) -> Result<Option<RegisterStamp>, String> {
        let _writes = self.writes.lock().map_err(|e| e.to_string())?;
        // A newer write of either kind already holds the register (#514
        // review): an overlapping recovery of the same repo whose fence is
        // newer reconciled first, or a newer preflight persisted.
        let newer_written = {
            let current = self.current()?;
            current
                .persisted
                .get(repo)
                .is_some_and(|epoch| *epoch > fence.epoch)
                || current
                    .reconciled
                    .get(repo)
                    .is_some_and(|reconciled| reconciled.epoch > fence.epoch)
        };
        if newer_written {
            return Ok(None);
        }
        persist()?;
        self.current()?.reconciled.insert(
            repo.to_string(),
            Reconciled {
                epoch: fence.epoch,
                report: Arc::new(report.clone()),
            },
        );
        Ok(Some(RegisterStamp {
            repo: repo.to_string(),
            epoch: fence.epoch,
        }))
    }
}

/// A preflight nothing can cancel — for tests that only need its result.
#[cfg(test)]
fn preflight_uncancelled<R: tauri::Runtime>(
    path: &str,
    app: &tauri::AppHandle<R>,
    state: &AppState,
) -> Result<PreflightResult, String> {
    let runs = PreflightRuns::default();
    let token = runs.begin()?;
    preflight_blocking(path, app, state, &runs, &token, &mut |_| {})
}

/// One live `preflight://progress` ping: the file about to be parsed and how
/// far through the walk the scan is — or, while the tree is still being
/// listed (`total` is `null`), the directory being entered and how many files
/// have been found so far (#453).
#[derive(Clone, Serialize)]
struct PreflightProgress<'a> {
    /// The UI-issued id of the scan this ping belongs to.
    run: u64,
    path: &'a str,
    done: usize,
    total: Option<usize>,
}

/// Emits at most one `preflight://progress` event every ~120ms (the same
/// budget as `job://detail`, #209) so thousands of files can't flood the IPC
/// bridge. The first step always fires, and so does the first file step
/// after listing, so the line never sits on a finished listing (#453).
fn preflight_progress_throttle<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    run: u64,
) -> impl FnMut(ingest::preflight::ScanStep<'_>) + '_ {
    let mut last: Option<std::time::Instant> = None;
    let mut listing = true;
    move |step| {
        let now = std::time::Instant::now();
        let listing_ended = listing && step.total.is_some();
        listing = step.total.is_none();
        if listing_ended
            || last.is_none_or(|t| now.duration_since(t) >= std::time::Duration::from_millis(120))
        {
            let _ = app.emit(
                "preflight://progress",
                PreflightProgress {
                    run,
                    path: step.path,
                    done: step.done,
                    total: step.total,
                },
            );
            last = Some(now);
        }
    }
}

/// The preflight pipeline behind the command, generic over the runtime so
/// the request → gate → accept → re-scan lane is testable end to end. The
/// scanned directory registers as a resolved ingest root (it *is* one — a
/// canonicalized directory), so its `.cartograph/adapters/` is discoverable
/// before the first ingest; gated+enabled plugins then count as coverage
/// and their uncovered-language findings close on the re-scan (#201).
fn preflight_blocking<R: tauri::Runtime>(
    path: &str,
    app: &tauri::AppHandle<R>,
    state: &AppState,
    runs: &PreflightRuns,
    token: &PreflightToken,
    on_progress: &mut dyn FnMut(ingest::preflight::ScanStep<'_>),
) -> Result<PreflightResult, String> {
    let stopped = || token.is_cancelled();
    let source = register_local_source(state, std::path::Path::new(path))?;
    let operation = SourceOperation::acquire(&state.sources, [(source.clone(), false)])?;
    let root = operation.root(&source.repo_key)?;
    let repo = source.repo_key.clone();
    let plugins = plugin_coverage(&active_plugins_for_root(app, state, root)?);
    // No TS parse here (#243): the eval-site AST proof is a full extraction,
    // and recovery performs exactly that parse moments later. Inline-eval
    // lines are reported pending that proof, and recovery reconciles them
    // (`reconciled_preflight_report`) — never claimed closed before then.
    let report = ingest::preflight::preflight_scan(
        root,
        &plugins,
        ingest::preflight::EvalProofSource::PendingRecovery,
        &mut |step| {
            if stopped() {
                return std::ops::ControlFlow::Break(());
            }
            on_progress(step);
            std::ops::ControlFlow::Continue(())
        },
    )
    .map_err(|e| e.to_string())?
    .ok_or(PREFLIGHT_CANCELLED)?;
    // A stopped scan must not replace the register with a result the user
    // abandoned.
    let persisted = runs.persist_if_current(token, &repo, |live| {
        persist_preflight_findings_if(state, &repo, &report, live)
    })?;
    Ok(match persisted {
        Persisted::Written(()) => PreflightResult {
            report,
            register: RegisterStamp {
                repo,
                epoch: token.epoch,
            },
        },
        Persisted::Reconciled { epoch, report } => PreflightResult {
            report: (*report).clone(),
            register: RegisterStamp { repo, epoch },
        },
    })
}

fn plugin_coverage(plugins: &[ActivePlugin]) -> Vec<ingest::preflight::PluginCoverage> {
    plugins
        .iter()
        .map(|plugin| ingest::preflight::PluginCoverage {
            plugin_id: plugin.plugin_id.clone(),
            extensions: plugin.extensions.clone(),
        })
        .collect()
}

/// The register holds one preflight result per repo: each scan replaces the
/// previous one, so every surface quotes the latest classification.
fn persist_preflight_findings(
    state: &AppState,
    repo: &str,
    report: &ingest::preflight::PreflightReport,
) -> Result<(), String> {
    persist_preflight_findings_if(state, repo, report, &|| true)
}

/// `persist_preflight_findings`, rolled back as `cancelled` unless
/// `claim_commit` succeeds immediately before the commit, so a preflight
/// cancelled mid-write leaves the register as it was (AC-0215).
fn persist_preflight_findings_if(
    state: &AppState,
    repo: &str,
    report: &ingest::preflight::PreflightReport,
    claim_commit: &dyn Fn() -> bool,
) -> Result<(), String> {
    let batch: Vec<NewFinding<'_>> = report
        .unsupported
        .iter()
        .map(|finding| NewFinding {
            kind: "unsupported",
            detector: &finding.detector,
            path: &finding.path,
            line: finding.line as i64,
            message: &finding.message,
        })
        .collect();
    let mut findings = state.findings.lock().map_err(|e| e.to_string())?;
    findings
        .replace_for_if(repo, ingest::preflight::DETECTOR_ID, &batch, claim_commit)
        .map_err(|e| e.to_string())?
        .map(drop)
        .ok_or_else(|| PREFLIGHT_CANCELLED.to_string())
}

/// Recovery's half of the inline-eval classification (AC-0099, AC-0200):
/// the extraction that just ran is the TS adapter's AST proof, so its eval
/// sites become the claims and the cheap textual scan re-runs with them.
/// Proven literals close, const-shaped-but-unproven sites downgrade to
/// potential Gaps, and the rest stay Unsupported.
///
/// The scan reads each captured file from `capture`, the exact bytes the
/// claims were proven on, so an edit made after capture cannot pair a claim
/// with different code (#439 review). It writes nothing: every recovery
/// replaces preflight's pending register findings with this report only once
/// its job has settled as completed, so a cancel that wins the race writes
/// nothing, and through `PreflightRuns::reconcile`, so a preflight already in
/// flight cannot overwrite it afterwards (AC-0200, AC-0209).
fn reconciled_preflight_report(
    root: &std::path::Path,
    capture: Option<&source_capture::Capture>,
    plugins: &[ingest::preflight::PluginCoverage],
    eval_sites: &[adapters_lang_ts::EvalSite],
) -> Result<ingest::preflight::PreflightReport, String> {
    let claims: Vec<ingest::preflight::EvalSiteCoverage> = eval_sites
        .iter()
        .map(|site| ingest::preflight::EvalSiteCoverage {
            path: site.path.clone(),
            line: site.line,
            proof: match site.proof {
                adapters_lang_ts::EvalProof::Covered => ingest::preflight::EvalProof::Covered,
                adapters_lang_ts::EvalProof::ConstUnproven => {
                    ingest::preflight::EvalProof::ConstUnproven
                }
                adapters_lang_ts::EvalProof::Dynamic => ingest::preflight::EvalProof::Dynamic,
            },
        })
        .collect();
    let members: Vec<&str> = capture
        .map(|capture| {
            capture
                .manifest()
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect()
        })
        .unwrap_or_default();
    let captured = |rel: &str| {
        capture
            .and_then(|capture| capture.file(rel).ok())
            .map(|file| file.bytes())
    };
    let eval = match capture {
        Some(_) => ingest::preflight::EvalProofSource::Captured {
            paths: &members,
            sites: &claims,
            bytes: &captured,
        },
        // Uncaptured extraction read the live tree itself.
        None => ingest::preflight::EvalProofSource::Claims(&claims),
    };
    let report = ingest::preflight::preflight_scan(root, plugins, eval, &mut |_| {
        std::ops::ControlFlow::Continue(())
    })
    .map_err(|e| e.to_string())?
    .ok_or("the reconciliation scan never stops early")?;
    Ok(report)
}

/// All configurable tier settings (T0 is always on and absent by design).
#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> Result<Vec<settings::TierSettings>, String> {
    let store = state.settings.lock().map_err(|e| e.to_string())?;
    store.all().map_err(|e| e.to_string())
}

/// Enable/disable a configurable tier (#118).
#[tauri::command]
fn set_tier_enabled(
    tier: String,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<Vec<settings::TierSettings>, String> {
    let mut store = state.settings.lock().map_err(|e| e.to_string())?;
    store
        .set_enabled(&tier, enabled)
        .map_err(|e| e.to_string())?;
    store.all().map_err(|e| e.to_string())
}

/// Choose local or cloud for an LLM tier; leaving cloud revokes consent.
#[tauri::command]
fn set_tier_provider(
    tier: String,
    provider: String,
    state: State<'_, AppState>,
) -> Result<Vec<settings::TierSettings>, String> {
    let mut store = state.settings.lock().map_err(|e| e.to_string())?;
    store
        .set_provider(&tier, &provider)
        .map_err(|e| e.to_string())?;
    store.all().map_err(|e| e.to_string())
}

/// Record standing cloud consent for a tier, storing the disclosure the
/// user saw. Only permits cloud — every call still needs the firewall's
/// per-payload grant (fail closed).
#[tauri::command]
fn grant_cloud_consent(
    tier: String,
    disclosure: String,
    state: State<'_, AppState>,
) -> Result<Vec<settings::TierSettings>, String> {
    let mut store = state.settings.lock().map_err(|e| e.to_string())?;
    store
        .grant_consent(&tier, &disclosure)
        .map_err(|e| e.to_string())?;
    store.all().map_err(|e| e.to_string())
}

/// Revoke a tier's standing cloud consent — immediate.
#[tauri::command]
fn revoke_cloud_consent(
    tier: String,
    state: State<'_, AppState>,
) -> Result<Vec<settings::TierSettings>, String> {
    let mut store = state.settings.lock().map_err(|e| e.to_string())?;
    store.revoke_consent(&tier).map_err(|e| e.to_string())?;
    store.all().map_err(|e| e.to_string())
}

/// The "Ingest parallelism" setting and what it resolves to here (#236).
#[tauri::command]
fn get_ingest_parallelism(
    state: State<'_, AppState>,
) -> Result<settings::IngestParallelism, String> {
    let store = state.settings.lock().map_err(|e| e.to_string())?;
    store.ingest_parallelism().map_err(|e| e.to_string())
}

/// Persist and apply "Ingest parallelism" (`0` = Auto). Takes effect for
/// the next extraction; recovered facts never depend on it (#236).
#[tauri::command]
fn set_ingest_parallelism(
    setting: u32,
    state: State<'_, AppState>,
) -> Result<settings::IngestParallelism, String> {
    let mut store = state.settings.lock().map_err(|e| e.to_string())?;
    let applied = store
        .set_ingest_parallelism(setting)
        .map_err(|e| e.to_string())?;
    source_walk::parallel::set_parallelism(settings::IngestParallelism::parallelism(
        applied.setting,
    ));
    Ok(applied)
}

/// Live egress summary for the status bar (#103's seam, now real).
#[tauri::command]
fn egress_summary(state: State<'_, AppState>) -> Result<settings::EgressSummary, String> {
    let store = state.settings.lock().map_err(|e| e.to_string())?;
    store.egress_summary().map_err(|e| e.to_string())
}

/// The full consent disclosure for a tier's cloud lane (#112): everything
/// the Settings consent panel must show *before* consent is recordable.
/// T2 semantic triage runs the Haiku lane; T3 agentic reasoning runs Opus
/// (Fable stays a per-escalation opt-in, #120). T0/T1 have no cloud lane.
#[tauri::command]
fn cloud_disclosure(tier: String) -> Result<llm::anthropic::CloudDisclosure, String> {
    let lane = match tier.as_str() {
        "T2" => llm::anthropic::ClaudeLane::Haiku,
        "T3" => llm::anthropic::ClaudeLane::Opus,
        other => return Err(format!("tier '{other}' has no cloud lane")),
    };
    Ok(llm::anthropic::disclosure(lane))
}

/// Compute and persist one ingest's recovery metrics (#119): tier tallies,
/// register counts, extractor coverage, and the whole-graph content hash
/// that makes AC-0039's determinism observable as history data. Reads the
/// same whole-graph projection every other surface reads.
fn record_ingest_metrics(
    state: &AppState,
    job_id: i64,
    record_repo: &str,
    commit_sha: &str,
    layers: &LayerBreakdown,
    coverage_repos: &std::collections::BTreeSet<String>,
) -> Result<(), String> {
    let (nodes, edges) = {
        let graph = state.graph.lock().map_err(|e| e.to_string())?;
        graph.read_snapshot().map_err(|e| e.to_string())?
    };
    // Only layers this ingest actually contained are in scope — a
    // zero-file layer's extractor did not run, so it reports null
    // coverage (not applicable), never a misleading 0%.
    let scope: std::collections::BTreeMap<String, u64> = [
        ("t0.adapter-ts", layers.ts.files),
        ("t0.adapter-python", layers.python.files),
        ("t0.adapter-go", layers.go.files),
        ("t0.adapter-java", layers.java.files),
        ("t0.adapter-kotlin", layers.kotlin.files),
        ("t0.iac-terraform", layers.tf.files),
        ("t0.webextension", layers.webext.files),
    ]
    .into_iter()
    .filter(|(_, files)| *files > 0)
    .map(|(extractor, files)| (extractor.to_string(), files))
    .collect();
    let computed = metrics::compute(&nodes, &edges, &scope, coverage_repos);
    let (unsupported, no_evidence) = {
        let findings = state.findings.lock().map_err(|e| e.to_string())?;
        findings.counts().map_err(|e| e.to_string())?
    };
    let mut store = state.metrics.lock().map_err(|e| e.to_string())?;
    store
        .record(
            job_id,
            record_repo,
            commit_sha,
            &computed,
            unsupported,
            no_evidence,
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Ingest records, newest first (#119): tier tallies, register counts, and
/// the graph content hash per run — evidence health over re-ingests.
#[tauri::command]
fn ingest_history(state: State<'_, AppState>) -> Result<Vec<metrics::IngestRecord>, String> {
    let store = state.metrics.lock().map_err(|e| e.to_string())?;
    store.history(50).map_err(|e| e.to_string())
}

/// Per-extractor coverage for the most recent ingest (#119).
#[tauri::command]
fn extractor_coverage(
    state: State<'_, AppState>,
) -> Result<Vec<metrics::ExtractorCoverage>, String> {
    let store = state.metrics.lock().map_err(|e| e.to_string())?;
    store.latest_coverage().map_err(|e| e.to_string())
}

/// The provider for one escalation mode. Local is the pinned catalog SLM;
/// cloud is the Opus reasoning lane and needs an API key — its absence is
/// an explicit error, never a silent local fallback.
fn escalation_provider(mode: &str) -> Result<Box<dyn LlmProvider>, String> {
    match mode {
        "local" => Ok(Box::new(
            llm::OllamaProvider::local_default().map_err(|e| e.to_string())?,
        )),
        "cloud" => {
            let key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
                "no Anthropic API key configured (set ANTHROPIC_API_KEY) — cloud escalation \
                 stays closed"
                    .to_string()
            })?;
            Ok(Box::new(
                llm::anthropic::AnthropicProvider::new(llm::anthropic::ClaudeLane::Opus, key)
                    .map_err(|e| e.to_string())?,
            ))
        }
        other => Err(format!("unknown escalation mode '{other}' (local | cloud)")),
    }
}

/// Strategy cards for one gap (#120): attempted tiers, stop reason, required
/// evidence, and the local/cloud options with exact egress estimates from
/// the firewall preview. Derivation only — nothing runs, nothing egresses.
#[tauri::command]
async fn gap_strategies(
    gap_id: String,
    app: tauri::AppHandle,
) -> Result<escalation::GapStrategyReport, String> {
    off_ui_thread(move || {
        let state = app.state::<AppState>();
        let task = task_evidence::prepare(&state, &gap_id, "escalate:preview")?;
        // Strategy text is derived from the same graph basis as the prepared
        // task; a concurrent recovery requires a new preview.
        let (nodes, edges) = state
            .graph
            .lock()
            .map_err(|e| e.to_string())?
            .read_snapshot()
            .map_err(|e| e.to_string())?;
        let snapshot =
            context_hub::ContextSnapshot::new(nodes.clone(), edges).map_err(|e| e.to_string())?;
        if snapshot.id() != task.source_basis().graph_snapshot_id {
            return Err("Task evidence changed; refresh the strategy preview.".into());
        }
        let gap = nodes
            .iter()
            .find(|node| node.id == gap_id)
            .ok_or("Selected gap is unavailable; refresh recovery.")?;
        let cloud_allowed = state
            .settings
            .lock()
            .map_err(|e| e.to_string())?
            .egress_policy()
            .map_err(|e| e.to_string())?
            .cloud_allowed(llm::AnalysisTier::Agentic);
        let firewall = llm::EgressFirewall::new(llm::EgressPolicy::default());
        let local = llm::OllamaProvider::local_default().map_err(|e| e.to_string())?;
        let preview = agents::AgentBroker::bounded_default()
            .preview_prepared(&local, &firewall, &task)
            .map_err(|e| e.to_string())?;
        let payload_bytes = serde_json::to_vec(&preview.payload)
            .map_err(|e| e.to_string())?
            .len() as u64;
        let mut report = escalation::strategies(task.task(), gap, cloud_allowed, payload_bytes);
        report.source_basis = Some(task.source_basis().clone());
        Ok(report)
    })
    .await
}

/// Exact redacted disclosure, including the receipt-bound input identity.
#[tauri::command]
async fn escalation_preview(
    gap_id: String,
    app: tauri::AppHandle,
) -> Result<llm::EgressPreview, String> {
    off_ui_thread(move || {
        let state = app.state::<AppState>();
        let task = task_evidence::prepare(&state, &gap_id, &format!("escalate:{gap_id}"))?;
        let policy = state
            .settings
            .lock()
            .map_err(|e| e.to_string())?
            .egress_policy()
            .map_err(|e| e.to_string())?;
        let provider = escalation_provider("cloud")?;
        agents::AgentBroker::bounded_default()
            .preview_prepared(provider.as_ref(), &llm::EgressFirewall::new(policy), &task)
            .map_err(|e| e.to_string())
    })
    .await
}

/// Run one escalation as a durable job (#120): local runs immediately;
/// cloud requires standing consent (settings) AND a per-payload grant whose
/// hash matches this exact preview. The result is a staged proposal —
/// accept/reject goes through record_agent_decision; the graph is never
/// touched (R-INT-1/R-INT-3).
#[tauri::command]
async fn run_escalation(
    gap_id: String,
    mode: String,
    approved_payload_hash: Option<String>,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<agents::StagedProposal, String> {
    let (running, execution) = start_job(&state, &format!("escalate:{gap_id}:{mode}"))?;
    emit_job(&app, &running);
    let fail = |error: String| -> String {
        report_failure(&app, &state, &execution, &error);
        error
    };

    report_progress(&app, &state, &execution, "context", 20.0).map_err(&fail)?;
    let context_app = app.clone();
    let task = off_ui_thread_for_job(execution.clone(), move |execution| {
        let state = context_app.state::<AppState>();
        if job_cancelled(&state, execution)? {
            return Err("cancelled".into());
        }
        task_evidence::prepare(&state, &gap_id, &format!("escalate:{gap_id}"))
    })
    .await
    .map_err(&fail)?;

    report_progress(&app, &state, &execution, "model", 70.0).map_err(&fail)?;
    let policy = (|| {
        let settings_store = state.settings.lock().map_err(|e| e.to_string())?;
        settings_store.egress_policy().map_err(|e| e.to_string())
    })()
    .map_err(&fail)?;
    let provider = escalation_provider(&mode).map_err(&fail)?;
    let firewall = llm::EgressFirewall::new(policy);
    let broker = agents::AgentBroker::bounded_default();
    // A cloud run re-derives the preview and only proceeds when the user's
    // approved hash matches this exact payload (one-action consent).
    let consent = if mode == "cloud" {
        let preview = broker
            .preview_prepared(provider.as_ref(), &firewall, &task)
            .map_err(|e| fail(e.to_string()))?;
        let approved = approved_payload_hash
            .ok_or_else(|| fail("cloud escalation requires an approved payload hash".into()))?;
        if approved != preview.payload_hash {
            return Err(fail(
                "approved payload hash does not match the current payload — re-review the \
                 preview before consenting"
                    .into(),
            ));
        }
        Some(llm::ConsentGrant::from_preview(&preview))
    } else {
        None
    };
    let payload_bytes = if consent.is_some() {
        let preview = broker
            .preview_prepared(provider.as_ref(), &firewall, &task)
            .map_err(|e| fail(e.to_string()))?;
        serde_json::to_vec(&preview.payload)
            .map_err(|e| fail(e.to_string()))?
            .len() as u64
    } else {
        0
    };

    // Cooperative cancellation: a cancel that landed after context assembly
    // must stop the run before any provider is invoked — for cloud, before
    // the consented payload could leave the device.
    if job_cancelled(&state, &execution)? {
        return Err("cancelled".to_string());
    }
    let staging_app = app.clone();
    let proposal = off_ui_thread_for_job(execution.clone(), move |execution| {
        let state = staging_app.state::<AppState>();
        if job_cancelled(&state, execution)? {
            return Err("cancelled".to_string());
        }
        let proposal = broker
            .propose_prepared(provider.as_ref(), &firewall, &task, consent.as_ref())
            .map_err(|error| error.to_string())?;
        // Persist even if cancellation arrived during the model call. The job
        // remains cancelled, but its completed result is available for review.
        stage_completed_prepared_job_proposal(&state, execution, &task, &proposal)
    })
    .await
    .map_err(&fail)?;

    report_progress(&app, &state, &execution, "validate", 90.0).map_err(&fail)?;
    if payload_bytes > 0 {
        (|| {
            let mut settings_store = state.settings.lock().map_err(|e| e.to_string())?;
            settings_store
                .record_egress(&proposal.proposal.provenance.extractor_id, payload_bytes)
                .map_err(|e| e.to_string())
        })()
        .map_err(&fail)?;
    }
    let job = (|| {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        jobs.finish_execution(&execution, std::slice::from_ref(&proposal.proposal_id))
            .map_err(|e| e.to_string())
    })()
    .map_err(&fail)?;
    emit_job(&app, &updated_job(job));
    Ok(proposal)
}

/// Escalate every instance of a gap class as one durable job (#167).
/// Local-tier only: a cloud consent grant binds to one exact payload hash
/// (AC-0063) and is never amortized across a class — cloud stays
/// per-instance. Each instance yields its own staged proposal through the
/// same propose-only path; one bad instance records a failure, never an
/// abort; cancel stops at the next instance boundary.
#[tauri::command]
async fn run_class_escalation(
    gap_ids: Vec<String>,
    mode: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<proposals::StagedBatchOutcome, String> {
    if mode != "local" {
        return Err(
            "class escalation runs local-only: a cloud consent grant binds to one exact \
             payload hash and cannot cover a whole class — escalate cloud per instance"
                .to_string(),
        );
    }
    if gap_ids.is_empty() {
        return Err("no gap instances to escalate".to_string());
    }
    let (running, execution) =
        start_job(&state, &format!("escalate-class:{}:{mode}", gap_ids.len()))?;
    emit_job(&app, &running);
    let fail = |error: String| -> String {
        report_failure(&app, &state, &execution, &error);
        error
    };

    report_progress(&app, &state, &execution, "context", 5.0).map_err(&fail)?;
    // Source text is prepared lazily per instance inside the worker loop.
    // The class carries gap identities only, not an unbounded union of evidence.
    let tasks: Vec<(String, Result<String, String>)> = gap_ids
        .into_iter()
        .map(|gap_id| (gap_id.clone(), Ok(gap_id)))
        .collect();

    let policy = (|| {
        let settings_store = state.settings.lock().map_err(|e| e.to_string())?;
        settings_store.egress_policy().map_err(|e| e.to_string())
    })()
    .map_err(&fail)?;
    let provider = escalation_provider(&mode).map_err(&fail)?;
    let firewall = llm::EgressFirewall::new(policy);
    let broker = agents::AgentBroker::bounded_default();

    let batch_app = app.clone();
    let mut outcome = off_ui_thread_for_job(execution.clone(), move |execution| {
        let state = batch_app.state::<AppState>();
        let total = tasks.len();
        run_job_staged_batch(
            &state,
            execution,
            tasks,
            |index, _| {
                report_progress(
                    &batch_app,
                    &state,
                    execution,
                    &format!("escalate {}/{total}", index + 1),
                    5.0 + (index as f64 / total as f64) * 90.0,
                )
            },
            |gap_id| {
                let task = task_evidence::prepare(&state, gap_id, &format!("escalate:{gap_id}"))?;
                if job_cancelled(&state, execution)? {
                    return Err("cancelled".into());
                }
                let proposal = broker
                    .propose_prepared(provider.as_ref(), &firewall, &task, None)
                    .map_err(|error| error.to_string())?;
                stage_completed_prepared_job_proposal(&state, execution, &task, &proposal)
            },
        )
    })
    .await
    .map_err(&fail)?;

    let job = (|| {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        if outcome.proposals.is_empty() && !outcome.failures.is_empty() {
            jobs.fail_execution(
                &execution,
                "No instance produced a durable proposal; inspect the batch failures.",
            )
        } else {
            let mut artifacts: Vec<String> = outcome
                .proposals
                .iter()
                .map(|proposal| proposal.proposal_id.clone())
                .collect();
            artifacts.push(format!("failures:{}", outcome.failures.len()));
            jobs.finish_execution(&execution, &artifacts)
        }
        .map_err(|e| e.to_string())
    })()
    .map_err(&fail)?;
    let job = updated_job(job);
    outcome.cancelled |= job.status == "cancelled";
    emit_job(&app, &job);
    // A cancelled batch still returns its partial outcome (#194 review):
    // completed instances are real staged proposals the user can triage,
    // and `cancelled: true` tells the register the run stopped early.
    Ok(outcome)
}

/// Notify the shell of a job transition (`job://changed`); the Jobs surface
/// and the global progress bar stay live without polling (#117).
fn emit_job<R: tauri::Runtime>(app: &tauri::AppHandle<R>, job: &Job) {
    let _ = app.emit("job://changed", job);
}

fn claim_job(state: &AppState, plan: &jobs::ClaimPlan) -> Result<(Job, JobExecution), String> {
    // The plan is owned data: no store mutex spans the OS reservation.
    let reservation = state
        .job_execution_locks
        .try_reserve(plan.lock_target())
        .map_err(|error| error.to_string())?;
    state
        .jobs
        .lock()
        .map_err(|error| error.to_string())?
        .claim_execution(plan, reservation)
        .map_err(|error| error.to_string())
}

fn start_job(state: &AppState, kind: &str) -> Result<(Job, JobExecution), String> {
    let plan = {
        let mut jobs = state.jobs.lock().map_err(|error| error.to_string())?;
        let job = jobs.enqueue(kind).map_err(|error| error.to_string())?;
        jobs.claim_plan(job.id, ClaimMode::StartQueued)
            .map_err(|error| error.to_string())?
    };
    claim_job(state, &plan)
}

fn updated_job(update: ExecutionUpdate) -> Job {
    match update {
        ExecutionUpdate::Applied(job) | ExecutionUpdate::Unchanged(job) => job,
    }
}

fn completed_job(job: &Job) -> Result<(), String> {
    match job.status.as_str() {
        "done" => Ok(()),
        "cancelled" => Err("cancelled".into()),
        _ => Err("Job execution has already finished without completing this work.".into()),
    }
}

fn check_completed_execution(state: &AppState, execution: &JobExecution) -> Result<(), String> {
    // A completed model result remains reviewable even if the cancelled job was
    // cleared during that call. The retained opaque execution supplies its
    // original identity; this exception never authorizes another model call or
    // a worker lifecycle write through a missing row.
    match state
        .jobs
        .lock()
        .map_err(|error| error.to_string())?
        .check_execution(execution)
    {
        Ok(ExecutionCheck::Running | ExecutionCheck::Cancelled)
        | Err(JobTransitionError::Missing) => {}
        Ok(ExecutionCheck::Terminal) => return Err("Job execution has already finished.".into()),
        Err(error) => return Err(error.to_string()),
    }
    Ok(())
}

fn stage_completed_prepared_job_proposal(
    state: &AppState,
    execution: &JobExecution,
    task: &agents::PreparedAgentTask,
    proposal: &agents::AgentProposal,
) -> Result<agents::StagedProposal, String> {
    check_completed_execution(state, execution)?;
    state
        .proposals
        .lock()
        .map_err(|error| error.to_string())?
        .stage_prepared(
            task,
            proposal,
            execution.id(),
            &task.source_basis().graph_snapshot_id,
        )
        .map_err(|error| error.to_string())
}

#[cfg(test)]
fn stage_completed_job_proposal(
    state: &AppState,
    execution: &JobExecution,
    task: &agents::AgentTask,
    proposal: &agents::AgentProposal,
    graph_snapshot_id: &str,
) -> Result<agents::StagedProposal, String> {
    check_completed_execution(state, execution)?;
    state
        .proposals
        .lock()
        .map_err(|error| error.to_string())?
        .stage(task, proposal, execution.id(), graph_snapshot_id)
        .map_err(|error| error.to_string())
}

fn run_job_staged_batch<T>(
    state: &AppState,
    execution: &JobExecution,
    tasks: Vec<(String, Result<T, String>)>,
    mut progress: impl FnMut(usize, usize) -> Result<(), String>,
    mut execute_and_stage: impl FnMut(&T) -> Result<agents::StagedProposal, String>,
) -> Result<proposals::StagedBatchOutcome, String> {
    // The broker's bool cancellation callback must stop on ownership errors,
    // while the host preserves their distinct cause in the returned failure.
    let stop_error = std::cell::RefCell::new(None);
    let outcome = proposals::run_staged_batch(
        tasks,
        || match job_cancelled(state, execution) {
            Ok(cancelled) => cancelled || stop_error.borrow().is_some(),
            Err(error) => {
                *stop_error.borrow_mut() = Some(error);
                true
            }
        },
        |index, total| {
            if let Err(error) = progress(index, total) {
                *stop_error.borrow_mut() = Some(error);
            }
        },
        |task| {
            if let Some(error) = stop_error.borrow().as_ref() {
                return Err(error.clone());
            }
            match job_cancelled(state, execution) {
                Ok(false) => execute_and_stage(task),
                Ok(true) => Err("cancelled".into()),
                Err(error) => {
                    *stop_error.borrow_mut() = Some(error.clone());
                    Err(error)
                }
            }
        },
    );
    if let Some(error) = stop_error.into_inner() {
        return Err(error);
    }
    Ok(outcome)
}

fn recover_jobs(jobs: &Mutex<JobStore>, locks: &JobExecutionLocks) -> Result<Vec<Job>, String> {
    let candidates = jobs
        .lock()
        .map_err(|error| error.to_string())?
        .recovery_candidates()
        .map_err(|error| error.to_string())?;
    let mut recovered = Vec::new();
    for candidate in candidates {
        let reservation = match locks.try_reserve(candidate.lock_target()) {
            Ok(reservation) => reservation,
            Err(JobTransitionError::Busy) => continue,
            Err(error) => return Err(error.to_string()),
        };
        if let Some(job) = jobs
            .lock()
            .map_err(|error| error.to_string())?
            .recover_reserved(&candidate, reservation)
            .map_err(|error| error.to_string())?
        {
            recovered.push(job);
        }
    }
    Ok(recovered)
}

/// A live "what it's doing right now" ping (#209) — current adapter/file
/// being read. Deliberately not part of the durable `Job` row: it's
/// best-effort and streamed far more often than a SQLite write per file
/// would tolerate, so it rides its own event instead of `job://changed`.
#[derive(Clone, Serialize)]
struct JobDetail<'a> {
    id: i64,
    detail: &'a str,
}

/// The `job://detail` a local recovery reports while it reconciles its
/// preflight findings with the eval proof it just published (AC-0200).
const RECONCILE_PREFLIGHT_DETAIL: &str = "Reconciling preflight findings";

fn emit_detail<R: tauri::Runtime>(app: &tauri::AppHandle<R>, job_id: i64, detail: &str) {
    let _ = app.emit("job://detail", JobDetail { id: job_id, detail });
}

/// A throttled sink for `extract_tree_incremental`'s per-file detail pings:
/// at most one `job://detail` event every ~120ms, so a repo with thousands
/// of files can't flood the Tauri IPC bridge. The first call always fires.
fn detail_throttle<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    job_id: i64,
) -> impl FnMut(&str) + '_ {
    let mut last: Option<std::time::Instant> = None;
    move |detail: &str| {
        let now = std::time::Instant::now();
        let due =
            last.is_none_or(|t| now.duration_since(t) >= std::time::Duration::from_millis(120));
        if due {
            emit_detail(app, job_id, detail);
            last = Some(now);
        }
    }
}

/// Record stage + percent for a running job and notify the shell.
fn report_progress<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    execution: &JobExecution,
    stage: &str,
    percent: f64,
) -> Result<(), String> {
    let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    let job = updated_job(
        jobs.progress_execution(execution, stage, percent)
            .map_err(|e| e.to_string())?,
    );
    emit_job(app, &job);
    Ok(())
}

/// Persist a job failure with its detail and notify the shell.
fn report_failure<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    execution: &JobExecution,
    error: &str,
) {
    let Ok(mut jobs) = state.jobs.lock() else {
        return;
    };
    if let Ok(update) = jobs.fail_execution(execution, error) {
        emit_job(app, &updated_job(update));
    }
}

fn finish_source_operation<R: tauri::Runtime>(
    state: &AppState,
    app: &tauri::AppHandle<R>,
    execution: &JobExecution,
    operation: &mut SourceOperation,
) -> Result<(), String> {
    if job_cancelled(state, execution)? {
        return Err("cancelled".into());
    }
    // Settle the cancellation race before publishing readiness. A failed final
    // availability transaction leaves all managed sources unavailable, even if
    // the historical job has already recorded its completed recovery work.
    let job = {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        updated_job(
            jobs.finish_execution(execution, &[])
                .map_err(|e| e.to_string())?,
        )
    };
    // Readiness and the released reservations come before `done` is
    // announced (#497 review): a preflight the user starts once they see it
    // must be able to read the recovered source.
    completed_job(&job)?;
    let ready = operation.set_writes_ready(&state.sources, true);
    operation.release();
    emit_job(app, &job);
    ready
}

fn job_cancelled(state: &AppState, execution: &JobExecution) -> Result<bool, String> {
    match state
        .jobs
        .lock()
        .map_err(|error| error.to_string())?
        .check_execution(execution)
        .map_err(|error| error.to_string())?
    {
        ExecutionCheck::Running => Ok(false),
        ExecutionCheck::Cancelled => Ok(true),
        ExecutionCheck::Terminal => Err("Job execution has already finished.".into()),
    }
}

fn ensure_running_job(state: &AppState, execution: &JobExecution) -> Result<(), String> {
    if job_cancelled(state, execution)? {
        Err("cancelled".into())
    } else {
        Ok(())
    }
}

fn register_local_source(
    state: &AppState,
    path: &std::path::Path,
) -> Result<RegisteredSource, String> {
    let root = paths::canonicalize(path).map_err(|e| e.to_string())?;
    let mut registry = state.sources.lock().map_err(|e| e.to_string())?;
    if let Some(source) = registry.get_by_root(&root)? {
        return Ok(source);
    }
    registry.register_local(&root)
}

fn source_operation(
    state: &AppState,
    mut requested: Vec<(RegisteredSource, bool)>,
) -> Result<SourceOperation, String> {
    let nodes = state
        .graph
        .lock()
        .map_err(|e| e.to_string())?
        .read_snapshot()
        .map_err(|e| e.to_string())?
        .0;
    {
        let registry = state.sources.lock().map_err(|e| e.to_string())?;
        for node in nodes.iter().filter(|node| node.label == "Repo") {
            let repo = node
                .id
                .strip_prefix("repo:")
                .ok_or("invalid repository fact identity")?;
            let source = registry
                .get_by_repo(repo)?
                .ok_or("registered ADR source unavailable; reconnect source")?;
            requested.push((source, false));
        }
    }
    SourceOperation::acquire(&state.sources, requested)
}

/// The staged ingest pipeline behind `ingest_path` and `retry_job`: extract →
/// load → stitch, with progress events and cooperative cancellation.
fn run_ingest<R: tauri::Runtime>(
    source: &RegisteredSource,
    operation: &SourceOperation,
    execution: &JobExecution,
    app: &tauri::AppHandle<R>,
    state: &AppState,
) -> Result<IngestSummary, String> {
    let job_id = execution.id();
    let fail = |error: String| -> String {
        report_failure(app, state, execution, &error);
        error
    };
    let cancelled = || -> Result<(), String> {
        if job_cancelled(state, execution)? {
            // Cancelled by the user: status is already `cancelled`; the
            // pipeline just stops. Not a failure.
            return Err("cancelled".to_string());
        }
        Ok(())
    };

    report_progress(app, state, execution, "scan", 5.0)?;
    let root = operation.root(&source.repo_key).map_err(&fail)?;
    let repo = source.repo_key.clone();

    cancelled()?;
    report_progress(app, state, execution, "extract", 15.0)?;
    let active_plugins = active_plugins_for_root(app, state, root).map_err(&fail)?;
    let mut on_file = detail_throttle(app, job_id);
    let primary = state
        .primary_sources
        .prepare(source, root, &[])
        .map_err(&fail)?;
    let mut receipts = Vec::new();
    let (extraction, layers, delta) = {
        let mut caches = state
            .extraction_caches
            .lock()
            .map_err(|e| fail(e.to_string()))?;
        ensure_running_job(state, execution)?;
        let cache = caches.repos.entry(repo.clone()).or_default();
        extract_tree_with_primary(
            root,
            &repo,
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
            cache,
            &active_plugins,
            &mut on_file,
            primary.capture.as_ref(),
            &mut receipts,
        )
        .map_err(fail)?
    };

    cancelled()?;
    state
        .primary_sources
        .persist(&primary, source, &receipts)
        .map_err(&fail)?;
    let bindings = primary_source::matching_bindings(&extraction, &receipts);
    report_progress(app, state, execution, "load", 70.0)?;
    let mut published = OperationFacts::default();
    {
        let mut graph = state.graph.lock().map_err(|e| fail(e.to_string()))?;
        ensure_running_job(state, execution)?;
        published.record_load(
            load_into_graph_with_bindings(
                &mut graph,
                &extraction,
                &repo,
                root,
                "workdir",
                &bindings,
            )
            .map_err(&fail)?
            .published,
        );
        report_progress(app, state, execution, "stitch", 90.0)?;

        ensure_running_job(state, execution)?;
        stitch_backings(&mut graph, &mut published).map_err(&fail)?;
    }

    relink_found_adrs(state, operation, execution, &mut published).map_err(&fail)?;

    ensure_running_job(state, execution)?;
    record_ingest_metrics(
        state,
        job_id,
        &repo,
        "workdir",
        &layers,
        &std::collections::BTreeSet::from([repo.clone()]),
    )
    .map_err(&fail)?;
    // Only once the graph holds the recovered facts (#439 review): a
    // recovery that fails earlier leaves preflight's pending findings in
    // place instead of closing sites whose facts were never published.
    emit_detail(app, job_id, RECONCILE_PREFLIGHT_DETAIL);
    let preflight = reconciled_preflight_report(
        root,
        primary.capture.as_ref(),
        &plugin_coverage(&active_plugins),
        &extraction.eval_sites,
    )
    .map_err(&fail)?;

    // Ordered before `done` is published (#497), so a preflight the user
    // starts once they see it is newer than this reconciliation.
    let fence = app.state::<PreflightRuns>().reserve_reconcile()?;
    // A cancel can land at any point after the last check; `finish` is
    // guarded to only transition a running job, so whichever outcome hit
    // the store first wins — read the row back to learn which.
    {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        let job = updated_job(
            jobs.finish_execution(execution, &[format!("graph:{repo}@workdir")])
                .map_err(|e| e.to_string())?,
        );
        emit_job(app, &job);
        completed_job(&job)?;
    }
    // Written only once the job has settled as completed, so a cancel that
    // wins the race writes nothing (AC-0200, #489); through the per-repo
    // fence, as GitHub and manifest recoveries do (AC-0209).
    let preflight_register =
        app.state::<PreflightRuns>()
            .reconcile(&fence, &repo, &preflight, || {
                persist_preflight_findings(state, &repo, &preflight)
            })?;
    Ok(IngestSummary {
        job_id,
        files: layers.files(),
        nodes: published.nodes(),
        edges: published.edges(),
        merged: published.merged,
        layers,
        delta,
        preflight,
        preflight_register,
    })
}

/// Run T0 extraction over a local directory and load the facts into the
/// graph (US-0002 local path; GitHub clone ingest is `add_repo`).
/// Run `work` on a blocking worker thread. Recovery pipelines must never
/// execute on the invoking thread: synchronous Tauri commands run on the
/// main thread, so an inline ingest freezes the whole app for its duration
/// (AC-0078, #158 — the macOS beachball on large repos).
async fn off_ui_thread<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| e.to_string())?
}

/// The blocking worker owns an execution clone independently of its async
/// waiter. Aborting the waiter cannot release a worker that is still running.
async fn off_ui_thread_for_job<T: Send + 'static>(
    execution: JobExecution,
    work: impl FnOnce(&JobExecution) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    off_ui_thread(move || work(&execution)).await
}

#[tauri::command]
async fn ingest_path(path: String, app: tauri::AppHandle) -> Result<IngestSummary, String> {
    off_ui_thread(move || ingest_path_blocking(path, app)).await
}

fn ingest_path_blocking(path: String, app: tauri::AppHandle) -> Result<IngestSummary, String> {
    let state = app.state::<AppState>();
    let source = register_local_source(&state, std::path::Path::new(&path))?;
    let operation = source_operation(&state, vec![(source.clone(), false)])?;
    let (running, execution) = start_job(&state, &source.ingest_job_kind())?;
    emit_job(&app, &running);
    run_ingest(&source, &operation, &execution, &app, &state)
}

/// Cancel a queued or running job; running work stops at its next stage
/// boundary (#117).
#[tauri::command]
fn cancel_job(id: i64, app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Job, String> {
    let job = state
        .jobs
        .lock()
        .map_err(|e| e.to_string())?
        .cancel(id)
        .map_err(|e| e.to_string())?;
    if let Some(investigation_id) = &job.investigation_id {
        state.investigations.wake(investigation_id);
        let detail = state
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .investigation(investigation_id)
            .map_err(|e| e.to_string())?;
        investigations::emit_changed(&app, &detail);
    }
    emit_job(&app, &job);
    Ok(job)
}

/// Retry a failed or cancelled job, or resume an interrupted one: claim a new
/// running attempt on the same row for kinds the shell can re-run
/// (`ingest-source-v1:*` reuses the content-addressed cache, so a resume recomputes
/// only what the interrupted run didn't finish — ADR-0014).
#[tauri::command]
async fn retry_job(id: i64, app: tauri::AppHandle) -> Result<RetriedJob, String> {
    off_ui_thread(move || retry_job_blocking(id, app)).await
}

/// A retried job's row, plus — for a retried recovery — its reconciled
/// preflight report, so the Preflight surface receives it exactly as it does
/// from the recovery command itself (#458).
#[derive(Serialize)]
struct RetriedJob {
    #[serde(flatten)]
    job: Job,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery: Option<RetriedRecovery>,
}

#[derive(Serialize)]
struct RetriedRecovery {
    preflight: ingest::preflight::PreflightReport,
    preflight_register: Option<RegisterStamp>,
}

impl From<Job> for RetriedJob {
    fn from(job: Job) -> Self {
        Self {
            job,
            recovery: None,
        }
    }
}

fn retry_source(state: &AppState, kind: &str) -> Result<Option<RegisteredSource>, String> {
    if kind.starts_with("ingest:") || kind.starts_with("ingest-source-v1:") {
        let id = sources::source_id_from_ingest_job_kind(kind)?;
        let source = state
            .sources
            .lock()
            .map_err(|e| e.to_string())?
            .get_by_id(id)?
            .ok_or("registered source unavailable; re-run ingestion")?;
        if !source.is_ready() {
            return Err("registered source unavailable; reconnect source".into());
        }
        return Ok(Some(source));
    }
    if kind == "noop" || kind.starts_with("plugin-gate:") {
        return Ok(None);
    }
    Err("retry is not supported for this job; re-run the add".into())
}

fn prepare_job_retry(
    state: &AppState,
    id: i64,
) -> Result<
    (
        Job,
        JobExecution,
        Option<RegisteredSource>,
        Option<SourceOperation>,
    ),
    String,
> {
    let plan = state
        .jobs
        .lock()
        .map_err(|e| e.to_string())?
        .claim_plan(id, ClaimMode::RetryTerminal)
        .map_err(|e| e.to_string())?;
    // Validate the supported binding and complete source guard plan before the
    // retry claim. Historical/unavailable jobs remain byte-for-byte intact.
    let source = retry_source(state, &plan.job().kind)?;
    let operation = source
        .as_ref()
        .map(|source| source_operation(state, vec![(source.clone(), false)]))
        .transpose()?;
    let (job, execution) = claim_job(state, &plan)?;
    Ok((job, execution, source, operation))
}

fn retry_job_blocking<R: tauri::Runtime>(
    id: i64,
    app: tauri::AppHandle<R>,
) -> Result<RetriedJob, String> {
    let state = app.state::<AppState>();
    let (job, execution, source, operation) = prepare_job_retry(&state, id)?;
    emit_job(&app, &job);
    let kind = job.kind;
    if let (Some(source), Some(operation)) = (source, operation) {
        let summary = run_ingest(&source, &operation, &execution, &app, &state)?;
        let job = state
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .get(id)
            .map_err(|e| e.to_string())?;
        return Ok(RetriedJob {
            job,
            recovery: Some(RetriedRecovery {
                preflight: summary.preflight,
                preflight_register: summary.preflight_register,
            }),
        });
    }
    // A conformance gate re-runs whole (#206 review): the verdict re-binds
    // to whatever bytes are on disk now, which is exactly what a retry
    // after an interrupt or a fixed corpus should do.
    if let Some(plugin_id) = kind.strip_prefix("plugin-gate:") {
        plugin_gate_blocking(plugin_id, &execution, &app)?;
        let jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        return jobs
            .get(id)
            .map(RetriedJob::from)
            .map_err(|e| e.to_string());
    }
    if kind == "noop" {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        let job = updated_job(
            jobs.finish_execution(&execution, &[])
                .map_err(|e| e.to_string())?,
        );
        emit_job(&app, &job);
        return Ok(job.into());
    }
    // add-repo / add-system re-dispatch needs their pipelines refactored
    // behind the same job-id seam; until then the caller is told explicitly
    // rather than silently doing nothing.
    let error = format!("retry re-dispatch not yet supported for kind '{kind}' — re-run the add");
    report_failure(&app, &state, &execution, &error);
    Err(error)
}

#[derive(Serialize)]
struct AddRepoSummary {
    job_id: i64,
    repo: String,
    commit_sha: String,
    files: u64,
    nodes: u64,
    edges: u64,
    merged: MergedFacts,
    layers: LayerBreakdown,
    delta: DeltaSummary,
    /// The clone's preflight report, reconciled with this recovery's eval
    /// proof exactly as a local recovery's is (AC-0209, #446).
    preflight: ingest::preflight::PreflightReport,
    /// As `IngestSummary::preflight_register` (#458).
    preflight_register: Option<RegisterStamp>,
}

/// Clone a GitHub repo (read-only, shallow) and ingest it with its real
/// identity — every fact's evidence carries owner/name@sha (US-0001,
/// AC-0001). Auth per the ADR-0009 ladder; failures carry remediation and
/// leave no partial clone (AC-0003).
#[tauri::command]
async fn add_repo(url: String, app: tauri::AppHandle) -> Result<AddRepoSummary, String> {
    off_ui_thread(move || add_repo_blocking(url, app)).await
}

fn add_repo_blocking<R: tauri::Runtime>(
    url: String,
    app: tauri::AppHandle<R>,
) -> Result<AddRepoSummary, String> {
    let state = app.state::<AppState>();
    let source = state
        .sources
        .lock()
        .map_err(|e| e.to_string())?
        .reserve_managed(&url)?;
    let mut operation = source_operation(&state, vec![(source.clone(), true)])?;
    let (running, execution) = start_job(&state, &format!("add-repo:{url}"))?;
    let job_id = execution.id();
    emit_job(&app, &running);
    let fail = |e: String| -> String {
        report_failure(&app, &state, &execution, &e);
        e
    };

    if job_cancelled(&state, &execution)? {
        return Err("cancelled".into());
    }
    let token = ingest::discover_token();
    let cloned = operation
        .clone_source(&source, token.as_deref())
        .map_err(&fail)?;
    if job_cancelled(&state, &execution)? {
        return Err("cancelled".into());
    }
    let root = operation.root(&source.repo_key).map_err(&fail)?;
    let active_plugins = active_plugins_for_root(&app, &state, root).map_err(&fail)?;
    let mut on_file = detail_throttle(&app, job_id);
    let primary = state
        .primary_sources
        .prepare(&source, root, &[])
        .map_err(&fail)?;
    let mut receipts = Vec::new();
    let (extraction, layers, delta) = {
        let mut caches = state
            .extraction_caches
            .lock()
            .map_err(|e| fail(e.to_string()))?;
        ensure_running_job(&state, &execution)?;
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_with_primary(
            root,
            &source.repo_key,
            &cloned.commit_sha,
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
            cache,
            &active_plugins,
            &mut on_file,
            primary.capture.as_ref(),
            &mut receipts,
        )
        .map_err(&fail)?
    };
    if job_cancelled(&state, &execution)? {
        return Err("cancelled".to_string());
    }
    state
        .primary_sources
        .persist(&primary, &source, &receipts)
        .map_err(&fail)?;
    let bindings = primary_source::matching_bindings(&extraction, &receipts);
    let mut published = OperationFacts::default();
    {
        let mut graph = state.graph.lock().map_err(|e| fail(e.to_string()))?;
        ensure_running_job(&state, &execution)?;
        published.record_load(
            load_into_graph_with_bindings(
                &mut graph,
                &extraction,
                &source.repo_key,
                root,
                &cloned.commit_sha,
                &bindings,
            )
            .map_err(&fail)?
            .published,
        );

        ensure_running_job(&state, &execution)?;
        stitch_backings(&mut graph, &mut published).map_err(&fail)?;
    }
    relink_found_adrs(&state, &operation, &execution, &mut published).map_err(&fail)?;
    ensure_running_job(&state, &execution)?;
    record_ingest_metrics(
        &state,
        job_id,
        &source.repo_key,
        &cloned.commit_sha,
        &layers,
        &std::collections::BTreeSet::from([source.repo_key.clone()]),
    )
    .map_err(&fail)?;
    // As for a local recovery (AC-0200, AC-0209): scanned only once the
    // graph holds the recovered facts, from the captured bytes the claims
    // were proven on.
    let preflight = reconciled_preflight_report(
        root,
        primary.capture.as_ref(),
        &plugin_coverage(&active_plugins),
        &extraction.eval_sites,
    )
    .map_err(&fail)?;
    // Reserved before `done` is published (#497).
    let fence = app.state::<PreflightRuns>().reserve_reconcile()?;
    finish_source_operation(&state, &app, &execution, &mut operation)?;
    // Written only once the job has settled as completed, so a cancel that
    // wins the race writes nothing (AC-0209); through the per-repo fence.
    let preflight_register =
        app.state::<PreflightRuns>()
            .reconcile(&fence, &source.repo_key, &preflight, || {
                persist_preflight_findings(&state, &source.repo_key, &preflight)
            })?;
    Ok(AddRepoSummary {
        job_id,
        repo: source.repo_key,
        commit_sha: cloned.commit_sha,
        files: layers.files(),
        nodes: published.nodes(),
        edges: published.edges(),
        merged: published.merged,
        layers,
        delta,
        preflight,
        preflight_register,
    })
}

/// GitHub-ish references clone; anything else is a path relative to the
/// manifest (local repos in one checkout, the dogfood case). A two-segment
/// entry like `services/api` is only owner/name shorthand when nothing by
/// that path exists next to the manifest — never resolved against the
/// process cwd.
fn manifest_entry_is_remote(url: &str, base: &std::path::Path) -> bool {
    url.starts_with("https://")
        || url.starts_with("git@")
        || url.starts_with("file://")
        || (url.split('/').count() == 2 && !base.join(url).exists())
}

#[derive(Serialize)]
struct AddSystemSummary {
    job_id: i64,
    /// `identity@sha12` per ingested repo, in manifest order.
    repos: Vec<String>,
    files: u64,
    nodes: u64,
    edges: u64,
    merged: MergedFacts,
    layers: LayerBreakdown,
    delta: DeltaSummary,
    /// Each recovered repo's reconciled preflight report, in manifest order
    /// (AC-0209, #446). Not `preflight`: the Preflight surface shows one
    /// repo's report, and a system has several.
    preflights: Vec<RepoPreflight>,
}

/// One repo's reconciled preflight report within a system recovery.
#[derive(Serialize)]
struct RepoPreflight {
    repo: String,
    report: ingest::preflight::PreflightReport,
}

fn manifest_dir(path: &std::path::Path) -> &std::path::Path {
    if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(std::path::Path::new("."))
    }
}

/// Ingest a whole system from `cartograph.system.toml` (US-0001 AC-0002):
/// clone/read every declared repo, apply its layer hints and the
/// manifest's known channel identities at ingest.
#[tauri::command]
async fn add_system(path: String, app: tauri::AppHandle) -> Result<AddSystemSummary, String> {
    off_ui_thread(move || add_system_blocking(path, app)).await
}

fn add_system_blocking<R: tauri::Runtime>(
    path: String,
    app: tauri::AppHandle<R>,
) -> Result<AddSystemSummary, String> {
    let state = app.state::<AppState>();
    let manifest_path = paths::canonicalize(&path).map_err(|e| e.to_string())?;
    let manifest =
        ingest::manifest::SystemManifest::load(&manifest_path).map_err(|e| e.to_string())?;
    let base = manifest_dir(&manifest_path);
    let admitted = {
        let mut registry = state.sources.lock().map_err(|e| e.to_string())?;
        manifest
            .repos
            .iter()
            .map(|entry| {
                let remote = manifest_entry_is_remote(&entry.url, base);
                let source = if remote {
                    registry.reserve_managed(&entry.url)?
                } else {
                    registry.register_local(&base.join(&entry.url))?
                };
                Ok((source, remote))
            })
            .collect::<Result<Vec<_>, String>>()?
    };
    let mut operation = source_operation(&state, admitted.clone())?;
    let (running, execution) = start_job(&state, &format!("add-system:{path}"))?;
    let job_id = execution.id();
    emit_job(&app, &running);
    let fail = |e: String| -> String {
        report_failure(&app, &state, &execution, &e);
        e
    };

    let token = ingest::discover_token();
    let mut cloned_commits = std::collections::BTreeMap::new();
    for (source, remote) in &admitted {
        if *remote
            && let std::collections::btree_map::Entry::Vacant(entry) =
                cloned_commits.entry(source.source_id.clone())
        {
            if job_cancelled(&state, &execution)? {
                return Err("cancelled".into());
            }
            let cloned = operation
                .clone_source(source, token.as_deref())
                .map_err(&fail)?;
            entry.insert(cloned.commit_sha);
        }
    }

    let mut repos = Vec::new();
    let mut repo_identities = std::collections::BTreeSet::new();
    let mut files = 0u64;
    let mut published = OperationFacts::default();
    let mut layers = LayerBreakdown::default();
    let mut delta = DeltaSummary::default();
    // Each repo's eval proof, reconciled only once the whole system is
    // published: a later repo's failure must leave every register pending.
    let mut proofs = Vec::new();
    let mut on_file = detail_throttle(&app, job_id);
    for (entry, (source, remote)) in manifest.repos.iter().zip(&admitted) {
        if job_cancelled(&state, &execution)? {
            return Err("cancelled".to_string());
        }
        let root = operation.root(&source.repo_key).map_err(&fail)?;
        let repo = source.repo_key.clone();
        let commit = if *remote {
            cloned_commits
                .get(&source.source_id)
                .ok_or("missing managed clone result")?
                .clone()
        } else {
            "workdir".to_string()
        };
        // state_json travels with the manifest, so it resolves against the
        // manifest dir — same rule as local repo paths.
        let state_path = entry.state_json.as_ref().map(|p| base.join(p));
        let pulumi_path = entry.pulumi_json.as_ref().map(|p| base.join(p));
        let trace_paths: Vec<std::path::PathBuf> =
            entry.otel_jsonl.iter().map(|p| base.join(p)).collect();
        let active_plugins = active_plugins_for_root(&app, &state, root).map_err(&fail)?;
        let primary = state
            .primary_sources
            .prepare(source, root, &entry.layers)
            .map_err(&fail)?;
        let mut receipts = Vec::new();
        let (extraction, repo_layers, repo_delta) = {
            let mut caches = state
                .extraction_caches
                .lock()
                .map_err(|e| fail(e.to_string()))?;
            ensure_running_job(&state, &execution)?;
            let cache = caches.repos.entry(repo.clone()).or_default();
            extract_tree_with_primary(
                root,
                &repo,
                &commit,
                &entry.layers,
                &manifest.env,
                state_path.as_deref(),
                pulumi_path.as_deref(),
                &trace_paths,
                cache,
                &active_plugins,
                &mut on_file,
                primary.capture.as_ref(),
                &mut receipts,
            )
            .map_err(&fail)?
        };
        ensure_running_job(&state, &execution)?;
        state
            .primary_sources
            .persist(&primary, source, &receipts)
            .map_err(&fail)?;
        let bindings = primary_source::matching_bindings(&extraction, &receipts);
        files += repo_layers.files();
        layers.add(repo_layers);
        delta.add(
            repo_delta.recomputed_files,
            repo_delta.reused_files,
            repo_delta.deleted_files,
        );
        {
            let mut graph = state.graph.lock().map_err(|e| fail(e.to_string()))?;
            ensure_running_job(&state, &execution)?;
            published.record_load(
                load_into_graph_with_bindings(
                    &mut graph,
                    &extraction,
                    &repo,
                    root,
                    &commit,
                    &bindings,
                )
                .map_err(&fail)?
                .published,
            );
        }
        let sha12: String = commit.chars().take(12).collect();
        repos.push(format!("{repo}@{sha12}"));
        if repo_identities.insert(repo.clone()) {
            proofs.push((
                repo,
                primary,
                plugin_coverage(&active_plugins),
                extraction.eval_sites,
            ));
        }
    }
    {
        // After every repo is in: infra from one repo can back channels
        // published by another.
        let mut graph = state.graph.lock().map_err(|e| fail(e.to_string()))?;
        ensure_running_job(&state, &execution)?;
        stitch_backings(&mut graph, &mut published).map_err(&fail)?;
    }
    relink_found_adrs(&state, &operation, &execution, &mut published).map_err(&fail)?;
    // One history record for the whole system; the per-repo identities are
    // the record's identity (a system has no single commit).
    ensure_running_job(&state, &execution)?;
    record_ingest_metrics(
        &state,
        job_id,
        &repos.join(","),
        "system",
        &layers,
        &repo_identities,
    )
    .map_err(&fail)?;
    // AC-0200, AC-0209: each repo reconciles exactly as a local recovery.
    let preflights = proofs
        .into_iter()
        .map(|(repo, primary, plugins, eval_sites)| {
            let root = operation.root(&repo)?;
            let report =
                reconciled_preflight_report(root, primary.capture.as_ref(), &plugins, &eval_sites)?;
            Ok(RepoPreflight { repo, report })
        })
        .collect::<Result<Vec<_>, String>>()
        .map_err(&fail)?;
    // Reserved before `done` is published (#497).
    let runs = app.state::<PreflightRuns>();
    let fence = runs.reserve_reconcile()?;
    finish_source_operation(&state, &app, &execution, &mut operation)?;
    // Written only once the job has settled as completed, so a cancel that
    // wins the race writes nothing (AC-0209); through each repo's fence.
    for preflight in &preflights {
        runs.reconcile(&fence, &preflight.repo, &preflight.report, || {
            persist_preflight_findings(&state, &preflight.repo, &preflight.report)
        })?;
    }
    Ok(AddSystemSummary {
        job_id,
        repos,
        files,
        nodes: published.nodes(),
        edges: published.edges(),
        merged: published.merged,
        layers,
        delta,
        preflights,
    })
}

/// The resource/topology map artifact as Mermaid text (SPEC-00 §7, M2 exit
/// gate; channels join via observed BACKS edges at M6). Deterministic for
/// a given graph.
#[tauri::command]
fn export_topology(state: State<'_, AppState>) -> Result<String, String> {
    let (nodes, edges) = {
        let graph = state.graph.lock().map_err(|e| e.to_string())?;
        read_filtered_graph(
            &*graph,
            &["Resource", "Channel"],
            spec::TOPOLOGY_EDGE_LABELS,
        )?
    };
    Ok(spec::topology_mermaid(&nodes, &edges))
}

/// The flow-dossier artifact as Markdown (SPEC-00 §7, M3 exit gate):
/// every T0-traceable flow with per-hop tiers, Gap truncation, and score.
#[tauri::command]
fn export_flows(state: State<'_, AppState>) -> Result<String, String> {
    let (nodes, edges) = read_flow_graph(&state.graph)?;
    let flows = flowtracer::trace(&nodes, &edges);
    Ok(spec::flow_dossier(&flows))
}

/// The traced flows as data (same graph slice as `export_flows`) — the UI
/// surfaces status and score per R-INT-2 without parsing the dossier.
#[tauri::command]
fn list_flows(state: State<'_, AppState>) -> Result<Vec<flowtracer::Flow>, String> {
    let (nodes, edges) = read_flow_graph(&state.graph)?;
    Ok(flowtracer::trace(&nodes, &edges))
}

/// The anchor kinds the tracer sought in the current graph, with counts —
/// a zero-flow Inspector names what recovery looked for (#165, R-INT-4).
#[tauri::command]
fn list_flow_anchors(state: State<'_, AppState>) -> Result<Vec<flowtracer::AnchorProbe>, String> {
    let (nodes, edges) = read_flow_graph(&state.graph)?;
    Ok(flowtracer::anchor_probes(&nodes, &edges))
}

/// Copy the flow selection before tracing or rendering.
fn read_flow_graph(graph: &Mutex<SqliteGraphStore>) -> Result<(Vec<Node>, Vec<Edge>), String> {
    let graph = graph.lock().map_err(|e| e.to_string())?;
    read_filtered_graph(
        &*graph,
        flowtracer::FLOW_NODE_LABELS,
        flowtracer::FLOW_EDGE_LABELS,
    )
}

/// Preserve the legacy sequence of label queries without mixing database
/// revisions. The stable label-rank sort happens after the read transaction;
/// within each label, nodes keep the snapshot's stable id order. Edge order
/// remains (src, dst, label), as it was for the original filtered edge query.
fn read_filtered_graph(
    graph: &impl GraphStore,
    node_labels: &[&str],
    edge_labels: &[&str],
) -> Result<(Vec<Node>, Vec<Edge>), String> {
    let (mut nodes, edges) = graph
        .read_snapshot_filtered(Some(node_labels), Some(edge_labels))
        .map_err(|e| e.to_string())?;
    nodes.sort_by_key(|node| {
        node_labels
            .iter()
            .position(|label| *label == node.label)
            .unwrap_or(usize::MAX)
    });
    Ok((nodes, edges))
}

#[derive(Serialize)]
struct SemanticPreview {
    eval_id: Option<i64>,
    provider: String,
    eval: semantic::EvalReport,
    proposals: Vec<semantic::SemanticProposal>,
    approved: Vec<semantic::SemanticProposal>,
    gaps_filled: usize,
    flows: Vec<flowtracer::Flow>,
    dossier: String,
}

fn build_semantic_preview(
    provider: &dyn LlmProvider,
    nodes: &[Node],
    edges: &[core_graph::Edge],
    eval_pairs: &[semantic::LabeledPair],
    precision_floor: f32,
) -> Result<SemanticPreview, String> {
    let eval = semantic::evaluate(provider, eval_pairs, precision_floor)
        .map_err(|error| error.to_string())?;
    let (hops, candidates) = semantic::graph_inputs(nodes, edges);
    let proposals =
        semantic::propose(provider, &hops, &candidates, 3).map_err(|error| error.to_string())?;
    let approved = semantic::gated_proposals(&proposals, &eval);
    let overlay = semantic::overlay(nodes, edges, &approved, &eval);
    let flows = flowtracer::trace(&overlay.nodes, &overlay.edges);
    let dossier = spec::flow_dossier(&flows);
    Ok(SemanticPreview {
        eval_id: None,
        provider: provider.id().to_string(),
        eval,
        proposals,
        approved,
        gaps_filled: overlay.gaps_filled,
        flows,
        dossier,
    })
}

/// Run the local-only T2 resolver as a best-effort preview. Confirmed graph
/// facts are read into an ephemeral overlay; only eval-approved Gap fills are
/// reflected in the returned flows and dossier.
#[tauri::command]
async fn semantic_preview(
    eval_pairs: Vec<semantic::LabeledPair>,
    precision_floor: f32,
    state: State<'_, AppState>,
) -> Result<SemanticPreview, String> {
    let (nodes, edges) = {
        let graph = state.graph.lock().map_err(|error| error.to_string())?;
        // Computed channel gaps can be backed only by a T0 IaC Resource.
        // Resources are semantic candidates, not flow nodes, and any Channel
        // they imply is materialized only in the ephemeral approved overlay.
        let mut node_labels = flowtracer::FLOW_NODE_LABELS.to_vec();
        node_labels.push("Resource");
        read_filtered_graph(&*graph, &node_labels, flowtracer::FLOW_EDGE_LABELS)?
    };
    let mut preview = tauri::async_runtime::spawn_blocking(move || {
        let provider = llm::OllamaProvider::local_default().map_err(|error| error.to_string())?;
        build_semantic_preview(&provider, &nodes, &edges, &eval_pairs, precision_floor)
    })
    .await
    .map_err(|error| error.to_string())??;
    let mut jobs = state.jobs.lock().map_err(|error| error.to_string())?;
    let eval = jobs
        .record_eval(
            &preview.provider,
            &preview.eval,
            preview.proposals.len(),
            preview.approved.len(),
        )
        .map_err(|error| error.to_string())?;
    preview.eval_id = Some(eval.id);
    Ok(preview)
}

fn build_spec_bundle(
    graph: &impl GraphStore,
    decisions: &agents::DecisionLog,
    mode: spec::ExportMode,
) -> Result<spec::SpecBundle, String> {
    let (nodes, edges) = graph.read_snapshot().map_err(|error| error.to_string())?;
    let flows = flowtracer::trace(&nodes, &edges);
    let rejected_hashes = decisions
        .list_assertions()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|record| record.decision == agents::AssertionDecision::Rejected)
        .map(|record| record.assertion.provenance.content_hash)
        .collect();
    Ok(spec::compile_spec(
        &nodes,
        &edges,
        &flows,
        mode,
        &rejected_hashes,
    ))
}

/// Compile the full official spec set under one R-INT-5 export policy.
#[tauri::command]
fn export_spec(
    mode: spec::ExportMode,
    state: State<'_, AppState>,
) -> Result<spec::SpecBundle, String> {
    let graph = state.graph.lock().map_err(|error| error.to_string())?;
    let decisions = state.decisions.lock().map_err(|error| error.to_string())?;
    build_spec_bundle(&*graph, &decisions, mode)
}

/// Nodes carrying `label` (e.g. `Endpoint`, `Repo`), ordered by id.
#[tauri::command]
fn list_nodes(label: String, state: State<'_, AppState>) -> Result<Vec<Node>, String> {
    let graph = state.graph.lock().map_err(|e| e.to_string())?;
    graph.nodes_with_label(&label).map_err(|e| e.to_string())
}

/// Complete, deterministically ordered graph projection for the read-only
/// Atlas surface. Provenance remains attached to every returned fact.
#[derive(Debug, PartialEq, Eq, Serialize)]
struct AtlasSnapshot {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

fn build_atlas_snapshot(graph: &impl GraphStore) -> Result<AtlasSnapshot, String> {
    let (nodes, edges) = graph.read_snapshot().map_err(|error| error.to_string())?;
    Ok(AtlasSnapshot { nodes, edges })
}

#[tauri::command]
fn atlas_snapshot(state: State<'_, AppState>) -> Result<AtlasSnapshot, String> {
    let graph = state.graph.lock().map_err(|error| error.to_string())?;
    build_atlas_snapshot(&*graph)
}

#[derive(Serialize)]
struct EvidenceSource {
    text: String,
    window_start: u64,
    window_start_line: u64,
    truncated: bool,
}

/// Read-only source window containing an evidence span, confined to the
/// exact registered repository root (NG1: navigation, never edit).
#[tauri::command]
fn read_evidence(
    repo: String,
    path: String,
    byte_start: u64,
    byte_end: u64,
    state: State<'_, AppState>,
) -> Result<EvidenceSource, String> {
    let window = source_access::with_registered_read(&state.sources, &repo, |root| {
        evidence::read_source(root, &path, &(byte_start..byte_end)).map_err(|e| e.to_string())
    })?;
    Ok(EvidenceSource {
        text: window.text,
        window_start: window.window_start,
        window_start_line: window.window_start_line,
        truncated: window.truncated,
    })
}

/// Open a URL in the system browser — never inside the webview (#154).
fn open_external(url: &str) {
    let Some((program, args)) = url_launcher(std::env::consts::OS, url) else {
        eprintln!(
            "cartograph: no system browser launcher for {}; not opening {url}",
            std::env::consts::OS
        );
        return;
    };
    let mut command = std::process::Command::new(program);
    command.args(args);
    // A GUI app spawning a console program flashes a console window on
    // Windows unless the child is created without one (#226).
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    if let Err(error) = command.spawn() {
        eprintln!("cartograph: could not launch {program} for {url}: {error}");
    }
}

/// The program and arguments that hand `url` to the system browser on `os`
/// (`std::env::consts::OS`), or `None` where no launcher is known (AC-0222).
///
/// Windows uses the URL protocol handler rather than `explorer`, which cuts a
/// URL at its first `&` and exits nonzero even on success. `rundll32` gets
/// the URL as one argument and no shell parses it, so `&` needs no quoting
/// (unlike `cmd /C start`).
fn url_launcher(os: &str, url: &str) -> Option<(&'static str, Vec<String>)> {
    match os {
        "macos" => Some(("open", vec![url.to_string()])),
        "linux" | "freebsd" | "netbsd" | "openbsd" | "dragonfly" => {
            Some(("xdg-open", vec![url.to_string()]))
        }
        "windows" => Some((
            "rundll32",
            vec!["url.dll,FileProtocolHandler".to_string(), url.to_string()],
        )),
        _ => None,
    }
}

/// Native Help submenu (#154): in-app Help, the wiki user guide, issue
/// reporting, and About — appended to the platform-default menu so the
/// standard Edit/Window items survive.
fn install_help_menu(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu};
    let handle = app.handle();
    let help = Submenu::with_items(
        handle,
        "Help",
        true,
        &[
            &MenuItem::with_id(handle, "help:open", "Cartograph Help", true, None::<&str>)?,
            &MenuItem::with_id(
                handle,
                "help:guide",
                "User guide (wiki)",
                true,
                None::<&str>,
            )?,
            &MenuItem::with_id(handle, "help:issue", "Report an issue", true, None::<&str>)?,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::about(
                handle,
                Some("About Cartograph"),
                Some(AboutMetadata {
                    name: Some("Cartograph".into()),
                    version: Some(app.package_info().version.to_string()),
                    ..Default::default()
                }),
            )?,
        ],
    )?;
    let menu = Menu::default(handle)?;
    // The platform default may already ship a (near-empty) Help submenu —
    // replace it rather than presenting two Help menus (#195 review).
    for item in menu.items()? {
        let Some(existing) = item.as_submenu() else {
            continue;
        };
        if existing.text()? == "Help" {
            menu.remove(&item)?;
        }
    }
    menu.append(&help)?;
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| match event.id().0.as_str() {
        "help:open" => {
            let _ = app.emit("help://open", ());
        }
        "help:guide" => open_external("https://github.com/qwts/cartograph/wiki"),
        "help:issue" => open_external("https://github.com/qwts/cartograph/issues/new"),
        _ => {}
    });
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            install_help_menu(app)?;
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let graph = SqliteGraphStore::open(data_dir.join("graph.db"))?;
            let state_path = data_dir.join("state.db");
            let jobs = JobStore::open(&state_path)?;
            let job_execution_locks =
                JobExecutionLocks::open(&data_dir, jobs.execution_namespace())?;
            let jobs = Mutex::new(jobs);
            // Only unchanged recorded attempts whose OS ownership is available
            // are interrupted. Live and legacy-unknown jobs remain untouched.
            recover_jobs(&jobs, &job_execution_locks).map_err(std::io::Error::other)?;
            investigations::recover(&jobs, &job_execution_locks).map_err(std::io::Error::other)?;
            let findings = FindingStore::open(&state_path)?;
            let sources =
                SourceRegistry::open(&state_path, &data_dir).map_err(std::io::Error::other)?;
            let tier_settings = settings::SettingsStore::open(&state_path)?;
            // Extraction reads the worker count process-wide (#236).
            source_walk::parallel::set_parallelism(settings::IngestParallelism::parallelism(
                tier_settings.ingest_parallelism()?.setting,
            ));
            let decisions = agents::DecisionLog::open(&state_path)?;
            let staged_proposals = agents::ProposalStore::open(data_dir.join("proposals.sqlite"))?;
            let recovery_metrics = metrics::MetricsStore::open(&state_path)?;
            app.manage(PreflightRuns::default());
            app.manage(AppState {
                graph: Mutex::new(graph),
                jobs,
                investigations: crate::investigations::InvestigationRuntime::default(),
                job_execution_locks,
                findings: Mutex::new(findings),
                settings: Mutex::new(tier_settings),
                decisions: Mutex::new(decisions),
                proposals: Mutex::new(staged_proposals),
                extraction_caches: Mutex::new(ExtractionCaches::default()),
                sources: Arc::new(Mutex::new(sources)),
                primary_sources: primary_source::PrimarySourceStore::open(&data_dir)
                    .map_err(std::io::Error::other)?,
                metrics: Mutex::new(recovery_metrics),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            investigations::commands::investigation_specialists,
            investigations::commands::start_investigation,
            investigations::commands::list_investigations,
            investigations::commands::get_investigation,
            investigations::commands::investigation_events,
            investigations::commands::investigation_result,
            investigations::commands::investigation_consent,
            investigations::commands::approve_investigation_step,
            investigations::commands::decline_investigation_step,
            investigations::commands::cancel_investigation,
            investigations::commands::read_investigation_citation,
            ping,
            graph_stats,
            clear_graph,
            system_contents,
            adapter_inventory,
            list_plugins,
            set_plugin_enabled,
            run_plugin_gate,
            clear_finished_jobs,
            list_jobs,
            list_evals,
            proposals::record_agent_decision,
            proposals::list_staged_proposals,
            task_evidence::assessment::assess_staged_basis,
            list_agent_decisions,
            reapply_agent_decisions,
            record_assertion_decision,
            list_assertion_decisions,
            ingest_path,
            cancel_job,
            retry_job,
            preflight,
            cancel_preflight,
            findings_summary,
            list_findings,
            get_settings,
            set_tier_enabled,
            set_tier_provider,
            grant_cloud_consent,
            revoke_cloud_consent,
            egress_summary,
            get_ingest_parallelism,
            set_ingest_parallelism,
            cloud_disclosure,
            ingest_history,
            extractor_coverage,
            gap_strategies,
            escalation_preview,
            run_escalation,
            run_class_escalation,
            list_nodes,
            atlas_snapshot,
            context::query_context,
            read_evidence,
            primary_source::describe_captured_source,
            primary_source::read_captured_source,
            primary_source::list_retained_sources,
            primary_source::preview_forget_source,
            primary_source::forget_retained_source,
            export_topology,
            export_flows,
            list_flows,
            list_flow_anchors,
            export_spec,
            semantic_preview,
            add_repo,
            add_system
        ])
        .run(tauri::generate_context!())
        .expect("error while running Cartograph");
}

#[cfg(test)]
fn test_source_registry(
    state_path: &std::path::Path,
    roots: &std::collections::BTreeSet<String>,
) -> Arc<Mutex<SourceRegistry>> {
    let mut registry = SourceRegistry::open(state_path, state_path.parent().unwrap()).unwrap();
    for root in roots {
        registry.register_local(std::path::Path::new(root)).unwrap();
    }
    Arc::new(Mutex::new(registry))
}

#[cfg(test)]
mod tests {
    use super::test_source_registry;
    use core_graph::{Edge, GraphStore, Node, SqliteGraphStore};
    use llm::{Embedding, Locality, ProviderCaps, ProviderError};

    #[test]
    fn windows_url_launcher_keeps_every_query_parameter() {
        // AC-0222 (#226): the URL reaches the protocol handler as one
        // argument, `&` and all; `explorer` truncated it at the first `&`.
        let url = "https://github.com/qwts/cartograph/issues/new?labels=bug&title=a%20b&body=x";
        let (program, args) = super::url_launcher("windows", url).unwrap();
        assert_eq!(program, "rundll32");
        assert_eq!(args, ["url.dll,FileProtocolHandler", url]);
    }

    #[test]
    fn url_launcher_covers_desktop_oses_and_declines_unknown_ones() {
        // AC-0222: macOS and Linux pass the URL through untouched; an OS
        // with no known launcher gets None (logged, never a guessed program).
        let url = "https://example.test/?a=1&b=2";
        assert_eq!(
            super::url_launcher("macos", url),
            Some(("open", vec![url.to_string()]))
        );
        assert_eq!(
            super::url_launcher("linux", url),
            Some(("xdg-open", vec![url.to_string()]))
        );
        assert_eq!(super::url_launcher("ios", url), None);
        assert_eq!(super::url_launcher("android", url), None);
    }

    #[test]
    fn findings_summary_counts_with_register_predicates() {
        // #116: one predicate set (spec's) feeds every surface, and the
        // three lanes never bleed into each other. A fact without
        // provenance counts as a Gap (unknown ≠ confirmed), so the
        // non-gap fixtures carry real Confirmed provenance.
        let confirmed = serde_json::to_value(
            core_prov::Provenance::new(
                core_prov::Tier::Deterministic,
                core_prov::ConfidenceTier::Confirmed,
                vec![],
                "t0.adapter-ts",
                b"fixture",
            )
            .expect("within ceiling"),
        )
        .expect("serializes");
        let nodes = vec![
            Node {
                id: "gap:chan".into(),
                label: "Gap".into(),
                props: serde_json::json!({}),
            },
            Node {
                id: "drift:adr-3".into(),
                label: "Drift".into(),
                props: serde_json::json!({ "kind": "drift", "prov": confirmed }),
            },
            Node {
                id: "svc:api".into(),
                label: "Service".into(),
                props: serde_json::json!({ "prov": confirmed }),
            },
        ];
        let gap = serde_json::to_value(
            core_prov::Provenance::new(
                core_prov::Tier::Deterministic,
                core_prov::ConfidenceTier::Gap,
                vec![],
                "t0.adapter-ts",
                b"gap-fixture",
            )
            .expect("within ceiling"),
        )
        .expect("serializes");
        let edges = vec![
            Edge {
                src: "svc:api".into(),
                dst: "adr:3".into(),
                label: "CONFLICTS".into(),
                props: serde_json::json!({ "prov": confirmed }),
            },
            // An unresolved call emits a gap node AND a gap CALLS edge; the
            // edge supports the same finding and must not double it (#241).
            Edge {
                src: "svc:api".into(),
                dst: "gap:chan".into(),
                label: "CALLS".into(),
                props: serde_json::json!({
                    "reason": "unresolved call target after import/type resolution",
                    "prov": &gap,
                }),
            },
            // An edge-only gap between two real facts (no gap node on either
            // end) is a finding of its own — folding must not swallow it.
            Edge {
                src: "svc:api".into(),
                dst: "sym:handler".into(),
                label: "CALLS".into(),
                props: serde_json::json!({
                    "reason": "callee not statically resolvable",
                    "prov": gap,
                }),
            },
        ];
        let summary = super::summarize_register(&nodes, &edges, 2, 1);
        // One finding for the node+edge pair, one for the standalone edge.
        assert_eq!(summary.gaps, 2);
        // Drift keeps its own node-only rule: the CONFLICTS edge supports
        // the drift node's finding (parity with drift_register's count).
        assert_eq!(summary.drift, 1);
        assert_eq!(summary.unsupported, 2);
        assert_eq!(summary.no_evidence, 1);
        assert_eq!(summary.open_findings, 5); // 2 gaps + 2 unsupported + 1 no-evidence
        assert_eq!(summary.graph_facts, 6);
    }

    #[test]
    fn adapter_inventory_matches_what_the_ts_walker_ingests() {
        // AC-0095 (#247): the Settings adapter cards derive their "Files:"
        // copy from ingest::preflight's registry, while the walker ingests
        // adapters_lang_ts::SOURCE_EXTENSIONS. One assertion binds the two
        // hand-maintained lists so coverage and inventory cannot disagree.
        let registry: std::collections::BTreeSet<String> = ingest::preflight::INSTALLED_ADAPTERS
            .iter()
            .filter(|adapter| adapter.id == "t0.adapter-ts")
            .flat_map(|adapter| adapter.extensions.iter().map(|ext| format!(".{ext}")))
            .collect();
        let walker: std::collections::BTreeSet<String> = adapters_lang_ts::SOURCE_EXTENSIONS
            .iter()
            .map(|ext| (*ext).to_string())
            .collect();
        assert_eq!(
            registry, walker,
            "Settings/Preflight adapter inventory must list exactly the \
             extensions the TS/JS walker ingests"
        );
    }

    struct M7KeywordProvider;

    impl llm::LlmProvider for M7KeywordProvider {
        fn id(&self) -> &str {
            "test-keywords"
        }

        fn locality(&self) -> Locality {
            Locality::Local
        }

        fn capabilities(&self) -> ProviderCaps {
            ProviderCaps {
                embeddings: true,
                chat: false,
                tool_use: false,
            }
        }

        fn embed(&self, batch: &[String]) -> Result<Vec<Embedding>, ProviderError> {
            Ok(batch
                .iter()
                .map(|text| {
                    let text = text.to_ascii_lowercase();
                    vec![
                        f32::from(text.contains("order")),
                        f32::from(text.contains("user")),
                        f32::from(text.contains("billing")),
                        0.01,
                    ]
                })
                .collect())
        }
    }

    struct M7RealInputsProvider;

    impl llm::LlmProvider for M7RealInputsProvider {
        fn id(&self) -> &str {
            "test-real-input-keywords"
        }

        fn locality(&self) -> Locality {
            Locality::Local
        }

        fn capabilities(&self) -> ProviderCaps {
            ProviderCaps {
                embeddings: true,
                chat: false,
                tool_use: false,
            }
        }

        fn embed(&self, batch: &[String]) -> Result<Vec<Embedding>, ProviderError> {
            Ok(batch
                .iter()
                .map(|text| {
                    let text = text.to_ascii_lowercase();
                    vec![
                        f32::from(text.contains("order")),
                        f32::from(text.contains("process")),
                        f32::from(text.contains("queue")),
                        f32::from(text.contains("user")),
                        0.01,
                    ]
                })
                .collect())
        }
    }

    fn m7_prov(path: &str, confidence: &str) -> serde_json::Value {
        serde_json::json!({
            "tier": "Deterministic",
            "confidence_tier": confidence,
            "evidence": [{
                "repo": "local/shop",
                "path": path,
                "byte_start": 1,
                "byte_end": 5,
                "commit_sha": "abc123"
            }],
            "extractor_id": "t0.test",
            "content_hash": "hash"
        })
    }

    #[test]
    fn semantic_preview_fills_only_eval_gated_gap_overlay() {
        // AC-0021/AC-0022: app path stages T2 links, gates them on paired
        // precision, and traces an inferred overlay without mutating T0 input.
        let nodes = vec![
            Node {
                id: "ep:shop@POST:/orders".into(),
                label: "Endpoint".into(),
                props: serde_json::json!({
                    "method": "POST", "path": "/orders", "prov": m7_prov("api.ts", "Confirmed")
                }),
            },
            Node {
                id: "sym:shop@api.ts#placeOrder".into(),
                label: "Symbol".into(),
                props: serde_json::json!({
                    "name": "placeOrder", "prov": m7_prov("api.ts", "Confirmed")
                }),
            },
            Node {
                id: "gap:chan:shop@api.ts@10".into(),
                label: "Gap".into(),
                props: serde_json::json!({
                    "kind": "sqs-queue",
                    "raw": "computed order destination",
                    "reason": "runtime-computed channel identity",
                    "prov": m7_prov("api.ts", "Gap")
                }),
            },
            Node {
                id: "chan:sqs-queue:orders".into(),
                label: "Channel".into(),
                props: serde_json::json!({
                    "kind": "sqs-queue", "identity": "orders queue",
                    "prov": m7_prov("infra.tf", "Confirmed")
                }),
            },
            Node {
                id: "chan:sqs-queue:users".into(),
                label: "Channel".into(),
                props: serde_json::json!({
                    "kind": "sqs-queue", "identity": "users queue",
                    "prov": m7_prov("infra.tf", "Confirmed")
                }),
            },
        ];
        let edges = vec![
            Edge {
                src: "ep:shop@POST:/orders".into(),
                dst: "sym:shop@api.ts#placeOrder".into(),
                label: "HANDLES".into(),
                props: serde_json::json!({"prov": m7_prov("api.ts", "Confirmed")}),
            },
            Edge {
                src: "sym:shop@api.ts#placeOrder".into(),
                dst: "gap:chan:shop@api.ts@10".into(),
                label: "PUBLISHES".into(),
                props: serde_json::json!({"prov": m7_prov("api.ts", "Gap")}),
            },
        ];
        let eval_pairs = vec![
            semantic::LabeledPair {
                query: "order destination".into(),
                candidate: "orders queue".into(),
                is_match: true,
            },
            semantic::LabeledPair {
                query: "order destination".into(),
                candidate: "users queue".into(),
                is_match: false,
            },
            semantic::LabeledPair {
                query: "billing event".into(),
                candidate: "billing channel".into(),
                is_match: true,
            },
            semantic::LabeledPair {
                query: "billing event".into(),
                candidate: "users queue".into(),
                is_match: false,
            },
        ];
        let preview =
            crate::build_semantic_preview(&M7KeywordProvider, &nodes, &edges, &eval_pairs, 0.95)
                .unwrap();
        assert_eq!(preview.gaps_filled, 1);
        assert_eq!(preview.approved[0].target_id, "chan:sqs-queue:orders");
        assert_eq!(preview.flows[0].status, flowtracer::FlowStatus::Inferred);
        assert!(preview.dossier.contains("InferredStrong"));
        assert!(nodes.iter().any(|node| node.label == "Gap"));
    }

    #[test]
    fn semantic_preview_uses_real_ingested_resource_and_call_gaps() {
        // AC-0021 / #67: exercise the production extractors, not synthetic
        // graph fixtures. Computed SQS identity + IaC and an unresolved
        // relative-import call both reach the eval-gated T2 preview.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("publisher.ts"),
            r#"
import { SendMessageCommand } from '@aws-sdk/client-sqs';
declare function lookupQueue(): string;
const ordersQueueUrl = lookupQueue();
export function publishOrder() {
  return new SendMessageCommand({ QueueUrl: ordersQueueUrl, MessageBody: '{}' });
}
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("caller.ts"),
            r#"
import { processOrder } from './missing';
export function run() { processOrder(); }
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("orders.ts"),
            "export function processOrder() {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.tf"),
            r#"
resource "aws_sqs_queue" "orders" {
  name = "orders"
}
"#,
        )
        .unwrap();

        let extraction = crate::extract_tree(
            dir.path(),
            "local/shop",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        let gaps: Vec<_> = extraction
            .nodes
            .iter()
            .filter(|node| node.label == "Gap")
            .collect();
        assert_eq!(gaps.len(), 2, "real extraction gaps: {gaps:?}");
        assert!(gaps.iter().any(|node| node.props["kind"] == "sqs-queue"));
        assert!(
            gaps.iter()
                .any(|node| node.props["callee"] == "processOrder")
        );

        let eval_pairs = vec![
            semantic::LabeledPair {
                query: "order destination".into(),
                candidate: "orders queue".into(),
                is_match: true,
            },
            semantic::LabeledPair {
                query: "order destination".into(),
                candidate: "users queue".into(),
                is_match: false,
            },
            semantic::LabeledPair {
                query: "process order".into(),
                candidate: "process order".into(),
                is_match: true,
            },
            semantic::LabeledPair {
                query: "process order".into(),
                candidate: "publish order".into(),
                is_match: false,
            },
        ];
        let preview = crate::build_semantic_preview(
            &M7RealInputsProvider,
            &extraction.nodes,
            &extraction.edges,
            &eval_pairs,
            0.95,
        )
        .unwrap();
        assert_eq!(
            preview.gaps_filled, 2,
            "proposals: {:#?}",
            preview.proposals
        );
        assert!(preview.approved.iter().any(|proposal| {
            proposal.edge_label == "PUBLISHES"
                && proposal.target_node.as_ref().is_some_and(|node| {
                    node.props["backing_resource"] == "res:local/shop@aws_sqs_queue.orders"
                })
        }));
        assert!(preview.approved.iter().any(|proposal| {
            proposal.edge_label == "CALLS"
                && proposal.target_id == "sym:local/shop@orders.ts#processOrder"
        }));
        assert!(
            extraction
                .nodes
                .iter()
                .all(|node| { node.props["prov"]["confidence_tier"] != "InferredStrong" })
        );
    }

    #[test]
    fn ingest_parses_found_adr_and_links_explicit_governed_target() {
        // AC-0036 (T-0036): the production ingest path turns an existing
        // Markdown ADR and explicit target id into Confirmed graph facts.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("orders.ts"),
            "export function placeOrder() {}\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("docs/adr")).unwrap();
        std::fs::write(
            dir.path().join("docs/adr/ADR-0001-orders.md"),
            "# Place orders in one service\n\n- **Status:** Accepted\n- **Governs:** `sym:local/shop@orders.ts#placeOrder`\n",
        )
        .unwrap();

        let extraction = crate::extract_tree(
            dir.path(),
            "local/shop",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        let adr = extraction
            .nodes
            .iter()
            .find(|node| node.label == "ADR")
            .unwrap();
        assert_eq!(adr.props["origin"], "found");
        assert_eq!(adr.props["prov"]["confidence_tier"], "Confirmed");
        let decides = extraction
            .edges
            .iter()
            .find(|edge| edge.label == "DECIDES")
            .unwrap();
        assert_eq!(decides.src, adr.id);
        assert_eq!(decides.dst, "sym:local/shop@orders.ts#placeOrder");
        assert_eq!(decides.props["prov"]["confidence_tier"], "Confirmed");
    }

    #[test]
    fn polyglot_ingest_keeps_one_adapters_placeholder_gap() {
        // AC-0207 (#237 review): TS and Go both import `foo`. The TS closure
        // leaves `mod:foo` a Gap (a tsconfig paths alias with no file); the Go
        // closure proves it external. The merged extraction must hold one
        // `mod:foo`, the Gap — never the later Confirmed duplicate.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{"compilerOptions":{"baseUrl":".","paths":{"foo":["./nowhere/foo"]}}}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("app.ts"), "import { x } from \"foo\";\n").unwrap();
        std::fs::write(
            dir.path().join("go.mod"),
            "module example.com/app\n\ngo 1.22\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nimport \"foo\"\n\nfunc main() { foo.X() }\n",
        )
        .unwrap();

        let extraction = crate::extract_tree(
            dir.path(),
            "local/poly",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        let modules: Vec<_> = extraction
            .nodes
            .iter()
            .filter(|node| node.id == "mod:foo")
            .collect();
        assert_eq!(modules.len(), 1, "one reconciled mod:foo");
        assert_eq!(modules[0].props["placeholder"], true);
        assert_eq!(modules[0].props["boundary"], "unresolved");
        assert_eq!(modules[0].props["prov"]["confidence_tier"], "Gap");
        // Both adapters really did import it.
        for importer in ["file:local/poly@app.ts", "file:local/poly@main.go"] {
            assert!(
                extraction
                    .edges
                    .iter()
                    .any(|edge| edge.src == importer && edge.dst == "mod:foo")
            );
        }
    }

    #[test]
    fn system_relinks_found_adr_to_cross_repo_target() {
        // AC-0036 (T-0036): a decision repo may be loaded before the service
        // containing its explicit target; the full-system pass links it once
        // both repos are present.
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join("docs-repo");
        let service = dir.path().join("service");
        std::fs::create_dir_all(docs.join("docs/adr")).unwrap();
        std::fs::create_dir_all(&service).unwrap();
        std::fs::write(
            docs.join("docs/adr/ADR-0001-service.md"),
            "# Service ownership\n\n- **Status:** Accepted\n- **Governs:** `sym:local/service@app.ts#handle`\n",
        )
        .unwrap();
        std::fs::write(service.join("app.ts"), "export function handle() {}\n").unwrap();

        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        let docs_extraction = crate::extract_tree(
            &docs,
            "local/docs-repo",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        crate::load_into_graph(
            &mut store,
            &docs_extraction,
            "local/docs-repo",
            &docs,
            "workdir",
        )
        .unwrap();
        assert!(store.edges_with_labels(&["DECIDES"]).unwrap().is_empty());

        let service_extraction = crate::extract_tree(
            &service,
            "local/service",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        crate::load_into_graph(
            &mut store,
            &service_extraction,
            "local/service",
            &service,
            "workdir",
        )
        .unwrap();
        let updates = crate::collect_found_adrs(&store.all_nodes().unwrap(), |repo| match repo {
            "local/docs-repo" => Ok(docs.as_path()),
            "local/service" => Ok(service.as_path()),
            _ => Err("unexpected fixture source".into()),
        })
        .unwrap();
        crate::apply_found_adrs(&mut store, updates).unwrap();

        let decides = store.edges_with_labels(&["DECIDES"]).unwrap();
        assert_eq!(decides.len(), 1);
        assert_eq!(
            decides[0].src,
            "adr:local/docs-repo@docs/adr/ADR-0001-service.md"
        );
        assert_eq!(decides[0].dst, "sym:local/service@app.ts#handle");
        assert_eq!(decides[0].props["prov"]["confidence_tier"], "Confirmed");

        // Removing the declaration reconciles the previously confirmed edge;
        // it must not survive as a zombie after the next deterministic pass.
        std::fs::write(
            docs.join("docs/adr/ADR-0001-service.md"),
            "# Service ownership\n\n- **Status:** Accepted\n",
        )
        .unwrap();
        let updates = crate::collect_found_adrs(&store.all_nodes().unwrap(), |repo| match repo {
            "local/docs-repo" => Ok(docs.as_path()),
            "local/service" => Ok(service.as_path()),
            _ => Err("unexpected fixture source".into()),
        })
        .unwrap();
        crate::apply_found_adrs(&mut store, updates).unwrap();
        assert!(store.edges_with_labels(&["DECIDES"]).unwrap().is_empty());

        // Removing the source file reconciles the found ADR node as well.
        std::fs::remove_file(docs.join("docs/adr/ADR-0001-service.md")).unwrap();
        let updates = crate::collect_found_adrs(&store.all_nodes().unwrap(), |repo| match repo {
            "local/docs-repo" => Ok(docs.as_path()),
            "local/service" => Ok(service.as_path()),
            _ => Err("unexpected fixture source".into()),
        })
        .unwrap();
        crate::apply_found_adrs(&mut store, updates).unwrap();
        assert!(
            store
                .nodes_with_label("ADR")
                .unwrap()
                .into_iter()
                .all(|node| node.id != "adr:local/docs-repo@docs/adr/ADR-0001-service.md")
        );
    }

    #[test]
    fn layer_summary_reports_ts_and_tf_files_and_facts() {
        // AC-0049: zero counts are explicit, so a Pulumi/TS tree cannot look
        // like successful Terraform recovery.
        let ts_only = tempfile::tempdir().unwrap();
        std::fs::write(
            ts_only.path().join("infra.ts"),
            "export const bucket = 'pulumi-shaped';\n",
        )
        .unwrap();
        let (_, summary) = crate::extract_tree_with_summary(
            ts_only.path(),
            "local/pulumi",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(summary.ts.files, 1);
        assert!(summary.ts.nodes > 0);
        assert_eq!(summary.tf, crate::LayerSummary::default());

        std::fs::write(
            ts_only.path().join("main.tf"),
            "resource \"aws_s3_bucket\" \"uploads\" {}\n",
        )
        .unwrap();
        let (_, summary) = crate::extract_tree_with_summary(
            ts_only.path(),
            "local/mixed",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(summary.ts.files, 1);
        assert_eq!(summary.tf.files, 1);
        assert!(summary.tf.nodes > 0);
        assert_eq!(summary.files(), 2);
    }

    #[test]
    fn ingest_summary_quotes_distinct_published_facts_and_reports_merges() {
        // AC-0195 (#242): one relation cited from several sites, and distinct
        // declarations sharing one id, collapse to one stored fact each. The
        // summary must quote what the store holds and state what it merged.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("util.ts"),
            "export function helper() {}\nexport function other() {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("app.ts"),
            "import { helper } from './util';\nimport { other } from './util';\n\
             export function run() { helper(); helper(); other(); }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Owner.java"),
            "package demo;\nclass Owner {\n  Pet getPet(String name) { return null; }\n  \
             Pet getPet(int id) { return null; }\n}\nclass Pet {}\n",
        )
        .unwrap();
        let (extraction, layers) = crate::extract_tree_with_summary(
            dir.path(),
            "local/merges",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        let mut published = crate::OperationFacts::default();
        published.record_load(
            crate::load_into_graph(
                &mut store,
                &extraction,
                "local/merges",
                dir.path(),
                "workdir",
            )
            .unwrap()
            .published,
        );

        // The summary totals are the store's own counts after a first ingest.
        assert_eq!(
            store.fact_counts().unwrap(),
            (published.nodes(), published.edges())
        );
        // Nothing vanishes unreported: raw occurrences = published + merged
        // (the Repo node is the load's own addition).
        assert_eq!(
            extraction.nodes.len() as u64 + 1,
            published.nodes() + published.merged.nodes
        );
        assert_eq!(
            extraction.edges.len() as u64,
            published.edges() + published.merged.edges
        );
        // `run` calls `helper` twice and imports `./util` twice: one relation each.
        let calls_helper = extraction
            .edges
            .iter()
            .filter(|edge| {
                edge.label == "CALLS" && edge.src.ends_with("#run") && edge.dst.ends_with("#helper")
            })
            .count();
        assert_eq!(calls_helper, 2, "fixture must emit repeated call sites");
        assert!(published.merged.edges >= 2);
        // The two `getPet` overloads share one symbol id with differing spans.
        assert_eq!(published.merged.node_collisions, 1);
        assert!(published.merged.nodes >= 1);
        // Per-layer rows count distinct facts, so they never exceed the store.
        let java_symbols = extraction
            .nodes
            .iter()
            .filter(|node| node.id.contains("Owner.java"))
            .map(|node| node.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(java_symbols.len() as u64 <= layers.java.nodes);
        assert_eq!(
            layers.ts.edges,
            crate::distinct_edge_count(
                &extraction
                    .edges
                    .iter()
                    .filter(|edge| edge.props["prov"]["extractor_id"] == "t0.adapter-ts")
                    .cloned()
                    .collect::<Vec<_>>()
            )
        );
    }

    #[test]
    fn collapsed_relation_keeps_every_call_site_as_evidence() {
        // AC-0203 (#436): `run` calls `helper` twice. The stored CALLS edge
        // cites both sites in source order, and re-ingesting yields the same
        // edge. A relation cited once keeps its extracted provenance.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("util.ts"),
            "export function helper() {}\nexport function other() {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("app.ts"),
            "import { helper, other } from './util';\n\
             export function run() { helper(); other(); helper(); }\n",
        )
        .unwrap();
        let ingest = || {
            let extraction = crate::extract_tree(
                dir.path(),
                "local/sites",
                "workdir",
                &[],
                &std::collections::BTreeMap::new(),
                None,
                None,
                &[],
            )
            .unwrap();
            let mut store = SqliteGraphStore::open_in_memory().unwrap();
            crate::load_into_graph(
                &mut store,
                &extraction,
                "local/sites",
                dir.path(),
                "workdir",
            )
            .unwrap();
            (extraction, store)
        };
        let calls = |edges: &[Edge], dst: &str| {
            edges
                .iter()
                .filter(|edge| {
                    edge.label == "CALLS" && edge.src.ends_with("#run") && edge.dst.ends_with(dst)
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        let prov = |edge: &Edge| {
            serde_json::from_value::<core_prov::Provenance>(edge.props["prov"].clone()).unwrap()
        };
        let (extraction, store) = ingest();
        let sites = calls(&extraction.edges, "#helper");
        assert_eq!(sites.len(), 2, "fixture must emit repeated call sites");
        let stored = store.all_edges().unwrap();
        let [helper] = calls(&stored, "#helper").try_into().unwrap();
        let helper_prov = prov(&helper);
        let mut expected = sites
            .iter()
            .flat_map(|site| prov(site).evidence)
            .collect::<Vec<_>>();
        expected.sort_by_key(|span| span.byte_start);
        assert_eq!(helper_prov.evidence, expected);
        assert!(helper_prov.evidence[0].byte_start < helper_prov.evidence[1].byte_start);
        // Tier and confidence stay those of the relation itself.
        assert_eq!(helper_prov.tier, prov(&sites[1]).tier);
        assert_eq!(helper_prov.confidence_tier, prov(&sites[1]).confidence_tier);
        // The hash covers every cited site, not just the last one.
        assert!(
            sites
                .iter()
                .all(|site| prov(site).content_hash != helper_prov.content_hash)
        );
        let [other] = calls(&stored, "#other").try_into().unwrap();
        assert_eq!(other, calls(&extraction.edges, "#other")[0]);
        // Deterministic: a second ingest stores identical edges and hashes.
        let (_, again) = ingest();
        assert_eq!(again.all_edges().unwrap(), stored);
        assert_eq!(
            crate::deterministic_graph_hashes(&again).unwrap(),
            crate::deterministic_graph_hashes(&store).unwrap()
        );
    }

    #[test]
    fn collapsed_relation_evidence_is_bounded_and_keeps_its_confidence() {
        // AC-0203 (#436): evidence is sorted, de-duplicated and capped with
        // the overflow recorded; an occurrence at another confidence tier
        // never lends its span to the stored relation.
        let site = |start: u64, confidence: core_prov::ConfidenceTier| Edge {
            src: "sym:r@a.ts#run".into(),
            dst: "sym:r@b.ts#helper".into(),
            label: "CALLS".into(),
            props: serde_json::json!({
                "prov": core_prov::Provenance::new(
                    core_prov::Tier::Deterministic,
                    confidence,
                    vec![core_prov::EvidenceRef {
                        repo: "r".into(),
                        path: "a.ts".into(),
                        byte_start: start,
                        byte_end: start + 1,
                        commit_sha: "c".into(),
                    }],
                    "t0.test",
                    format!("CALLS at {start}").as_bytes(),
                )
                .unwrap(),
            }),
        };
        let confirmed = core_prov::ConfidenceTier::Confirmed;
        let over = crate::MAX_MERGED_EDGE_EVIDENCE as u64 + 3;
        let mut edges = (0..over)
            .rev()
            .map(|start| site(start, confirmed))
            .collect::<Vec<_>>();
        edges.push(site(0, confirmed)); // exact duplicate span
        edges.push(site(1_000, core_prov::ConfidenceTier::Gap));
        edges.push(site(5, confirmed)); // last occurrence supplies the props
        let merged = crate::merge_edge_occurrences(&edges);
        assert_eq!(merged.len(), 1);
        let edge = merged.values().next().unwrap();
        let prov =
            serde_json::from_value::<core_prov::Provenance>(edge.props["prov"].clone()).unwrap();
        assert_eq!(prov.confidence_tier, confirmed);
        assert_eq!(
            prov.evidence
                .iter()
                .map(|span| span.byte_start)
                .collect::<Vec<_>>(),
            (0..crate::MAX_MERGED_EDGE_EVIDENCE as u64).collect::<Vec<_>>()
        );
        assert_eq!(edge.props["evidence_omitted"], 3);
        // Occurrence order does not change the merged fact.
        edges.rotate_left(4);
        edges.push(site(5, confirmed));
        assert_eq!(
            crate::merge_edge_occurrences(&edges).values().next(),
            Some(edge)
        );
    }

    #[test]
    fn collapsed_relation_hash_tracks_sites_that_share_one_fact_hash() {
        // AC-0203 (#456 review): duplicate imports hash the same fact bytes
        // at every site. The merged hash must still change when a site is
        // added, removed or moved, while a single site keeps its own hash.
        let site = |start: u64| Edge {
            src: "file:r@a.ts".into(),
            dst: "file:r@b.ts".into(),
            label: "IMPORTS".into(),
            props: serde_json::json!({
                "prov": core_prov::Provenance::new(
                    core_prov::Tier::Deterministic,
                    core_prov::ConfidenceTier::Confirmed,
                    vec![core_prov::EvidenceRef {
                        repo: "r".into(),
                        path: "a.ts".into(),
                        byte_start: start,
                        byte_end: start + 10,
                        commit_sha: "c".into(),
                    }],
                    "t0.test",
                    b"IMPORTS ./b",
                )
                .unwrap(),
            }),
        };
        let hash = |starts: &[u64]| {
            let edges = starts.iter().map(|start| site(*start)).collect::<Vec<_>>();
            let merged = crate::merge_edge_occurrences(&edges);
            let edge = merged.values().next().unwrap();
            edge.props["prov"]["content_hash"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let single = hash(&[0]);
        assert_eq!(single, site(0).props["prov"]["content_hash"]);
        let two = hash(&[0, 40]);
        assert_ne!(two, single);
        assert_ne!(hash(&[0, 40, 80]), two); // site added
        assert_ne!(hash(&[0, 60]), two); // site moved
        assert_eq!(hash(&[40, 0]), two); // order-independent
        assert_eq!(hash(&[0, 40, 40]), two); // exact duplicate span
        // Spans beyond the bound still count toward the hash.
        let bound = crate::MAX_MERGED_EDGE_EVIDENCE as u64;
        let full = (0..bound).map(|n| n * 20).collect::<Vec<_>>();
        let mut over = full.clone();
        over.push(bound * 20);
        assert_ne!(hash(&over), hash(&full));
    }

    #[test]
    fn system_summary_counts_a_channel_shared_by_two_repos_once() {
        // AC-0195 (#242 review): a channel is keyed globally
        // (`chan:{kind}:{identity}`), so two repos that publish to one queue
        // share one stored fact. Summing per-repo counts would count it
        // twice; the operation reports it once and states the repeat.
        let dir = tempfile::tempdir().unwrap();
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        let mut published = crate::OperationFacts::default();
        let mut per_repo_sum = (0u64, 0u64);
        let mut channel_ids = Vec::new();
        for repo in ["orders", "billing"] {
            let root = dir.path().join(repo);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join("app.ts"),
                r#"
import { SQSClient, SendMessageCommand } from '@aws-sdk/client-sqs';
const sqs = new SQSClient({});
export function queue() {
  return sqs.send(new SendMessageCommand({ QueueUrl: 'https://sqs.us-east-1.amazonaws.com/9/orders', MessageBody: '{}' }));
}
"#,
            )
            .unwrap();
            let identity = format!("local/{repo}");
            let extraction = crate::extract_tree(
                &root,
                &identity,
                "workdir",
                &[],
                &std::collections::BTreeMap::new(),
                None,
                None,
                &[],
            )
            .unwrap();
            channel_ids.extend(
                extraction
                    .nodes
                    .iter()
                    .filter(|node| node.label == "Channel")
                    .map(|node| node.id.clone()),
            );
            let loaded =
                crate::load_into_graph(&mut store, &extraction, &identity, &root, "workdir")
                    .unwrap()
                    .published;
            per_repo_sum.0 += loaded.nodes.len() as u64;
            per_repo_sum.1 += loaded.edges.len() as u64;
            published.record_load(loaded);
        }
        crate::stitch_backings(&mut store, &mut published).unwrap();

        assert_eq!(channel_ids.len(), 2, "each repo emits the shared channel");
        assert_eq!(channel_ids[0], channel_ids[1]);
        assert_eq!(
            store.fact_counts().unwrap(),
            (published.nodes(), published.edges())
        );
        assert!(
            per_repo_sum.0 > published.nodes(),
            "a per-repo sum double counts"
        );
        assert!(published.merged.nodes >= 1);
    }

    #[test]
    fn operation_facts_follow_a_whole_graph_patch() {
        // AC-0195 (#242 review): found-ADR relinking retracts and re-adds
        // facts after the load. The operation mirrors the store's patch
        // order, so a retracted node takes its incident edges with it and a
        // re-published key is neither lost nor counted as merged.
        let key = |src: &str, dst: &str| (src.to_string(), dst.to_string(), "DECIDES".to_string());
        let mut published = crate::OperationFacts::default();
        published.record_load(crate::PublishedFacts {
            nodes: ["adr:a", "adr:stale", "sym:x"].map(String::from).into(),
            edges: [key("adr:a", "sym:x"), key("adr:stale", "sym:x")].into(),
            merged: crate::MergedFacts::default(),
        });
        let node = |id: &str| Node {
            id: id.into(),
            label: "ADR".into(),
            props: serde_json::json!({}),
        };
        published.record_patch(&core_graph::GraphPatch {
            upsert_nodes: vec![node("adr:a"), node("adr:b")],
            upsert_edges: vec![Edge {
                src: "adr:b".into(),
                dst: "sym:x".into(),
                label: "DECIDES".into(),
                props: serde_json::json!({}),
            }],
            delete_node_ids: vec!["adr:stale".into()],
            delete_edges: vec![key("adr:a", "sym:x")],
        });
        assert_eq!(
            published.nodes,
            ["adr:a", "adr:b", "sym:x"].map(String::from).into()
        );
        assert_eq!(published.edges, [key("adr:b", "sym:x")].into());
        assert_eq!(published.merged, crate::MergedFacts::default());
    }

    #[test]
    fn ingest_summary_includes_state_backed_backs_edges() {
        // AC-0195 (#242 review): BACKS edges are stitched after the load,
        // from observed state. The summary is taken after that stage, so it
        // still equals the store's counts.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.tf"),
            "resource \"aws_sqs_queue\" \"orders\" {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("app.ts"),
            r#"
import { SQSClient, SendMessageCommand } from '@aws-sdk/client-sqs';
const sqs = new SQSClient({});
export function queueOrder() {
  return sqs.send(new SendMessageCommand({ QueueUrl: 'https://sqs.us-east-1.amazonaws.com/9/orders', MessageBody: '{}' }));
}
"#,
        )
        .unwrap();
        let state = dir.path().join("shop.state.json");
        std::fs::write(
            &state,
            r#"{
  "format_version": "1.0",
  "values": { "root_module": { "resources": [{
    "address": "aws_sqs_queue.orders", "mode": "managed",
    "type": "aws_sqs_queue", "name": "orders",
    "values": { "url": "https://sqs.us-east-1.amazonaws.com/9/orders" },
    "sensitive_values": {}
  }] } }
}"#,
        )
        .unwrap();
        let extraction = crate::extract_tree(
            dir.path(),
            "local/shop",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            Some(&state),
            None,
            &[],
        )
        .unwrap();
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        let mut published = crate::OperationFacts::default();
        published.record_load(
            crate::load_into_graph(&mut store, &extraction, "local/shop", dir.path(), "workdir")
                .unwrap()
                .published,
        );
        let loaded_edges = published.edges();

        assert_eq!(
            crate::stitch_backings(&mut store, &mut published).unwrap(),
            1
        );
        assert_eq!(published.edges(), loaded_edges + 1);
        assert_eq!(
            store.fact_counts().unwrap(),
            (published.nodes(), published.edges())
        );
    }

    #[test]
    fn toolchain_facts_land_in_the_graph_with_defined_in_edges() {
        // AC-0096 (#215): config files become Tool nodes with cited
        // settings, DEFINED_IN the config File node — end to end through
        // the extraction pipeline and the FK-enforcing graph load.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{ "name": "shop", "type": "module", "dependencies": { "react": "^19" } }"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("app.ts"), "export function run() {}\n").unwrap();
        // The vite config is a .ts source file: the TS adapter owns its
        // File node, and the toolchain must reuse it rather than clobber.
        std::fs::write(
            dir.path().join("vite.config.ts"),
            "export default { base: '/' };\n",
        )
        .unwrap();
        let (extraction, summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/tools",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert!(summary.tools.files >= 2);
        assert!(summary.tools.nodes > 0);

        let mut graph = SqliteGraphStore::open_in_memory().unwrap();
        crate::load_into_graph(
            &mut graph,
            &extraction,
            "local/tools",
            dir.path(),
            "workdir",
        )
        .unwrap();
        let react = graph.get_node("tool:local/tools@react").unwrap().unwrap();
        assert_eq!(react.label, "Tool");
        assert_eq!(react.props["settings"]["requirement"], "^19");
        let edges = graph.all_edges().unwrap();
        assert!(edges.iter().any(|edge| edge.src == "tool:local/tools@react"
            && edge.dst == "file:local/tools@package.json"
            && edge.label == "DEFINED_IN"));
        // Presence-only code config: Tool node exists, DEFINED_IN targets
        // the TS adapter's own File node (no duplicate/clobbered file).
        let vite = graph
            .get_node("tool:local/tools@vite.config.ts")
            .unwrap()
            .unwrap();
        assert_eq!(vite.props["settings_behind_code"], true);
        let vite_file = graph
            .get_node("file:local/tools@vite.config.ts")
            .unwrap()
            .unwrap();
        assert!(
            vite_file.props.get("config").is_none(),
            "adapter-owned File node survives the toolchain pass"
        );
        assert!(
            edges
                .iter()
                .any(|edge| edge.src == "tool:local/tools@vite.config.ts"
                    && edge.dst == "file:local/tools@vite.config.ts"
                    && edge.label == "DEFINED_IN")
        );
    }

    #[test]
    fn webextension_manifest_ingest_reports_layer_and_security_grants() {
        // US-0016: a Manifest V3 extension yields deterministic topology,
        // entry bindings against .ts sources, and exact-scope GRANTS that
        // the security projection can flag — reported as its own layer.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/background")).unwrap();
        std::fs::write(
            dir.path().join("manifest.json"),
            r#"{
  "manifest_version": 3,
  "name": "Image Trail",
  "version": "0.10.1",
  "background": { "service_worker": "src/background/service-worker.js" },
  "permissions": ["activeTab", "storage"],
  "optional_host_permissions": ["http://*/*"]
}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/background/service-worker.ts"),
            "export function main(): void {}\n",
        )
        .unwrap();

        let (extraction, summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/image-trail",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();

        assert_eq!(summary.webext.files, 1);
        assert!(summary.webext.nodes > 0);
        let ext = extraction
            .nodes
            .iter()
            .find(|node| node.label == "Extension")
            .expect("extension node");
        assert_eq!(ext.props["prov"]["extractor_id"], "t0.webextension");
        // The declared .js entry binds to the extracted .ts File node —
        // one node id shared between the manifest fact and the TS pass.
        assert!(extraction.edges.iter().any(|edge| {
            edge.label == "ENTRY"
                && edge.dst == "file:local/image-trail@src/background/service-worker.ts"
        }));
        assert_eq!(
            extraction
                .nodes
                .iter()
                .filter(|node| node.id == "file:local/image-trail@src/background/service-worker.ts")
                .count(),
            1,
            "manifest binding must reuse the TS pass's File node, not duplicate it"
        );
        // Wildcard host scope is an exact, projectable GRANTS fact.
        let grant = extraction
            .edges
            .iter()
            .find(|edge| edge.label == "GRANTS" && edge.dst.ends_with("host:http://*/*"))
            .expect("host grant");
        assert_eq!(grant.props["resource_scopes"][0], "http://*/*");
    }

    #[test]
    fn chrome_messaging_stitches_channels_across_extension_contexts() {
        // US-0016/AC-0072: literal + const-map message identities become
        // Confirmed chrome-message channels with PUBLISHES/SUBSCRIBES edges
        // connecting content script and service worker; a runtime-computed
        // identity stays an explicit Gap.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/protocol.ts"),
            "export const MessageType = { Capture: 'ext.capture' } as const;\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/content.ts"),
            "import { MessageType } from './protocol.js';\n\
             export function capture(kind: string) {\n\
               void chrome.runtime.sendMessage({ type: MessageType.Capture });\n\
               void chrome.runtime.sendMessage({ type: `ext.${kind}` });\n\
             }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/worker.ts"),
            "import { MessageType } from './protocol.js';\n\
             export const handlers = {\n\
               [MessageType.Capture]: () => 'ok',\n\
             };\n\
             chrome.runtime.onMessage.addListener(() => true);\n",
        )
        .unwrap();

        let (extraction, _) = crate::extract_tree_with_summary(
            dir.path(),
            "local/ext",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();

        let channel = extraction
            .nodes
            .iter()
            .find(|node| node.id == "chan:chrome-message:ext.capture")
            .expect("confirmed chrome-message channel");
        assert_eq!(channel.props["prov"]["confidence_tier"], "Confirmed");
        assert!(extraction.edges.iter().any(|edge| {
            edge.label == "PUBLISHES"
                && edge.src == "sym:local/ext@src/content.ts#capture"
                && edge.dst == channel.id
        }));
        assert!(extraction.edges.iter().any(|edge| {
            edge.label == "SUBSCRIBES"
                && edge.src == "file:local/ext@src/worker.ts"
                && edge.dst == channel.id
        }));
        // The template-string identity bottoms out as an explicit Gap.
        assert!(extraction.nodes.iter().any(|node| {
            node.label == "Gap"
                && node.props["kind"] == "chrome-message"
                && node.props["reason"] == "runtime-computed channel identity"
        }));
    }

    #[test]
    fn indexeddb_data_model_joins_the_graph() {
        // US-0016/AC-0073: store declarations plus repository operations
        // yield a cited DataEntity with READS/WRITES relations.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/schema.ts"),
            "export const DataStore = { History: 'history' } as const;\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/repo.ts"),
            "import { DataStore } from './schema.js';\n\
             export function save(tx: IDBTransaction, record: unknown) {\n\
               tx.objectStore(DataStore.History).put(record);\n\
             }\n",
        )
        .unwrap();

        let (extraction, _) = crate::extract_tree_with_summary(
            dir.path(),
            "local/ext",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();

        let entity = extraction
            .nodes
            .iter()
            .find(|node| node.id == "data:local/ext@idb:history")
            .expect("data entity");
        assert_eq!(entity.props["prov"]["confidence_tier"], "Confirmed");
        assert!(extraction.edges.iter().any(|edge| {
            edge.label == "WRITES"
                && edge.src == "sym:local/ext@src/repo.ts#save"
                && edge.dst == entity.id
        }));
    }

    #[test]
    fn webextension_dogfood_compiles_a_useful_deterministic_spec() {
        // US-0016/AC-0074: a full fixture extension — manifest, messaging,
        // IndexedDB — compiles into artifacts with useful cited assertions,
        // and the verified-only bundle is byte-identical across repeat
        // ingest of the same tree.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/background")).unwrap();
        std::fs::create_dir_all(dir.path().join("src/content")).unwrap();
        std::fs::write(
            dir.path().join("manifest.json"),
            r#"{
  "manifest_version": 3,
  "name": "Fixture Trail",
  "version": "1.0.0",
  "action": { "default_title": "Toggle" },
  "background": { "service_worker": "src/background/worker.js" },
  "permissions": ["storage"],
  "optional_host_permissions": ["http://*/*"]
}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/protocol.ts"),
            "export const MessageType = { Capture: 'ext.capture' } as const;\n\
             export const DataStore = { History: 'history' } as const;\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/content/content.ts"),
            "import { MessageType } from '../protocol.js';\n\
             export function capture(kind: string) {\n\
               void chrome.runtime.sendMessage({ type: MessageType.Capture });\n\
               void chrome.runtime.sendMessage({ type: `ext.${kind}` });\n\
             }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/background/worker.ts"),
            "import { MessageType, DataStore } from '../protocol.js';\n\
             export function persist(tx: IDBTransaction, record: unknown) {\n\
               tx.objectStore(DataStore.History).put(record);\n\
             }\n\
             export const handlers = {\n\
               [MessageType.Capture]: () => 'ok',\n\
             };\n\
             chrome.runtime.onMessage.addListener(() => true);\n",
        )
        .unwrap();

        let compile = || {
            let (extraction, _) = crate::extract_tree_with_summary(
                dir.path(),
                "local/fixture-trail",
                "workdir",
                &[],
                &std::collections::BTreeMap::new(),
                None,
                None,
                &[],
            )
            .unwrap();
            spec::compile_spec(
                &extraction.nodes,
                &extraction.edges,
                &[],
                spec::ExportMode::VerifiedOnly,
                &std::collections::BTreeSet::new(),
            )
        };
        let bundle = compile();

        let artifact = |name: &str| {
            bundle
                .artifacts
                .iter()
                .find(|artifact| artifact.file_name == name)
                .unwrap_or_else(|| panic!("{name} artifact"))
        };
        // Data model: the IndexedDB entity with its cited WRITES relation.
        let data = artifact("data_model.md");
        assert!(
            data.content
                .contains("data:local/fixture-trail@idb:history"),
            "data model names the store: {}",
            data.content
        );
        // Security view: the wildcard optional host permission is a finding.
        let security = artifact("security.md");
        assert!(
            security.content.contains("http://*/*"),
            "wildcard host grant projected: {}",
            security.content
        );
        // Gap register: the runtime-computed message identity is explicit.
        let gaps = artifact("gap_register.md");
        assert!(
            gaps.content.contains("runtime-computed channel identity"),
            "computed message identity is an explicit gap: {}",
            gaps.content
        );
        assert!(bundle.assertion_count > 0);

        // Determinism: a second independent ingest compiles byte-identically.
        let again = compile();
        assert_eq!(
            serde_json::to_string(&bundle).unwrap(),
            serde_json::to_string(&again).unwrap(),
            "verified-only bundle must be identical across repeat ingest"
        );
    }

    #[test]
    #[ignore = "manual dogfood (MT-DF-01): set CARTOGRAPH_DOGFOOD_ROOT to an image-trail checkout"]
    fn dogfood_extraction_against_local_image_trail_checkout() {
        let root = std::env::var("CARTOGRAPH_DOGFOOD_ROOT").expect("checkout path");
        let root = std::path::Path::new(&root);
        let compile = || {
            let (extraction, summary) = crate::extract_tree_with_summary(
                root,
                "qwts/image-trail",
                "workdir",
                &[],
                &std::collections::BTreeMap::new(),
                None,
                None,
                &[],
            )
            .unwrap();
            (
                spec::compile_spec(
                    &extraction.nodes,
                    &extraction.edges,
                    &[],
                    spec::ExportMode::VerifiedOnly,
                    &std::collections::BTreeSet::new(),
                ),
                extraction,
                summary,
            )
        };
        let (bundle, extraction, summary) = compile();
        let count = |label: &str| {
            extraction
                .nodes
                .iter()
                .filter(|node| node.label == label)
                .count()
        };
        println!(
            "layers: ts {} files / webext {} manifests · nodes {} edges {}",
            summary.ts.files,
            summary.webext.files,
            extraction.nodes.len(),
            extraction.edges.len()
        );
        println!(
            "extension {} · contexts {} · commands {} · permissions {} · channels {} · data entities {} · gaps {}",
            count("Extension"),
            count("ExtensionContext"),
            count("Command"),
            count("Permission"),
            count("Channel"),
            count("DataEntity"),
            count("Gap"),
        );
        for artifact in &bundle.artifacts {
            println!(
                "{}: {} assertions",
                artifact.file_name,
                artifact.assertions.len()
            );
        }
        assert!(summary.webext.files >= 1, "manifest recognized");
        assert!(count("Extension") >= 1);
        assert!(count("ExtensionContext") >= 1);
        assert!(count("Permission") >= 1);
        assert!(count("DataEntity") >= 1, "IndexedDB stores recovered");
        // Determinism against the real tree, not only the fixture.
        let (again, _, _) = compile();
        assert_eq!(
            serde_json::to_string(&bundle).unwrap(),
            serde_json::to_string(&again).unwrap(),
        );
    }

    #[test]
    fn python_server_ingest_reports_layer_and_endpoints() {
        // AC-0053/T-0053: the app runs the import-proven Python pass for the
        // server layer and reports it independently from TypeScript.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("api.py"),
            "from fastapi import FastAPI\n\napp = FastAPI()\n\n@app.get('/orders')\ndef orders():\n    return []\n",
        )
        .unwrap();

        let (extraction, summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/python-app",
            "workdir",
            &["server".into()],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();

        assert_eq!(summary.python.files, 1);
        assert!(summary.python.nodes > 0);
        assert_eq!(summary.ts, crate::LayerSummary::default());
        assert_eq!(summary.tf, crate::LayerSummary::default());
        let endpoint = extraction
            .nodes
            .iter()
            .find(|node| node.id == "ep:local/python-app@GET:/orders")
            .expect("FastAPI endpoint");
        assert_eq!(endpoint.props["language"], "python");
        assert_eq!(endpoint.props["framework"], "fastapi");
        assert_eq!(endpoint.props["prov"]["extractor_id"], "t0.adapter-python");
        assert!(extraction.edges.iter().any(|edge| {
            edge.label == "HANDLES"
                && edge.src == endpoint.id
                && edge.dst == "sym:local/python-app@api.py#orders"
        }));

        let (client_only, client_summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/python-app",
            "workdir",
            &["client".into()],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(client_summary.python, crate::LayerSummary::default());
        assert!(
            client_only
                .nodes
                .iter()
                .all(|node| node.props["language"] != "python")
        );
    }

    #[test]
    fn go_server_ingest_reports_layer_and_endpoints() {
        // AC-0054/T-0054: the app runs Go only for server scope and reports
        // its facts independently from the other deterministic languages.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module example.com/api\n").unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nimport \"net/http\"\n\nfunc orders(w http.ResponseWriter, r *http.Request) {}\nfunc routes() { http.HandleFunc(\"GET /orders\", orders) }\n",
        )
        .unwrap();

        let (extraction, summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/go-app",
            "workdir",
            &["server".into()],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(summary.go.files, 1);
        assert!(summary.go.nodes > 0);
        assert_eq!(summary.ts, crate::LayerSummary::default());
        assert_eq!(summary.python, crate::LayerSummary::default());
        assert_eq!(summary.tf, crate::LayerSummary::default());
        let endpoint = extraction
            .nodes
            .iter()
            .find(|node| node.id == "ep:local/go-app@GET:/orders")
            .expect("net/http endpoint");
        assert_eq!(endpoint.props["language"], "go");
        assert_eq!(endpoint.props["framework"], "net/http");
        assert_eq!(endpoint.props["prov"]["extractor_id"], "t0.adapter-go");

        let (client_only, client_summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/go-app",
            "workdir",
            &["client".into()],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(client_summary.go, crate::LayerSummary::default());
        assert!(
            client_only
                .nodes
                .iter()
                .all(|node| node.props["language"] != "go")
        );
    }

    #[test]
    fn java_server_ingest_reports_layer_and_endpoints() {
        // AC-0079/AC-0080: the app runs the annotation-proven Java pass for
        // the server layer and reports it independently.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/com/demo")).unwrap();
        std::fs::write(
            dir.path().join("src/com/demo/UserController.java"),
            "package com.demo;\n\nimport org.springframework.web.bind.annotation.*;\n\n@RestController\n@RequestMapping(\"/api\")\npublic class UserController {\n    @GetMapping(\"/users\")\n    public String users() { return \"[]\"; }\n}\n",
        )
        .unwrap();

        let (extraction, summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/java-app",
            "workdir",
            &["server".into()],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(summary.java.files, 1);
        assert!(summary.java.nodes > 0);
        assert_eq!(summary.ts, crate::LayerSummary::default());
        let endpoint = extraction
            .nodes
            .iter()
            .find(|node| node.id == "ep:local/java-app@GET:/api/users")
            .expect("Spring endpoint");
        assert_eq!(endpoint.props["language"], "java");
        assert_eq!(endpoint.props["framework"], "spring");
        assert_eq!(endpoint.props["prov"]["extractor_id"], "t0.adapter-java");
        assert!(extraction.edges.iter().any(|edge| {
            edge.label == "HANDLES"
                && edge.src == endpoint.id
                && edge.dst
                    == "sym:local/java-app@src/com/demo/UserController.java#UserController.users"
        }));

        let (client_only, client_summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/java-app",
            "workdir",
            &["client".into()],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(client_summary.java, crate::LayerSummary::default());
        assert!(
            client_only
                .nodes
                .iter()
                .all(|node| node.props["language"] != "java")
        );
    }

    #[test]
    fn kotlin_server_ingest_reports_layer_and_endpoints() {
        // AC-0098: the app runs the annotation-proven Kotlin pass for the
        // server layer and reports it independently.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/com/demo")).unwrap();
        std::fs::write(
            dir.path().join("src/com/demo/UserController.kt"),
            "package com.demo\n\nimport org.springframework.web.bind.annotation.*\n\n@RestController\n@RequestMapping(\"/api\")\nclass UserController {\n    @GetMapping(\"/users\")\n    fun users(): String = \"[]\"\n}\n",
        )
        .unwrap();

        let (extraction, summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/kotlin-app",
            "workdir",
            &["server".into()],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(summary.kotlin.files, 1);
        assert!(summary.kotlin.nodes > 0);
        assert_eq!(summary.ts, crate::LayerSummary::default());
        let endpoint = extraction
            .nodes
            .iter()
            .find(|node| node.id == "ep:local/kotlin-app@GET:/api/users")
            .expect("Spring endpoint");
        assert_eq!(endpoint.props["language"], "kotlin");
        assert_eq!(endpoint.props["framework"], "spring");
        assert_eq!(endpoint.props["prov"]["extractor_id"], "t0.adapter-kotlin");
        assert!(extraction.edges.iter().any(|edge| {
            edge.label == "HANDLES"
                && edge.src == endpoint.id
                && edge.dst
                    == "sym:local/kotlin-app@src/com/demo/UserController.kt#UserController.users"
        }));

        let (client_only, client_summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/kotlin-app",
            "workdir",
            &["client".into()],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(client_summary.kotlin, crate::LayerSummary::default());
        assert!(
            client_only
                .nodes
                .iter()
                .all(|node| node.props["language"] != "kotlin")
        );
    }

    #[test]
    fn system_contents_lists_each_repo_from_graph_facts() {
        // AC-0085: composition comes from the graph's own evidence —
        // distinct repos with their recorded commit identity, sorted, one
        // entry per repo no matter how many facts it contributed. Evidence
        // (not File ids) is the source, so an infra-only repo whose
        // extractor emits Resource nodes still counts (#187 review).
        let fact = |id: &str, label: &str, repo: &str, path: &str, commit: &str| Node {
            id: id.into(),
            label: label.into(),
            props: serde_json::json!({
                "prov": { "evidence": [{ "repo": repo, "path": path, "commit_sha": commit }] },
            }),
        };
        let files = vec![
            fact(
                "res:aws_sqs_queue.orders",
                "Resource",
                "local/infra",
                "main.tf",
                "workdir",
            ),
            fact(
                "file:acme/shop@src/app.ts",
                "File",
                "acme/shop",
                "src/app.ts",
                "a1b2c3d",
            ),
            fact(
                "sym:app.ts#main",
                "Symbol",
                "acme/shop",
                "src/app.ts",
                "a1b2c3d",
            ),
            // Synthetic nodes without evidence assert nothing about repos.
            Node {
                id: "gap:x".into(),
                label: "Gap".into(),
                props: serde_json::json!({ "reason": "r" }),
            },
        ];
        let contents = crate::system_contents_of(&files);
        assert_eq!(
            contents,
            [
                crate::SystemRepo {
                    repo: "acme/shop".into(),
                    display_name: None,
                    commit: "a1b2c3d".into(),
                },
                crate::SystemRepo {
                    repo: "local/infra".into(),
                    display_name: None,
                    commit: "workdir".into(),
                },
            ]
        );

        // An empty graph names nothing — the UI states the system is empty.
        assert!(crate::system_contents_of(&[]).is_empty());
    }

    #[test]
    fn clear_graph_preserves_job_spine() {
        // AC-0050: only disposable graph facts are cleared; durable jobs live
        // in their separate state-spine database and remain untouched.
        let dir = tempfile::tempdir().unwrap();
        let mut graph = SqliteGraphStore::open(dir.path().join("graph.db")).unwrap();
        graph
            .put_node(&Node {
                id: "a".into(),
                label: "Resource".into(),
                props: serde_json::json!({}),
            })
            .unwrap();
        graph
            .put_node(&Node {
                id: "b".into(),
                label: "Resource".into(),
                props: serde_json::json!({}),
            })
            .unwrap();
        graph
            .put_edge(&Edge {
                src: "a".into(),
                dst: "b".into(),
                label: "REFERENCES".into(),
                props: serde_json::json!({}),
            })
            .unwrap();
        let mut jobs = crate::jobs::JobStore::open(dir.path().join("state.db")).unwrap();
        let job = jobs.enqueue("ingest:fixture").unwrap();

        let stats = crate::clear_graph_store(&mut graph).unwrap();
        assert_eq!(stats.nodes, 0);
        assert_eq!(stats.edges, 0);
        assert_eq!(jobs.list().unwrap()[0].id, job.id);
    }

    #[test]
    fn clear_finished_jobs_removes_terminal_rows_only() {
        // AC-0076: clearing finished jobs deletes done/failed/cancelled rows
        // while queued, running, and interrupted (resumable) jobs survive —
        // and graph facts are untouched (jobs live on their own spine).
        let dir = tempfile::tempdir().unwrap();
        let mut graph = SqliteGraphStore::open(dir.path().join("graph.db")).unwrap();
        graph
            .put_node(&Node {
                id: "a".into(),
                label: "Resource".into(),
                props: serde_json::json!({}),
            })
            .unwrap();
        let mut jobs = crate::jobs::JobStore::open(dir.path().join("state.db")).unwrap();
        let mut with_status = |status: &str| {
            let job = jobs.enqueue(&format!("ingest:{status}")).unwrap();
            if status != "queued" {
                jobs.set_status(job.id, status).unwrap();
            }
            job.id
        };
        for status in ["done", "failed", "cancelled"] {
            with_status(status);
        }
        let kept: Vec<i64> = ["queued", "running", "interrupted"]
            .into_iter()
            .map(&mut with_status)
            .collect();

        assert_eq!(jobs.clear_finished().unwrap(), 3);
        let remaining: Vec<i64> = jobs.list().unwrap().into_iter().map(|j| j.id).collect();
        assert_eq!(remaining.len(), 3);
        for id in kept {
            assert!(remaining.contains(&id));
        }
        // A second clear is a no-op, and the graph never lost its fact.
        assert_eq!(jobs.clear_finished().unwrap(), 0);
        assert_eq!(graph.node_count().unwrap(), 1);
    }

    #[test]
    fn clearing_a_live_cancelled_job_still_stops_the_worker() {
        // #157 review (P1): a cancelled job can be cleared while its worker
        // is still between cancellation checks. The guard must fail closed —
        // a missing row stops the exact execution, instead of
        // continuing to write graph facts after cancellation.
        let dir = tempfile::tempdir().unwrap();
        let state = super::registered_source_tests::app_state(dir.path());
        let (job, execution) = super::start_job(&state, "ingest:/big").unwrap();
        state.jobs.lock().unwrap().cancel(job.id).unwrap();
        assert_eq!(state.jobs.lock().unwrap().clear_finished().unwrap(), 1);
        assert_eq!(
            super::job_cancelled(&state, &execution).unwrap_err(),
            super::JobTransitionError::Missing.to_string()
        );
    }

    #[test]
    fn cancelled_add_jobs_never_flip_back_to_done() {
        // #166 review: with the UI responsive during an add, Cancel can race
        // the worker. The terminal transition must be atomic — a cancelled
        // job stays cancelled and the pipeline reports it, never a success.
        let dir = tempfile::tempdir().unwrap();
        let state = super::registered_source_tests::app_state(dir.path());
        let (job, execution) = super::start_job(&state, "add-repo:https://example.test/a").unwrap();
        state.jobs.lock().unwrap().cancel(job.id).unwrap();
        // The worker's completion attempt must not overwrite the cancel.
        let after = super::updated_job(
            state
                .jobs
                .lock()
                .unwrap()
                .finish_execution(&execution, &[])
                .unwrap(),
        );
        assert_eq!(after.status, "cancelled");

        // And an uncancelled run completes normally through the same path.
        let (_, execution) = super::start_job(&state, "add-repo:https://example.test/b").unwrap();
        assert_eq!(
            super::updated_job(
                state
                    .jobs
                    .lock()
                    .unwrap()
                    .finish_execution(&execution, &[])
                    .unwrap()
            )
            .status,
            "done"
        );

        let (_, failed_execution) = super::start_job(&state, "noop").unwrap();
        let failed = super::updated_job(
            state
                .jobs
                .lock()
                .unwrap()
                .fail_execution(&failed_execution, "fixture failure")
                .unwrap(),
        );
        assert_ne!(super::completed_job(&failed).unwrap_err(), "cancelled");
    }

    #[test]
    fn heavy_ingest_commands_run_off_the_calling_thread() {
        // AC-0078: every recovery command hops through off_ui_thread before
        // touching the pipeline — the invoking (webview/main) thread never
        // executes extraction, so a large repo cannot freeze the app.
        let caller = std::thread::current().id();
        let worker = tauri::async_runtime::block_on(crate::off_ui_thread(move || {
            Ok(std::thread::current().id())
        }))
        .unwrap();
        assert_ne!(caller, worker);
        // Errors from the pipeline pass through unchanged.
        let err = tauri::async_runtime::block_on(crate::off_ui_thread(|| {
            Err::<(), String>("boom".into())
        }));
        assert_eq!(err, Err("boom".into()));
    }

    #[test]
    fn state_json_resolves_from_manifest_directory_for_both_input_forms() {
        // AC-0009: observed state is relative to the topology manifest,
        // whether add_system receives that manifest file or its directory.
        let dir = tempfile::tempdir().unwrap();
        let manifest_file = dir.path().join(ingest::manifest::MANIFEST_NAME);
        std::fs::write(&manifest_file, "[[repos]]\nurl = \"acme/shop\"\n").unwrap();

        assert_eq!(crate::manifest_dir(dir.path()), dir.path());
        assert_eq!(crate::manifest_dir(&manifest_file), dir.path());
        assert_eq!(
            crate::manifest_dir(dir.path()).join("state.json"),
            dir.path().join("state.json")
        );
        assert_eq!(
            crate::manifest_dir(&manifest_file).join("state.json"),
            dir.path().join("state.json")
        );
    }

    #[test]
    fn system_manifest_enriches_pulumi_resources() {
        // AC-0051/T-0051 and AC-0052/T-0052: an infra-only repo still runs
        // Pulumi-via-TS extraction, filters unrelated app facts, and overlays
        // the manifest-relative observed deployment without inventing nodes.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("infra");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("index.ts"),
            r#"
import * as aws from '@pulumi/aws';
export function applicationLookalike() { return 'not infra'; }
export const orders = new aws.sqs.Queue('orders', {});
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("stack.json"),
            r#"{"deployment":{"resources":[
              {"urn":"urn:pulumi:dev::shop::aws:sqs/queue:Queue::orders","type":"aws:sqs/queue:Queue","inputs":{},"outputs":{"url":"https://sqs.example/orders"}},
              {"urn":"urn:pulumi:dev::shop::aws:s3/bucket:Bucket::unmatched","type":"aws:s3/bucket:Bucket","inputs":{},"outputs":{}}
            ]}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join(ingest::manifest::MANIFEST_NAME),
            "[[repos]]\nurl = \"infra\"\nlayers = [\"infra\"]\npulumi_json = \"stack.json\"\n",
        )
        .unwrap();
        let manifest = ingest::manifest::SystemManifest::load(dir.path()).unwrap();
        let entry = &manifest.repos[0];
        let pulumi_path = entry.pulumi_json.as_ref().map(|path| dir.path().join(path));
        let extraction = crate::extract_tree(
            &repo,
            "local/infra",
            "workdir",
            &entry.layers,
            &manifest.env,
            None,
            pulumi_path.as_deref(),
            &[],
        )
        .unwrap();
        assert!(extraction.nodes.iter().all(|node| node.label != "Symbol"));
        let resources = extraction
            .nodes
            .iter()
            .filter(|node| node.label == "Resource")
            .collect::<Vec<_>>();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].props["logical_id"], "orders");
        assert_eq!(
            resources[0].props["observed"]["outputs"]["url"],
            "https://sqs.example/orders"
        );
        assert_eq!(
            resources[0].props["observed_prov"]["extractor_id"],
            dynamic::PULUMI_EXTRACTOR_ID
        );

        // Pulumi observation can drive the same cross-layer join as Terraform
        // state, retaining its own extractor provenance. Removing the
        // observation on re-ingest must reconcile that derived edge.
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        crate::load_into_graph(&mut store, &extraction, "local/infra", &repo, "workdir").unwrap();
        store
            .put_node(&Node {
                id: "chan:sqs-queue:https://sqs.example/orders".into(),
                label: "Channel".into(),
                props: serde_json::json!({}),
            })
            .unwrap();
        assert_eq!(
            crate::stitch_backings(&mut store, &mut crate::OperationFacts::default()).unwrap(),
            1
        );
        let backing = store.edges_with_labels(&["BACKS"]).unwrap();
        assert_eq!(backing.len(), 1);
        assert_eq!(
            backing[0].props["prov"]["extractor_id"],
            dynamic::PULUMI_EXTRACTOR_ID
        );

        let without_observation = crate::extract_tree(
            &repo,
            "local/infra",
            "workdir",
            &entry.layers,
            &manifest.env,
            None,
            None,
            &[],
        )
        .unwrap();
        crate::load_into_graph(
            &mut store,
            &without_observation,
            "local/infra",
            &repo,
            "workdir",
        )
        .unwrap();
        assert_eq!(
            crate::stitch_backings(&mut store, &mut crate::OperationFacts::default()).unwrap(),
            0
        );
        assert!(store.edges_with_labels(&["BACKS"]).unwrap().is_empty());
    }

    #[test]
    fn cloned_repo_ingests_with_real_identity() {
        // US-0001 (AC-0001): clone -> extract -> every fact carries
        // owner-ish identity + commit SHA instead of local@workdir.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src-repo");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("app.ts"),
            "import express from 'express';\nconst app = express();\napp.get('/ping', (req, res) => {});\n",
        )
        .unwrap();
        let git = |args: &[&str], cwd: &std::path::Path| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        git(&["init", "-q", "-b", "main"], &src);
        git(&["add", "."], &src);
        git(&["commit", "-q", "-m", "init"], &src);
        let bare = dir.path().join("shop.git");
        git(
            &[
                "clone",
                "-q",
                "--bare",
                src.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
            dir.path(),
        );

        let mut registry =
            super::SourceRegistry::open(dir.path().join("state.db"), dir.path()).unwrap();
        let source = registry
            .reserve_managed(&format!("file://{}", bare.display()))
            .unwrap();
        let mut operation = super::SourceOperation::acquire(
            &std::sync::Mutex::new(registry),
            [(source.clone(), true)],
        )
        .unwrap();
        let cloned = operation.clone_source(&source, None).unwrap();
        assert_eq!(cloned.repo, source.repo_key);
        assert_eq!(cloned.commit_sha.len(), 40);

        let extraction = crate::extract_tree(
            &cloned.path,
            &cloned.repo,
            &cloned.commit_sha,
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        let ep = extraction
            .nodes
            .iter()
            .find(|n| n.label == "Endpoint")
            .expect("endpoint recovered from the clone");
        let ev = &ep.props["prov"]["evidence"][0];
        assert_eq!(ev["repo"], source.repo_key);
        assert_eq!(ev["commit_sha"].as_str().unwrap(), cloned.commit_sha);

        // Repo facts retain commit, while operational roots stay in the registry.
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        crate::load_into_graph(
            &mut store,
            &extraction,
            &cloned.repo,
            &cloned.path,
            &cloned.commit_sha,
        )
        .unwrap();
        let repos = store.nodes_with_label("Repo").unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].id, format!("repo:{}", source.repo_key));
        assert!(repos[0].props.get("root").is_none());
        assert_eq!(
            repos[0].props["commit"].as_str().unwrap(),
            cloned.commit_sha
        );
    }

    #[test]
    fn identical_repos_do_not_collide_in_one_graph() {
        // US-0001 slice 2 (#1 scope note): the same relative path, route,
        // and Terraform address in two repos stay two facts — ids are
        // repo-namespaced, provenance never cross-contaminates. Channels
        // stay global: they are the cross-repo stitch points.
        let fixture = |dir: &std::path::Path| {
            std::fs::write(
                dir.join("app.ts"),
                r#"
import express from 'express';
import { EventEmitter } from 'events';
const app = express();
const bus = new EventEmitter();
app.get('/health', (req, res) => { beat(); });
export function beat() { bus.emit('heartbeat'); }
"#,
            )
            .unwrap();
            std::fs::write(dir.join("main.tf"), "resource \"aws_sqs_queue\" \"q\" {}\n").unwrap();
        };
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        fixture(a.path());
        fixture(b.path());

        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        for (dir, repo, sha) in [
            (a.path(), "acme/one", "a".repeat(40)),
            (b.path(), "acme/two", "b".repeat(40)),
        ] {
            let ex = crate::extract_tree(
                dir,
                repo,
                &sha,
                &[],
                &std::collections::BTreeMap::new(),
                None,
                None,
                &[],
            )
            .unwrap();
            crate::load_into_graph(&mut store, &ex, repo, dir, &sha).unwrap();
        }

        // Two of everything repo-scoped…
        let eps = store.nodes_with_label("Endpoint").unwrap();
        assert_eq!(eps.len(), 2);
        assert!(eps.iter().any(|e| e.id == "ep:acme/one@GET:/health"));
        assert!(eps.iter().any(|e| e.id == "ep:acme/two@GET:/health"));
        for ep in &eps {
            let ev = &ep.props["prov"]["evidence"][0];
            let repo = ev["repo"].as_str().unwrap();
            assert!(ep.id.contains(repo), "provenance matches its own repo");
        }
        assert_eq!(store.nodes_with_label("Repo").unwrap().len(), 2);
        let resources = store.nodes_with_label("Resource").unwrap();
        assert_eq!(resources.len(), 2, "same tf address, two nodes");
        // …and ONE of the global channel: both repos emit 'heartbeat', and
        // that shared identity is exactly what M5 stitches across repos.
        let chans = store.nodes_with_label("Channel").unwrap();
        assert_eq!(chans.len(), 1);
        assert_eq!(chans[0].id, "chan:inproc-event:heartbeat");
    }

    #[test]
    fn parallel_extraction_is_byte_identical_to_serial() {
        // AC-0208/T-0208 (#236): the same tree extracted with one worker and
        // with many yields byte-identical facts and an identical graph hash
        // set; parallelism changes only wall-clock time.
        let dir = tempfile::tempdir().unwrap();
        let write = |rel: &str, text: String| {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        for i in 0..24 {
            let next = (i + 1) % 24;
            write(
                &format!("web/src/m{i:02}.ts"),
                format!(
                    "import {{ f{next} }} from './m{next:02}';\n\
                     export function f{i}() {{ return f{next}(); }}\n\
                     app.get('/r{i}', f{i});\n"
                ),
            );
            write(
                &format!("svc/pkg{}/m{i:02}.py", i % 3),
                format!("def g{i}():\n    return g{i}()\n"),
            );
            write(
                &format!("go/p{}/m{i:02}.go", i % 4),
                format!("package p{}\n\nfunc H{i}() {{ H{i}() }}\n", i % 4),
            );
            write(
                &format!("jvm/src/C{i:02}.java"),
                format!("package demo;\nclass C{i:02} {{ void m() {{ m(); }} }}\n"),
            );
            write(
                &format!("jvm/src/K{i:02}.kt"),
                format!("package demo\nclass K{i:02} {{ fun m() {{ m() }} }}\n"),
            );
        }
        let extract = |workers: usize| {
            source_walk::parallel::with_workers(workers, || {
                let (extraction, layers, delta) = crate::extract_tree_incremental(
                    dir.path(),
                    "local/parallel",
                    "workdir",
                    &[],
                    &std::collections::BTreeMap::new(),
                    None,
                    None,
                    &[],
                    &mut crate::RepoExtractionCache::default(),
                    &[],
                    &mut |_| {},
                )
                .unwrap();
                let mut store = SqliteGraphStore::open_in_memory().unwrap();
                crate::load_into_graph(
                    &mut store,
                    &extraction,
                    "local/parallel",
                    dir.path(),
                    "workdir",
                )
                .unwrap();
                (
                    serde_json::to_vec(&(&extraction.nodes, &extraction.edges)).unwrap(),
                    serde_json::to_vec(&layers).unwrap(),
                    delta.recomputed_files,
                    crate::deterministic_graph_hashes(&store).unwrap(),
                )
            })
        };
        let serial = extract(1);
        assert!(serial.2 >= 120, "every fixture file is parsed");
        for workers in [2, 8] {
            let parallel = extract(workers);
            assert!(
                parallel.0 == serial.0,
                "facts differ with {workers} workers"
            );
            assert_eq!(parallel.1, serial.1);
            assert_eq!(parallel.2, serial.2);
            assert_eq!(parallel.3, serial.3);
        }
    }

    #[test]
    fn reingest_hashes_are_identical_and_delta_removes_stale_facts() {
        // AC-0039/T-0039 and AC-0040/T-0040: an unchanged tree reuses every
        // parse and yields the same ordered T0 hash set; one changed/deleted
        // file is the only parse work and its old facts cannot remain.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.ts"),
            "import { helper } from './helper';\nexport function run() { helper(); }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("helper.ts"),
            "export function helper() {}\n",
        )
        .unwrap();
        let repo = "local/delta";
        let mut cache = crate::RepoExtractionCache::default();
        let mut store = SqliteGraphStore::open_in_memory().unwrap();

        let (first, _, initial_delta) = crate::extract_tree_incremental(
            dir.path(),
            repo,
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
            &mut cache,
            &[],
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(initial_delta.recomputed_files, 2);
        crate::load_into_graph(&mut store, &first, repo, dir.path(), "workdir").unwrap();
        let initial_hashes = crate::deterministic_graph_hashes(&store).unwrap();

        let (same, _, same_delta) = crate::extract_tree_incremental(
            dir.path(),
            repo,
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
            &mut cache,
            &[],
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(same_delta.recomputed_files, 0);
        assert_eq!(same_delta.reused_files, 2);
        let reconcile =
            crate::load_into_graph(&mut store, &same, repo, dir.path(), "workdir").unwrap();
        assert_eq!(reconcile.inserted_or_updated, 0);
        assert_eq!(
            crate::deterministic_graph_hashes(&store).unwrap(),
            initial_hashes
        );

        std::fs::write(
            dir.path().join("helper.ts"),
            "export function replacement() {}\n",
        )
        .unwrap();
        let (changed, _, changed_delta) = crate::extract_tree_incremental(
            dir.path(),
            repo,
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
            &mut cache,
            &[],
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(changed_delta.recomputed_files, 1);
        assert_eq!(changed_delta.reused_files, 1);
        crate::load_into_graph(&mut store, &changed, repo, dir.path(), "workdir").unwrap();
        assert!(
            store
                .get_node("sym:local/delta@helper.ts#helper")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get_node("sym:local/delta@helper.ts#replacement")
                .unwrap()
                .is_some()
        );

        std::fs::remove_file(dir.path().join("helper.ts")).unwrap();
        let (deleted, _, deleted_delta) = crate::extract_tree_incremental(
            dir.path(),
            repo,
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
            &mut cache,
            &[],
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(deleted_delta.deleted_files, 1);
        crate::load_into_graph(&mut store, &deleted, repo, dir.path(), "workdir").unwrap();
        assert!(
            store
                .get_node("sym:local/delta@helper.ts#replacement")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn manifest_local_paths_beat_owner_name_shorthand() {
        // AC-0002 classification: `services/api` next to the manifest is a
        // local repo; the same shape with nothing on disk is a GitHub
        // shorthand to clone.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("services/api")).unwrap();
        assert!(!crate::manifest_entry_is_remote("services/api", dir.path()));
        assert!(crate::manifest_entry_is_remote("acme/shop", dir.path()));
        assert!(crate::manifest_entry_is_remote(
            "https://github.com/acme/shop",
            dir.path()
        ));
        assert!(!crate::manifest_entry_is_remote(
            "./services/api",
            dir.path()
        ));
    }

    #[test]
    fn system_manifest_applies_hints_and_identities_at_ingest() {
        // AC-0002 end to end: two local repos declared in one manifest —
        // the infra-hinted repo's TS is skipped, and a producer whose
        // queue URL no env file defines resolves through the manifest's
        // declared identity.
        let dir = tempfile::tempdir().unwrap();
        // `services/api`: two segments, exactly the shape of owner/name
        // shorthand — must classify as local because it exists next to the
        // manifest (never resolved against the process cwd).
        let server = dir.path().join("services").join("api");
        let infra = dir.path().join("infra");
        std::fs::create_dir_all(&server).unwrap();
        std::fs::create_dir_all(&infra).unwrap();
        std::fs::write(
            server.join("send.ts"),
            r#"
import { SQSClient, SendMessageCommand } from '@aws-sdk/client-sqs';
const sqs = new SQSClient({});
export function push() {
  return sqs.send(new SendMessageCommand({ QueueUrl: process.env.ORDERS_QUEUE }));
}
"#,
        )
        .unwrap();
        std::fs::write(
            infra.join("main.tf"),
            "resource \"aws_sqs_queue\" \"orders\" {}\n",
        )
        .unwrap();
        // A .ts file in the infra repo that the layer hint must skip.
        std::fs::write(infra.join("script.ts"), "export function x() {}\n").unwrap();
        std::fs::write(
            dir.path().join("cartograph.system.toml"),
            r#"
[[repos]]
url = "services/api"
layers = ["server", "events"]

[[repos]]
url = "./infra"
layers = ["infra"]

[env]
ORDERS_QUEUE = "https://sqs.example/orders"
"#,
        )
        .unwrap();

        let manifest = ingest::manifest::SystemManifest::load(dir.path()).unwrap();
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        for entry in &manifest.repos {
            let root = super::paths::canonicalize(dir.path().join(&entry.url)).unwrap();
            let name = root.file_name().unwrap().to_string_lossy().into_owned();
            let repo = format!("local/{name}");
            let ex = crate::extract_tree(
                &root,
                &repo,
                "workdir",
                &entry.layers,
                &manifest.env,
                None,
                None,
                &[],
            )
            .unwrap();
            crate::load_into_graph(&mut store, &ex, &repo, &root, "workdir").unwrap();
        }

        // Layer hint applied: the infra repo contributed no TS facts.
        let files = store.nodes_with_label("File").unwrap();
        assert!(files.iter().all(|f| !f.id.contains("script.ts")));
        assert_eq!(store.nodes_with_label("Resource").unwrap().len(), 1);

        // Manifest identity applied: the env-ref channel resolved Confirmed
        // via the manifest, not a Gap.
        let chans = store.nodes_with_label("Channel").unwrap();
        assert_eq!(chans.len(), 1);
        assert_eq!(chans[0].id, "chan:sqs-queue:https://sqs.example/orders");
        assert!(store.nodes_with_label("Gap").unwrap().is_empty());
        let publish = store.edges_with_labels(&["PUBLISHES"]).unwrap();
        assert_eq!(
            publish[0].props["resolver"],
            "config:cartograph.system.toml"
        );
    }

    #[test]
    fn cross_repo_flow_stitches_via_literal_channel_identity() {
        // M5 exit gate (US-0004 AC-0010 at cross-repo scope): a producer
        // repo and a consumer repo, declared as one system, stitch through
        // the global channel — one flow spans both repos, every hop T0.
        let dir = tempfile::tempdir().unwrap();
        let producer = dir.path().join("orders-api");
        let consumer = dir.path().join("mailer");
        std::fs::create_dir_all(&producer).unwrap();
        std::fs::create_dir_all(&consumer).unwrap();
        std::fs::write(
            producer.join("app.ts"),
            r#"
import express from 'express';
import { SQSClient, SendMessageCommand } from '@aws-sdk/client-sqs';
const app = express();
const sqs = new SQSClient({});
app.post('/orders', (req, res) => { queueOrder(); });
export function queueOrder() {
  return sqs.send(new SendMessageCommand({ QueueUrl: 'https://sqs.us-east-1.amazonaws.com/1/orders', MessageBody: '{}' }));
}
"#,
        )
        .unwrap();
        std::fs::write(
            consumer.join("worker.ts"),
            r#"
import { Consumer } from 'sqs-consumer';
export function startWorker() {
  return new Consumer({ queueUrl: 'https://sqs.us-east-1.amazonaws.com/1/orders', handleMessage: handle });
}
function handle() {}
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("cartograph.system.toml"),
            r#"
[[repos]]
url = "orders-api"
layers = ["server", "events"]

[[repos]]
url = "mailer"
layers = ["events", "server"]
"#,
        )
        .unwrap();

        let manifest = ingest::manifest::SystemManifest::load(dir.path()).unwrap();
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        for entry in &manifest.repos {
            let root = super::paths::canonicalize(dir.path().join(&entry.url)).unwrap();
            let name = root.file_name().unwrap().to_string_lossy().into_owned();
            let repo = format!("local/{name}");
            let ex = crate::extract_tree(
                &root,
                &repo,
                "workdir",
                &entry.layers,
                &manifest.env,
                None,
                None,
                &[],
            )
            .unwrap();
            crate::load_into_graph(&mut store, &ex, &repo, &root, "workdir").unwrap();
        }

        // One global channel, published from repo A, subscribed from repo B.
        let chans = store.nodes_with_label("Channel").unwrap();
        assert_eq!(chans.len(), 1);
        assert_eq!(
            chans[0].id,
            "chan:sqs-queue:https://sqs.us-east-1.amazonaws.com/1/orders"
        );

        // One flow, triggered in the producer repo, terminating in the
        // consumer repo — the cross-repo hop rides the channel.
        let mut flow_nodes = Vec::new();
        for label in flowtracer::FLOW_NODE_LABELS {
            flow_nodes.extend(store.nodes_with_label(label).unwrap());
        }
        let flow_edges = store
            .edges_with_labels(flowtracer::FLOW_EDGE_LABELS)
            .unwrap();
        let flows = flowtracer::trace(&flow_nodes, &flow_edges);
        assert_eq!(flows.len(), 1, "one system, one flow");
        let flow = &flows[0];
        assert_eq!(flow.trigger, "ep:local/orders-api@POST:/orders");
        assert_eq!(flow.status, flowtracer::FlowStatus::Verified);
        let sub = flow
            .hops
            .iter()
            .find(|h| h.label == "SUBSCRIBES")
            .expect("flow crosses the channel");
        assert!(
            sub.dst.contains("local/mailer@"),
            "consumer hop lands in the other repo: {}",
            sub.dst
        );
        // No gaps anywhere: both sides carry the same literal identity
        // (AC-0010); the config-resolved path is AC-0011's manifest test.
        assert!(store.nodes_with_label("Gap").unwrap().is_empty());
    }

    #[test]
    fn otel_trace_resolves_runtime_channel_gap_with_observed_provenance() {
        // M6 exit gate (issue #54, AC-0012, T-0012): T0 emits a Gap for a
        // runtime channel identity; OTLP/JSONL fills that exact source slot
        // at T1 and enriches the matching HTTP endpoint without touching T0.
        let dir = tempfile::tempdir().unwrap();
        let repo_dir = dir.path().join("shop");
        std::fs::create_dir_all(&repo_dir).unwrap();
        std::fs::write(
            repo_dir.join("app.ts"),
            r#"
import express from 'express';
import { SQSClient, SendMessageCommand } from '@aws-sdk/client-sqs';
const app = express();
const sqs = new SQSClient({});
function runtimeQueue() { return process.argv[2]; }
app.post('/orders', (_req, _res) => queueOrder());
export function queueOrder() {
  return sqs.send(new SendMessageCommand({ QueueUrl: runtimeQueue(), MessageBody: '{}' }));
}
"#,
        )
        .unwrap();
        let trace_path = dir.path().join("shop.otlp.jsonl");
        std::fs::write(
            &trace_path,
            r#"{"resourceSpans":[{"scopeSpans":[{"spans":[{"traceId":"trace-shop","spanId":"span-send","name":"send order","attributes":[{"key":"messaging.system","value":{"stringValue":"aws_sqs"}},{"key":"messaging.destination.name","value":{"stringValue":"https://sqs.example/runtime-orders"}},{"key":"code.file.path","value":{"stringValue":"/checkout/app.ts"}}]},{"traceId":"trace-shop","spanId":"span-http","name":"POST /orders","attributes":[{"key":"http.request.method","value":{"stringValue":"POST"}},{"key":"http.route","value":{"stringValue":"/orders"}}]}]}]}]}
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("cartograph.system.toml"),
            r#"
[[repos]]
url = "shop"
layers = ["server", "events"]
otel_jsonl = ["shop.otlp.jsonl"]
"#,
        )
        .unwrap();

        let manifest = ingest::manifest::SystemManifest::load(dir.path()).unwrap();
        let entry = &manifest.repos[0];
        let trace_paths: Vec<_> = entry
            .otel_jsonl
            .iter()
            .map(|path| dir.path().join(path))
            .collect();
        let extraction = crate::extract_tree(
            &repo_dir,
            "local/shop",
            "workdir",
            &entry.layers,
            &manifest.env,
            None,
            None,
            &trace_paths,
        )
        .unwrap();

        assert!(extraction.nodes.iter().all(|node| node.label != "Gap"));
        let channel = extraction
            .nodes
            .iter()
            .find(|node| node.id == "chan:sqs-queue:https://sqs.example/runtime-orders")
            .unwrap();
        assert_eq!(channel.props["prov"]["tier"], "Dynamic");
        assert_eq!(channel.props["prov"]["confidence_tier"], "Confirmed");
        assert_eq!(channel.props["observed"]["span_id"], "span-send");
        let publish = extraction
            .edges
            .iter()
            .find(|edge| edge.label == "PUBLISHES")
            .unwrap();
        assert_eq!(publish.dst, channel.id);
        assert_eq!(publish.props["resolver"], dynamic::OTEL_EXTRACTOR_ID);
        assert_eq!(publish.props["prov"]["tier"], "Dynamic");
        let endpoint = extraction
            .nodes
            .iter()
            .find(|node| node.label == "Endpoint")
            .unwrap();
        assert_eq!(endpoint.props["prov"]["tier"], "Deterministic");
        assert_eq!(endpoint.props["observed"]["span_id"], "span-http");
        assert_eq!(endpoint.props["observed_prov"]["tier"], "Dynamic");
        assert_eq!(
            endpoint.props["observed_prov"]["evidence"][0]["path"],
            trace_path.to_string_lossy().as_ref()
        );
    }

    #[test]
    fn observed_state_backs_channels_and_resolves_placeholders() {
        // M6 slice 1 (AC-0009, T-0009): `terraform show -json` output
        // enriches the T0 graph — the module placeholder resolves, the
        // secret is redacted, and the observed queue URL joins infra to
        // the code-layer channel with a BACKS edge on the topology map.
        let dir = tempfile::tempdir().unwrap();
        let repo_dir = dir.path().join("shop");
        std::fs::create_dir_all(&repo_dir).unwrap();
        std::fs::write(
            repo_dir.join("main.tf"),
            r#"
resource "aws_sqs_queue" "orders" {
  tags = { vpc = module.network.vpc_id }
}
"#,
        )
        .unwrap();
        std::fs::write(
            repo_dir.join("app.ts"),
            r#"
import { SQSClient, SendMessageCommand } from '@aws-sdk/client-sqs';
const sqs = new SQSClient({});
export function queueOrder() {
  return sqs.send(new SendMessageCommand({ QueueUrl: 'https://sqs.us-east-1.amazonaws.com/9/orders', MessageBody: '{}' }));
}
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("shop.state.json"),
            r#"{
  "format_version": "1.0",
  "values": { "root_module": {
    "resources": [{
      "address": "aws_sqs_queue.orders",
      "mode": "managed",
      "type": "aws_sqs_queue",
      "name": "orders",
      "values": {
        "url": "https://sqs.us-east-1.amazonaws.com/9/orders",
        "master_key": "hunter2"
      },
      "sensitive_values": { "master_key": true }
    }],
    "child_modules": [{
      "address": "module.network",
      "resources": [{
        "address": "module.network.aws_vpc.main",
        "mode": "managed",
        "type": "aws_vpc",
        "name": "main",
        "values": { "id": "vpc-123" },
        "sensitive_values": {}
      }]
    }]
  } }
}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("cartograph.system.toml"),
            r#"
[[repos]]
url = "shop"
state_json = "shop.state.json"
"#,
        )
        .unwrap();

        let manifest = ingest::manifest::SystemManifest::load(dir.path()).unwrap();
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        for entry in &manifest.repos {
            let root = super::paths::canonicalize(dir.path().join(&entry.url)).unwrap();
            let state_path = entry.state_json.as_ref().map(|p| dir.path().join(p));
            let ex = crate::extract_tree(
                &root,
                "local/shop",
                "workdir",
                &entry.layers,
                &manifest.env,
                state_path.as_deref(),
                None,
                &[],
            )
            .unwrap();
            crate::load_into_graph(&mut store, &ex, "local/shop", &root, "workdir").unwrap();
        }
        let backed =
            crate::stitch_backings(&mut store, &mut crate::OperationFacts::default()).unwrap();
        assert_eq!(backed, 1);

        let resources = store.nodes_with_label("Resource").unwrap();
        let queue = resources
            .iter()
            .find(|n| n.id == "res:local/shop@aws_sqs_queue.orders")
            .unwrap();
        // T0 provenance untouched; observation lands beside it (R-INT-1).
        assert_eq!(queue.props["prov"]["tier"], "Deterministic");
        assert_eq!(
            queue.props["observed"]["url"],
            "https://sqs.us-east-1.amazonaws.com/9/orders"
        );
        assert_eq!(queue.props["observed_prov"]["tier"], "Dynamic");
        // The secret never reaches the graph (US-0003 Security).
        assert_eq!(queue.props["observed"]["master_key"], dynamic::REDACTED);
        // The module placeholder was an ambiguous T0 ref; state resolved it.
        let module = resources
            .iter()
            .find(|n| n.id == "res:local/shop@module.network")
            .unwrap();
        assert!(module.props.get("placeholder").is_none());
        assert_eq!(module.props["resolved_by"], dynamic::EXTRACTOR_ID);

        // The join is on the artifact: channel cylinder + BACKS arrow.
        let mut nodes = store.nodes_with_label("Resource").unwrap();
        nodes.extend(store.nodes_with_label("Channel").unwrap());
        let edges = store.edges_with_labels(spec::TOPOLOGY_EDGE_LABELS).unwrap();
        let mmd = spec::topology_mermaid(&nodes, &edges);
        assert!(mmd.contains(r#"[("sqs-queue:https://sqs.us-east-1.amazonaws.com/9/orders")]"#));
        assert!(mmd.contains("-->|BACKS|"));
        // Re-running the join is idempotent (US-0014 re-ingest).
        assert_eq!(
            crate::stitch_backings(&mut store, &mut crate::OperationFacts::default()).unwrap(),
            1
        );
        assert_eq!(store.edges_with_labels(&["BACKS"]).unwrap().len(), 1);

        // AC-0009/T-0009 and AC-0040: removing the observation on re-ingest
        // removes its derived BACKS edge instead of retaining stale topology.
        let without_state = crate::extract_tree(
            &repo_dir,
            "local/shop",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        crate::load_into_graph(
            &mut store,
            &without_state,
            "local/shop",
            &repo_dir,
            "workdir",
        )
        .unwrap();
        assert_eq!(
            crate::stitch_backings(&mut store, &mut crate::OperationFacts::default()).unwrap(),
            0
        );
        assert!(store.edges_with_labels(&["BACKS"]).unwrap().is_empty());
        let queue = store
            .get_node("res:local/shop@aws_sqs_queue.orders")
            .unwrap()
            .unwrap();
        assert!(queue.props.get("observed").is_none());
        assert!(queue.props.get("observed_prov").is_none());
    }

    #[test]
    fn ingest_chain_produces_topology_artifact() {
        // The ingest -> graph -> spec pipeline, minus the Tauri shell:
        // mixed TS + Terraform tree in, Mermaid topology out (M2 exit gate).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.tf"),
            r#"
resource "aws_sqs_queue" "orders" {}
resource "aws_lambda_function" "fulfill" {}
resource "aws_lambda_event_source_mapping" "m" {
  event_source_arn = aws_sqs_queue.orders.arn
  function_name    = aws_lambda_function.fulfill.arn
}
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("app.ts"),
            r#"
import express from 'express';
import { EventEmitter } from 'events';
const app = express();
const bus = new EventEmitter();
app.post('/orders', (req, res) => { placeOrder(); });
export function placeOrder() { bus.emit('order.placed'); }
export function listen() { bus.on('order.placed', () => {}); }
// A class-method producer: the TS pass emits a qualified, proven method symbol.
export class Shipper {
  ship() { bus.emit('order.shipped'); }
}
"#,
        )
        .unwrap();
        // Client layer (US-0005): a routed component fetching the endpoint
        // the server file above registers.
        std::fs::write(
            dir.path().join("client.tsx"),
            r#"
import { Routes, Route } from 'react-router-dom';
export function Checkout() {
  const submit = () => fetch('/orders', { method: 'POST' });
  return <button onClick={submit}>Order</button>;
}
export function App() {
  return <Routes><Route path="/checkout" element={<Checkout />} /></Routes>;
}
"#,
        )
        .unwrap();

        let ts_id = adapters_lang_ts::SourceId {
            repo: "local",
            commit: "workdir",
        };
        let tf_id = iac::SourceId {
            repo: "local",
            commit: "workdir",
        };
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        // Mirrors ingest_path: TS + TF + stitch into one extraction, closed
        // over before anything reaches the FK-enforcing store.
        let mut extraction = adapters_lang_ts::extract_dir(dir.path(), &ts_id).unwrap();
        let tf = iac::extract_dir(dir.path(), &tf_id).unwrap();
        extraction.nodes.extend(tf.nodes);
        extraction.edges.extend(tf.edges);
        let cfg = events::ConfigIndex::from_dir(dir.path()).unwrap();
        let ev_id = events::SourceId {
            repo: "local",
            commit: "workdir",
        };
        let ev = events::stitch(&extraction.event_sites, &cfg, &ev_id);
        extraction.nodes.extend(ev.nodes);
        extraction.edges.extend(ev.edges);
        let endpoint_ids: Vec<String> = extraction
            .nodes
            .iter()
            .filter(|n| n.label == "Endpoint")
            .map(|n| n.id.clone())
            .collect();
        let fetched = events::stitch_fetches(&extraction.fetch_sites, &endpoint_ids, &cfg, &ev_id);
        extraction.nodes.extend(fetched.nodes);
        extraction.edges.extend(fetched.edges);
        extraction.close_over_endpoints();
        for n in &extraction.nodes {
            store.put_node(n).unwrap();
        }
        for e in &extraction.edges {
            store.put_edge(e).unwrap();
        }

        let nodes = store.nodes_with_label("Resource").unwrap();
        let edges = store.edges_with_labels(spec::TOPOLOGY_EDGE_LABELS).unwrap();
        let mmd = spec::topology_mermaid(&nodes, &edges);
        assert!(mmd.contains("|TRIGGERS|"));
        // The TS layer coexists without leaking onto the infra artifact.
        assert!(!mmd.contains("app_ts"));

        // The event layer stitched: producer and consumer share one channel
        // (US-0004), and channels stay off the infra artifact too.
        let channels = store.nodes_with_label("Channel").unwrap();
        let ids: Vec<&str> = channels.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "chan:inproc-event:order.placed",
                "chan:inproc-event:order.shipped"
            ]
        );
        assert!(!mmd.contains("order.placed"));
        // The class-method producer is a real, provenance-bearing Symbol, not
        // a close-over placeholder.
        let symbols = store.nodes_with_label("Symbol").unwrap();
        let ship = symbols
            .iter()
            .find(|symbol| symbol.id == "sym:local@app.ts#Shipper.ship")
            .expect("qualified class method");
        assert!(ship.props.get("placeholder").is_none());
        assert_eq!(ship.props["prov"]["confidence_tier"], "Confirmed");

        // Client layer (US-0005): the route became a Screen, and the
        // component's fetch resolved Confirmed against the server endpoint.
        let screens = store.nodes_with_label("Screen").unwrap();
        assert_eq!(screens.len(), 1);
        assert_eq!(screens[0].id, "screen:local@/checkout");
        let fetches = store.edges_with_labels(&["FETCHES"]).unwrap();
        assert_eq!(fetches.len(), 1);
        assert_eq!(fetches[0].dst, "ep:local@POST:/orders");
        assert_eq!(
            fetches[0].props["prov"]["confidence_tier"], "Confirmed",
            "resolvable fetch is Confirmed (AC-0014)"
        );

        // M4 exit gate: the flow anchors at the Screen (the fetched endpoint
        // is mid-flow, not a trigger) and runs end to end through the
        // channel to the consumer, exported as a dossier.
        let mut flow_nodes = Vec::new();
        for label in flowtracer::FLOW_NODE_LABELS {
            flow_nodes.extend(store.nodes_with_label(label).unwrap());
        }
        let flow_edges = store
            .edges_with_labels(flowtracer::FLOW_EDGE_LABELS)
            .unwrap();
        let flows = flowtracer::trace(&flow_nodes, &flow_edges);
        let dossier = spec::flow_dossier(&flows);
        assert!(dossier.contains("## Screen /checkout — Verified (score 1.00)"));
        assert!(
            !dossier.contains("## POST /orders"),
            "the fetched endpoint must not double-report as its own flow"
        );
        assert!(dossier.contains("FETCHES [Confirmed]"));
        assert!(dossier.contains("SUBSCRIBES [Confirmed]"));
        assert!(dossier.contains("chan:inproc-event:order.placed"));
    }

    #[test]
    fn plugin_gate_job_honors_cancel_and_retry_reruns_the_gate() {
        // #206 review (P2s): a cancelled plugin-gate job must never persist
        // a verdict — the job outcome and the trusted artifact state cannot
        // diverge — and Jobs retry re-dispatches the gate on the same row
        // instead of failing with "not yet supported".
        use tauri::Manager;
        const OK_ADAPTER: &[u8] = include_bytes!(
            "../../crates/adapters-plugin-host/tests/fixtures/compiled/ok-adapter.wasm"
        );

        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        let adapters = project.join(".cartograph/adapters");
        std::fs::create_dir_all(&adapters).unwrap();
        std::fs::write(adapters.join("t0.plugin-fixture.wasm"), OK_ADAPTER).unwrap();
        // A correct corpus against the fixed gate source id (`golden`).
        std::fs::write(
            adapters.join("t0.plugin-fixture.golden.json"),
            serde_json::json!({
                "extensions": ["foo"],
                "cases": [{
                    "path": "src/lib.rs",
                    "source": "hello world",
                    "nodes": [
                        {"id": "golden:src/lib.rs", "label": "TestNode", "props": {"len": 11}}
                    ],
                    "edges": [{
                        "src": "golden:src/lib.rs",
                        "dst": "golden:src/lib.rs",
                        "label": "SELF",
                        "props": {}
                    }],
                }],
            })
            .to_string(),
        )
        .unwrap();

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let state_path = dir.path().join("state.db");
        let mut roots = std::collections::BTreeSet::new();
        roots.insert(project.display().to_string());
        app.manage(super::AppState {
            graph: std::sync::Mutex::new(
                SqliteGraphStore::open(dir.path().join("graph.db")).unwrap(),
            ),
            jobs: std::sync::Mutex::new(super::JobStore::open(&state_path).unwrap()),
            investigations: crate::investigations::InvestigationRuntime::default(),
            job_execution_locks: super::job_execution_host_tests::locks(&state_path),
            findings: std::sync::Mutex::new(super::FindingStore::open(&state_path).unwrap()),
            settings: std::sync::Mutex::new(
                super::settings::SettingsStore::open(&state_path).unwrap(),
            ),
            decisions: std::sync::Mutex::new(agents::DecisionLog::open(&state_path).unwrap()),
            proposals: std::sync::Mutex::new(
                agents::ProposalStore::open(state_path.with_file_name("proposals.sqlite")).unwrap(),
            ),
            extraction_caches: std::sync::Mutex::new(super::ExtractionCaches::default()),
            primary_sources: super::primary_source::PrimarySourceStore::open(
                state_path.parent().unwrap(),
            )
            .unwrap(),
            sources: test_source_registry(&state_path, &roots),
            metrics: std::sync::Mutex::new(
                super::metrics::MetricsStore::open(&state_path).unwrap(),
            ),
        });
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let hash = core_prov::content_hash(OK_ADAPTER);

        // Enqueue a gate job, then cancel before the pipeline runs.
        let (job, execution) = super::start_job(&state, "plugin-gate:t0.plugin-fixture").unwrap();
        let job_id = job.id;
        state.jobs.lock().unwrap().cancel(job_id).unwrap();
        let result = super::plugin_gate_blocking("t0.plugin-fixture", &execution, &handle);
        assert_eq!(result.unwrap_err(), "cancelled");
        // The cancel won outright: no verdict, job row still cancelled.
        assert!(
            state
                .settings
                .lock()
                .unwrap()
                .plugin_gate("t0.plugin-fixture", &hash)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            state.jobs.lock().unwrap().get(job_id).unwrap().status,
            "cancelled"
        );

        // Retry claims the next attempt only after the old execution exits;
        // the production retry preparation then dispatches the same gate helper.
        drop(execution);
        let (_, execution, _, _) = super::prepare_job_retry(&state, job_id).unwrap();
        super::plugin_gate_blocking("t0.plugin-fixture", &execution, &handle)
            .expect("retried gate runs");
        assert_eq!(
            state.jobs.lock().unwrap().get(job_id).unwrap().status,
            "done"
        );
        let (passed, report_json) = state
            .settings
            .lock()
            .unwrap()
            .plugin_gate("t0.plugin-fixture", &hash)
            .unwrap()
            .expect("retried gate persists its verdict");
        assert!(passed, "gate report: {report_json}");
    }

    #[test]
    fn preflight_defers_eval_proof_and_recovery_reconciles_it() {
        // AC-0099, AC-0199, AC-0200, AC-0216 (#214, #243, #458): preflight no longer runs
        // the TS extraction, so every textual eval()/new Function() line is
        // reported pending AST proof — including the proven literal, which
        // preflight used to close. Recovery's own extraction then supplies
        // the adapter claims and replaces those findings in the register: a
        // proven literal closes, a const-shaped-but-unproven argument
        // downgrades to a potential Gap (leaving the Unsupported lane), and
        // runtime-computed ones stay Unsupported.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("app.ts"),
            concat!(
                "const CODE = getCode();\n",
                "export function boot() {\n",
                "  eval(\"function legacySetup() {}\");\n",
                "  eval(CODE);\n",
                "  eval(getCode() + '()');\n",
                "}\n",
                "new Function(getCode());\n",
                "new Function('return 1');\n",
            ),
        )
        .unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let path_arg = project.to_string_lossy().into_owned();
        let register_eval_lines = || -> Vec<i64> {
            state
                .findings
                .lock()
                .unwrap()
                .list()
                .unwrap()
                .into_iter()
                .filter(|f| f.detector == ingest::preflight::DETECTOR_ID)
                .filter(|f| f.message.contains("eval()") || f.message.contains("new Function()"))
                .map(|f| f.line)
                .collect()
        };

        let report = super::preflight_uncancelled(&path_arg, &handle, &state).unwrap();
        let pending: Vec<u64> = report
            .unsupported
            .iter()
            .filter(|f| f.kind == "inline-eval")
            .inspect(|f| assert!(f.message.contains("pending AST proof at recovery")))
            .map(|f| f.line)
            .collect();
        assert_eq!(
            pending,
            vec![3, 4, 5, 7, 8],
            "no line is closed or dropped before recovery proves it"
        );
        assert_eq!(register_eval_lines(), vec![3, 4, 5, 7, 8]);

        // A preflight already running when recovery starts (#439 review).
        let runs = app.state::<super::PreflightRuns>();
        let in_flight = runs.begin().unwrap();

        let source = super::register_local_source(&state, &project).unwrap();
        let operation = super::source_operation(&state, vec![(source.clone(), false)]).unwrap();
        let (_, execution) = super::start_job(&state, &source.ingest_job_kind()).unwrap();
        let summary = super::run_ingest(&source, &operation, &execution, &handle, &state).unwrap();
        drop(operation);
        assert_eq!(register_eval_lines(), vec![5, 7]);
        // The summary carries the reconciled report for the Preflight
        // surface, including the potential-Gap lane the register never
        // holds (#439 review).
        let lines = |findings: &[ingest::preflight::PatternFinding]| -> Vec<u64> {
            findings
                .iter()
                .filter(|f| f.kind == "inline-eval")
                .map(|f| f.line)
                .collect()
        };
        assert_eq!(lines(&summary.preflight.unsupported), vec![5, 7]);
        assert_eq!(lines(&summary.preflight.potential_gaps), vec![4]);

        // It finishes after recovery: its pending findings must not replace
        // the proven classification, and it answers with that classification
        // stamped at the recovery's place in the register order (#458).
        let stale =
            super::preflight_blocking(&path_arg, &handle, &state, &runs, &in_flight, &mut |_| {})
                .unwrap();
        assert_eq!(Some(&stale.register), summary.preflight_register.as_ref());
        assert_eq!(lines(&stale.unsupported), vec![5, 7]);
        assert_eq!(lines(&stale.potential_gaps), vec![4]);

        assert_eq!(
            register_eval_lines(),
            vec![5, 7],
            "recovery closes the proven literals (3, 8), downgrades the \
             const-shaped argument (4) out of Unsupported, and keeps the \
             runtime-computed sites (5, 7)"
        );
        let findings = state.findings.lock().unwrap().list().unwrap();
        assert!(
            findings
                .iter()
                .all(|f| !f.message.contains("pending AST proof")),
            "no pending wording survives recovery"
        );
    }

    #[test]
    fn a_recovery_that_fails_before_publishing_leaves_preflight_pending() {
        // AC-0200 (#439 review): reconciliation runs only after the graph
        // holds the recovered facts. A recovery that fails first (here: the
        // graph store is unusable) must not close the literal eval whose
        // facts never reached the graph.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("app.ts"), "eval('function f() {}');\n").unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let pending = || -> Vec<(i64, bool)> {
            state
                .findings
                .lock()
                .unwrap()
                .list()
                .unwrap()
                .into_iter()
                .filter(|f| f.detector == ingest::preflight::DETECTOR_ID)
                .map(|f| (f.line, f.message.contains("pending AST proof")))
                .collect()
        };
        super::preflight_uncancelled(&project.to_string_lossy(), &handle, &state).unwrap();
        assert_eq!(pending(), vec![(1, true)]);

        let source = super::register_local_source(&state, &project).unwrap();
        let operation = super::source_operation(&state, vec![(source.clone(), false)]).unwrap();
        let (_, execution) = super::start_job(&state, &source.ingest_job_kind()).unwrap();
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _graph = state.graph.lock().unwrap();
                    panic!("poison the graph store");
                })
                .join()
                .unwrap_err();
        });
        assert!(super::run_ingest(&source, &operation, &execution, &handle, &state).is_err());

        assert_eq!(
            pending(),
            vec![(1, true)],
            "the unpublished proof closed nothing"
        );
    }

    #[test]
    fn a_local_recovery_cancelled_during_reconciliation_writes_no_preflight_findings() {
        // AC-0200 (#489): the reconciled findings are written only once the
        // job has settled as completed. A cancel that lands while the
        // reconciliation scan runs — after the last cancellation check —
        // wins the race, so the register keeps preflight's pending findings.
        use tauri::{Listener, Manager};
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("app.ts"), EVAL_FIXTURE).unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        super::preflight_uncancelled(&project.to_string_lossy(), &handle, &state).unwrap();
        let source = super::register_local_source(&state, &project).unwrap();
        assert_eq!(
            preflight_register(&state, &source.repo_key),
            vec![(1, true), (2, true)]
        );

        let event_handle = handle.clone();
        let listener = handle.listen("job://detail", move |event| {
            let payload: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
            if payload["detail"] != super::RECONCILE_PREFLIGHT_DETAIL {
                return;
            }
            let state = event_handle.state::<super::AppState>();
            state
                .jobs
                .lock()
                .unwrap()
                .cancel(payload["id"].as_i64().unwrap())
                .unwrap();
        });
        let operation = super::source_operation(&state, vec![(source.clone(), false)]).unwrap();
        let (_, execution) = super::start_job(&state, &source.ingest_job_kind()).unwrap();
        let error = super::run_ingest(&source, &operation, &execution, &handle, &state)
            .err()
            .unwrap();
        handle.unlisten(listener);

        assert_eq!(error, "cancelled");
        let job = state.jobs.lock().unwrap().get(execution.id()).unwrap();
        assert_eq!(job.status, "cancelled");
        assert_eq!(
            preflight_register(&state, &source.repo_key),
            vec![(1, true), (2, true)],
            "a cancelled recovery leaves the pending findings untouched"
        );
    }

    /// A bare git repo holding `files`, cloneable as `file://<path>`.
    fn preflight_bare_repo(
        dir: &std::path::Path,
        name: &str,
        files: &[(&str, &str)],
    ) -> std::path::PathBuf {
        let src = dir.join(format!("{name}-src"));
        std::fs::create_dir_all(&src).unwrap();
        for (path, content) in files {
            std::fs::write(src.join(path), content).unwrap();
        }
        let git = |args: &[&str], cwd: &std::path::Path| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        git(&["init", "-q", "-b", "main"], &src);
        git(&["add", "."], &src);
        git(&["commit", "-q", "-m", "init"], &src);
        let bare = dir.join(format!("{name}.git"));
        git(
            &[
                "clone",
                "-q",
                "--bare",
                src.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
            dir,
        );
        bare
    }

    /// `repo`'s preflight register rows as (line, still pending AST proof).
    fn preflight_register(state: &super::AppState, repo: &str) -> Vec<(i64, bool)> {
        state
            .findings
            .lock()
            .unwrap()
            .list_for(repo)
            .unwrap()
            .into_iter()
            .filter(|f| f.detector == ingest::preflight::DETECTOR_ID)
            .inspect(|f| assert_eq!(f.kind, "unsupported"))
            .map(|f| (f.line, f.message.contains("pending AST proof")))
            .collect()
    }

    fn inline_eval_lines(findings: &[ingest::preflight::PatternFinding]) -> Vec<u64> {
        findings
            .iter()
            .filter(|f| f.kind == "inline-eval")
            .map(|f| f.line)
            .collect()
    }

    const EVAL_FIXTURE: &str = "eval('function legacySetup() {}');\neval(getCode());\n";

    #[test]
    fn github_recovery_writes_its_reconciled_preflight_findings() {
        // AC-0209 (#446): an `add_repo` recovery classifies the clone and
        // writes the result to the register exactly as a local recovery
        // does (AC-0200): the proven literal closes and the dynamic site
        // stays one Unsupported finding, with no pending wording.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let bare = preflight_bare_repo(dir.path(), "shop", &[("app.ts", EVAL_FIXTURE)]);
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();

        let summary =
            super::add_repo_blocking(format!("file://{}", bare.display()), handle.clone()).unwrap();

        assert_eq!(preflight_register(&state, &summary.repo), vec![(2, false)]);
        assert_eq!(inline_eval_lines(&summary.preflight.unsupported), vec![2]);
        assert!(summary.preflight.potential_gaps.is_empty());
    }

    #[test]
    fn a_cancelled_github_recovery_writes_no_preflight_findings() {
        // AC-0209 (#485 review): the findings are written only once the job
        // has settled as completed, so a cancel during recovery leaves the
        // register untouched.
        use tauri::{Listener, Manager};
        let dir = tempfile::tempdir().unwrap();
        let bare = preflight_bare_repo(dir.path(), "shop", &[("app.ts", EVAL_FIXTURE)]);
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let event_handle = handle.clone();
        let listener = handle.listen("job://detail", move |event| {
            let payload: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
            let state = event_handle.state::<super::AppState>();
            let _ = state
                .jobs
                .lock()
                .unwrap()
                .cancel(payload["id"].as_i64().unwrap());
        });

        let error = super::add_repo_blocking(format!("file://{}", bare.display()), handle.clone())
            .err()
            .unwrap();
        handle.unlisten(listener);

        assert_eq!(error, "cancelled");
        let findings = state.findings.lock().unwrap().list().unwrap();
        assert!(
            findings.is_empty(),
            "a cancelled recovery writes nothing: {findings:?}"
        );
    }

    #[test]
    fn manifest_recovery_writes_each_repos_reconciled_preflight_findings() {
        // AC-0209 (#446): every repo an `add_system` recovers, cloned or
        // local, gets its own reconciled register findings, and the summary
        // carries each report in manifest order.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let bare = preflight_bare_repo(dir.path(), "web", &[("app.ts", EVAL_FIXTURE)]);
        let api = dir.path().join("api");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(api.join("server.ts"), "new Function(getCode());\n").unwrap();
        let manifest = dir.path().join("cartograph.system.toml");
        std::fs::write(
            &manifest,
            format!(
                "[[repos]]\nurl = \"file://{}\"\n\n[[repos]]\nurl = \"api\"\n",
                bare.display()
            ),
        )
        .unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();

        let summary =
            super::add_system_blocking(manifest.to_string_lossy().into_owned(), handle.clone())
                .unwrap();

        let reports: Vec<(&str, Vec<u64>)> = summary
            .preflights
            .iter()
            .map(|p| (p.repo.as_str(), inline_eval_lines(&p.report.unsupported)))
            .collect();
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].1, vec![2], "the clone's dynamic eval");
        assert_eq!(reports[1].1, vec![1], "the local repo's dynamic Function");
        assert_eq!(preflight_register(&state, reports[0].0), vec![(2, false)]);
        assert_eq!(preflight_register(&state, reports[1].0), vec![(1, false)]);
    }

    #[test]
    fn a_failed_system_recovery_writes_no_preflight_findings() {
        // AC-0209 (#446): the first repo recovers, the second fails (its
        // declared state file is missing), so the system recovery fails and
        // no repo's register findings change — not even the first repo's.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let bare = preflight_bare_repo(dir.path(), "web", &[("app.ts", EVAL_FIXTURE)]);
        let api = dir.path().join("api");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(api.join("server.ts"), "export const ok = 1;\n").unwrap();
        let manifest = dir.path().join("cartograph.system.toml");
        std::fs::write(
            &manifest,
            format!(
                "[[repos]]\nurl = \"file://{}\"\n\n[[repos]]\nurl = \"api\"\nstate_json = \"missing.tfstate.json\"\n",
                bare.display()
            ),
        )
        .unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();

        let error =
            super::add_system_blocking(manifest.to_string_lossy().into_owned(), handle.clone())
                .err()
                .unwrap();
        assert!(error.contains("state_json"), "{error}");

        let findings = state.findings.lock().unwrap().list().unwrap();
        assert!(
            findings.is_empty(),
            "a failed recovery writes nothing: {findings:?}"
        );
    }

    /// Each preflight started on `done`: its repo and whether it ran.
    type PreflightOutcomes = std::sync::Mutex<Vec<(String, Result<(), String>)>>;

    /// On the recovery's `done` event, run a real preflight (the `preflight`
    /// command's own path: register the run, scan on a worker, persist) over
    /// every registered source's root — the user saw the job finish and
    /// started a new preflight before the reconciliation wrote. Returns each
    /// scan's outcome, keyed by repo.
    fn preflight_on_done(
        handle: &tauri::AppHandle<tauri::test::MockRuntime>,
    ) -> (tauri::EventId, std::sync::Arc<PreflightOutcomes>) {
        use tauri::{Listener, Manager};
        let outcomes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = outcomes.clone();
        let event_handle = handle.clone();
        let listener = handle.listen("job://changed", move |event| {
            let payload: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
            if payload["status"] != "done" {
                return;
            }
            let state = event_handle.state::<super::AppState>();
            let sources = state.sources.lock().unwrap().list().unwrap();
            for source in sources {
                let path = source.root().to_string_lossy().into_owned();
                let outcome = tauri::async_runtime::block_on(super::start_preflight(
                    path,
                    1,
                    event_handle.clone(),
                ))
                .map(drop);
                seen.lock().unwrap().push((source.repo_key, outcome));
            }
        });
        (listener, outcomes)
    }

    /// Every preflight started on `done` succeeded, and each repo's register
    /// holds that preflight's pending findings, not the reconciliation's.
    fn assert_preflights_on_done_won(
        state: &super::AppState,
        outcomes: &PreflightOutcomes,
        expected: &[(&str, Vec<(i64, bool)>)],
    ) {
        let outcomes = outcomes.lock().unwrap();
        assert_eq!(outcomes.len(), expected.len(), "{outcomes:?}");
        for (repo, outcome) in outcomes.iter() {
            assert_eq!(outcome, &Ok(()), "the preflight of {repo} ran");
        }
        for (repo, rows) in expected {
            assert_eq!(&preflight_register(state, repo), rows, "{repo}");
        }
    }

    #[test]
    fn a_preflight_started_after_a_local_recovery_is_done_keeps_its_findings() {
        // AC-0214 (#497): the reconciliation fence is taken before `done` is
        // published, so a preflight begun once the user sees it is newer:
        // it runs, and the reconciliation neither refuses nor overwrites it.
        use tauri::{Listener, Manager};
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("app.ts"), EVAL_FIXTURE).unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let source = super::register_local_source(&state, &project).unwrap();
        let operation = super::source_operation(&state, vec![(source.clone(), false)]).unwrap();
        let (_, execution) = super::start_job(&state, &source.ingest_job_kind()).unwrap();
        let (listener, outcomes) = preflight_on_done(&handle);

        super::run_ingest(&source, &operation, &execution, &handle, &state).unwrap();
        handle.unlisten(listener);

        let pending = vec![(1, true), (2, true)];
        assert_preflights_on_done_won(&state, &outcomes, &[(&source.repo_key, pending)]);
    }

    #[test]
    fn a_preflight_started_after_a_github_recovery_is_done_keeps_its_findings() {
        // AC-0214 (#497 and its review): as for a local recovery. The managed
        // clone is ready and its write reservation released before `done`,
        // so the preflight can read it.
        use tauri::{Listener, Manager};
        let dir = tempfile::tempdir().unwrap();
        let bare = preflight_bare_repo(dir.path(), "shop", &[("app.ts", EVAL_FIXTURE)]);
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let (listener, outcomes) = preflight_on_done(&handle);

        let summary =
            super::add_repo_blocking(format!("file://{}", bare.display()), handle.clone()).unwrap();
        handle.unlisten(listener);

        let pending = vec![(1, true), (2, true)];
        assert_preflights_on_done_won(&state, &outcomes, &[(&summary.repo, pending)]);
    }

    #[test]
    fn a_preflight_started_after_a_manifest_recovery_is_done_keeps_its_findings() {
        // AC-0214 (#497 and its review): as for a local recovery, for every
        // repo, cloned or local.
        use tauri::{Listener, Manager};
        let dir = tempfile::tempdir().unwrap();
        let bare = preflight_bare_repo(dir.path(), "web", &[("app.ts", EVAL_FIXTURE)]);
        let api = dir.path().join("api");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(api.join("server.ts"), "new Function(getCode());\n").unwrap();
        let manifest = dir.path().join("cartograph.system.toml");
        std::fs::write(
            &manifest,
            format!(
                "[[repos]]\nurl = \"file://{}\"\n\n[[repos]]\nurl = \"api\"\n",
                bare.display()
            ),
        )
        .unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let (listener, outcomes) = preflight_on_done(&handle);

        let summary =
            super::add_system_blocking(manifest.to_string_lossy().into_owned(), handle.clone())
                .unwrap();
        handle.unlisten(listener);

        assert_eq!(summary.preflights.len(), 2);
        assert_preflights_on_done_won(
            &state,
            &outcomes,
            &[
                (&summary.preflights[0].repo, vec![(1, true), (2, true)]),
                (&summary.preflights[1].repo, vec![(1, true)]),
            ],
        );
    }

    #[test]
    fn a_retried_recovery_returns_its_reconciled_preflight_report() {
        // AC-0216 (#458): Jobs → Retry re-runs the recovery through
        // `run_ingest`; the reconciled preflight report and its register
        // stamp reach the UI with the retried job row, as they do from the
        // recovery command itself.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("app.ts"), EVAL_FIXTURE).unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let source = super::register_local_source(&state, &project).unwrap();
        let job_id = {
            let (job, execution) = super::start_job(&state, &source.ingest_job_kind()).unwrap();
            state.jobs.lock().unwrap().cancel(job.id).unwrap();
            drop(execution);
            job.id
        };

        let retried = super::retry_job_blocking(job_id, handle.clone()).unwrap();

        assert_eq!(retried.job.status, "done");
        let recovery = retried.recovery.as_ref().expect("the recovery's report");
        assert_eq!(inline_eval_lines(&recovery.preflight.unsupported), vec![2]);
        let stamp = recovery.preflight_register.as_ref().unwrap();
        assert_eq!(stamp.repo, source.repo_key);
        assert_eq!(
            preflight_register(&state, &source.repo_key),
            vec![(2, false)]
        );
        let wire = serde_json::to_value(&retried).unwrap();
        assert_eq!(wire["id"], job_id, "the job row stays flat on the wire");
        assert_eq!(
            wire["recovery"]["preflight_register"]["repo"],
            source.repo_key
        );
    }

    #[test]
    fn an_older_recovery_fence_never_overwrites_a_newer_reconciliation() {
        // AC-0216 (#514 review): two overlapping recoveries of one repo. The
        // newer fence reconciles first; the older one, resuming late, writes
        // nothing, so the register and the stamps agree on the newer report.
        let runs = super::PreflightRuns::default();
        let older = runs.reserve_reconcile().unwrap();
        let newer = runs.reserve_reconcile().unwrap();
        let stamp = runs
            .reconcile(&newer, "repo", &Default::default(), || Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(stamp.epoch, newer.epoch);
        let mut wrote = false;
        let late = runs
            .reconcile(&older, "repo", &Default::default(), || {
                wrote = true;
                Ok(())
            })
            .unwrap();
        assert!(
            late.is_none() && !wrote,
            "the older recovery writes nothing"
        );
    }

    #[test]
    fn a_reconcile_fence_orders_before_later_preflights() {
        // AC-0214 (#497): a preflight begun before the fence is refused after
        // the reconciliation; one begun after it wins whether it persists
        // before the reconciliation (which then writes nothing) or after.
        let runs = super::PreflightRuns::default();
        let fence = runs.reserve_reconcile().unwrap();
        let newer = runs.begin().unwrap();
        runs.persist_if_current(&newer, "repo", |_| Ok(())).unwrap();
        let mut wrote = false;
        let written = runs
            .reconcile(&fence, "repo", &Default::default(), || {
                wrote = true;
                Ok(())
            })
            .unwrap();
        assert!(written.is_none() && !wrote, "the newer findings stand");

        let runs = super::PreflightRuns::default();
        let fence = runs.reserve_reconcile().unwrap();
        let later = runs.begin().unwrap();
        let stamp = runs
            .reconcile(&fence, "repo", &Default::default(), || Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!((stamp.repo.as_str(), stamp.epoch), ("repo", fence.epoch));
        assert!(matches!(
            runs.persist_if_current(&later, "repo", |_| Ok(())),
            Ok(super::Persisted::Written(()))
        ));

        let runs = super::PreflightRuns::default();
        let older = runs.begin().unwrap();
        let fence = runs.reserve_reconcile().unwrap();
        assert!(
            runs.reconcile(&fence, "repo", &Default::default(), || Ok(()))
                .unwrap()
                .is_some()
        );
        assert!(matches!(
            runs.persist_if_current(&older, "repo", |_| Ok(())),
            Ok(super::Persisted::Reconciled { .. })
        ));
    }

    /// A mock app managing the stores a preflight touches, rooted in `dir`.
    fn preflight_test_app(dir: &std::path::Path) -> tauri::App<tauri::test::MockRuntime> {
        use tauri::Manager;
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let state_path = dir.join("state.db");
        app.manage(super::PreflightRuns::default());
        app.manage(super::AppState {
            graph: std::sync::Mutex::new(SqliteGraphStore::open(dir.join("graph.db")).unwrap()),
            jobs: std::sync::Mutex::new(super::JobStore::open(&state_path).unwrap()),
            investigations: crate::investigations::InvestigationRuntime::default(),
            job_execution_locks: super::job_execution_host_tests::locks(&state_path),
            findings: std::sync::Mutex::new(super::FindingStore::open(&state_path).unwrap()),
            settings: std::sync::Mutex::new(
                super::settings::SettingsStore::open(&state_path).unwrap(),
            ),
            decisions: std::sync::Mutex::new(agents::DecisionLog::open(&state_path).unwrap()),
            proposals: std::sync::Mutex::new(
                agents::ProposalStore::open(state_path.with_file_name("proposals.sqlite")).unwrap(),
            ),
            extraction_caches: std::sync::Mutex::new(super::ExtractionCaches::default()),
            primary_sources: super::primary_source::PrimarySourceStore::open(dir).unwrap(),
            sources: test_source_registry(&state_path, &std::collections::BTreeSet::new()),
            metrics: std::sync::Mutex::new(
                super::metrics::MetricsStore::open(&state_path).unwrap(),
            ),
        });
        app
    }

    #[test]
    fn preflight_command_scans_on_a_worker_and_reports_progress() {
        // AC-0197 (#235): the preflight command never parses on the invoking
        // (webview/main) thread — the eval-claims walk runs on a blocking
        // worker and streams `preflight://progress` while it reads.
        use tauri::Listener;
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("app.ts"), "eval(getCode());\n").unwrap();
        let app = preflight_test_app(dir.path());
        let emitters = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = emitters.clone();
        app.listen_any("preflight://progress", move |event| {
            seen.lock()
                .unwrap()
                .push((std::thread::current().id(), event.payload().to_string()));
        });

        let caller = std::thread::current().id();
        let report = tauri::async_runtime::block_on(super::start_preflight(
            project.to_string_lossy().into_owned(),
            7,
            app.handle().clone(),
        ))
        .unwrap();

        assert!(report.unsupported.iter().any(|f| f.kind == "inline-eval"));
        let emitters = emitters.lock().unwrap();
        // The listing walk's first ping (the root) always fires, and so does
        // the first file ping after it (#453).
        assert_eq!(emitters.len(), 2, "{emitters:?}");
        assert!(
            emitters.iter().all(|(worker, _)| *worker != caller),
            "extraction ran on the calling thread"
        );
        assert_eq!(
            emitters[0].1,
            r#"{"run":7,"path":"","done":0,"total":null}"#
        );
        assert_eq!(
            emitters[1].1,
            r#"{"run":7,"path":"app.ts","done":0,"total":1}"#
        );
    }

    #[test]
    fn the_report_walk_reports_progress_too() {
        // AC-0197 (#434 review): the report walk reads every file, not just
        // the TS sources the eval pass parses, so it must keep the progress
        // line moving. A Go-only project gives the eval pass nothing to do.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("main.go"), "package main\n").unwrap();
        let app = preflight_test_app(dir.path());
        let state = app.state::<super::AppState>();
        let runs = app.state::<super::PreflightRuns>();
        let token = runs.begin().unwrap();

        let mut steps = Vec::new();
        super::preflight_blocking(
            &project.to_string_lossy(),
            app.handle(),
            &state,
            &runs,
            &token,
            &mut |step| steps.push((step.path.to_string(), step.done, step.total)),
        )
        .unwrap();
        assert_eq!(
            steps,
            vec![
                (String::new(), 0, None),
                ("main.go".to_string(), 0, Some(1))
            ]
        );
    }

    #[test]
    fn cancelled_preflight_stops_before_the_next_file_and_persists_nothing() {
        // AC-0198 (#235): a cancel stops the walk before the next file and
        // leaves the finding register untouched; a new preflight supersedes
        // (cancels) the one in flight.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let project = preflight_eval_project(dir.path());
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let runs = app.state::<super::PreflightRuns>();
        let path_arg = project.to_string_lossy().into_owned();

        // Cancelled while reading the first file: nothing after it is parsed.
        let token = runs.begin().unwrap();
        let mut announced = Vec::new();
        let result =
            super::preflight_blocking(&path_arg, &handle, &state, &runs, &token, &mut |step| {
                if step.total.is_none() {
                    return; // still listing the tree
                }
                announced.push(step.path.to_string());
                runs.cancel().unwrap();
            });
        assert_eq!(result.unwrap_err(), super::PREFLIGHT_CANCELLED);
        assert_eq!(announced, vec!["a.ts".to_string()]);
        assert!(
            state.findings.lock().unwrap().list().unwrap().is_empty(),
            "a cancelled preflight persists no findings"
        );

        // A new preflight supersedes the one in flight.
        let first = runs.begin().unwrap();
        let second = runs.begin().unwrap();
        assert!(first.is_cancelled());
        assert!(!second.is_cancelled());

        // The uncancelled run completes and persists its findings.
        let report =
            super::preflight_blocking(&path_arg, &handle, &state, &runs, &second, &mut |_| {})
                .unwrap();
        assert_eq!(report.unsupported.len(), 3);
        assert_eq!(state.findings.lock().unwrap().list().unwrap().len(), 3);
    }

    /// Three TS files, each with one dynamic eval — three findings when a
    /// preflight completes.
    fn preflight_eval_project(dir: &std::path::Path) -> std::path::PathBuf {
        let project = dir.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        for name in ["a.ts", "b.ts", "c.ts"] {
            std::fs::write(project.join(name), "eval(getCode());\n").unwrap();
        }
        project
    }

    #[test]
    fn cancelled_preflight_stops_while_listing_the_tree() {
        // AC-0198 (#453): a cancel that lands while the tree is still being
        // listed stops the listing walk at the next directory — no later
        // directory is entered, no file is announced or read, and nothing is
        // persisted.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        for sub in ["a", "b", "c"] {
            std::fs::create_dir_all(project.join(sub)).unwrap();
            std::fs::write(project.join(sub).join("x.ts"), "eval(getCode());\n").unwrap();
        }
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let runs = app.state::<super::PreflightRuns>();
        let token = runs.begin().unwrap();

        let mut steps = Vec::new();
        let result = super::preflight_blocking(
            &project.to_string_lossy(),
            &handle,
            &state,
            &runs,
            &token,
            &mut |step| {
                steps.push((step.path.to_string(), step.total));
                if step.path == "a" {
                    runs.cancel().unwrap();
                }
            },
        );
        assert_eq!(result.unwrap_err(), super::PREFLIGHT_CANCELLED);
        assert_eq!(
            steps,
            vec![(String::new(), None), ("a".to_string(), None)],
            "the walk stopped listing before directory b"
        );
        assert!(state.findings.lock().unwrap().list().unwrap().is_empty());
    }

    #[test]
    fn preflight_cancelled_before_its_worker_starts_never_runs() {
        // AC-0198 (#434 review): the run is registered when the command
        // starts, not when the blocking worker picks it up, so a Cancel that
        // arrives while the worker is still queued stops it.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let project = preflight_eval_project(dir.path());
        let app = preflight_test_app(dir.path());
        let state = app.state::<super::AppState>();

        let run = super::start_preflight(
            project.to_string_lossy().into_owned(),
            1,
            app.handle().clone(),
        );
        app.state::<super::PreflightRuns>().cancel().unwrap();
        let result = tauri::async_runtime::block_on(run);

        assert_eq!(result.unwrap_err(), super::PREFLIGHT_CANCELLED);
        assert!(state.findings.lock().unwrap().list().unwrap().is_empty());
    }

    #[test]
    fn preflight_supersession_follows_invocation_order() {
        // AC-0198 (#434 review): a newer preflight supersedes an older one
        // by invocation order, even when the older one's worker runs first.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let project = preflight_eval_project(dir.path());
        let app = preflight_test_app(dir.path());
        let path_arg = project.to_string_lossy().into_owned();

        let older = super::start_preflight(path_arg.clone(), 1, app.handle().clone());
        let newer = super::start_preflight(path_arg, 2, app.handle().clone());

        assert_eq!(
            tauri::async_runtime::block_on(older).unwrap_err(),
            super::PREFLIGHT_CANCELLED
        );
        let report = tauri::async_runtime::block_on(newer).unwrap();
        assert_eq!(report.unsupported.len(), 3);
        let state = app.state::<super::AppState>();
        assert_eq!(state.findings.lock().unwrap().list().unwrap().len(), 3);
    }

    #[test]
    fn recovery_fences_only_older_preflights_of_its_repo() {
        // AC-0200 (#439 review): a reconciliation refuses preflights of the
        // same repo that began before it; other repos and later preflights
        // are unaffected, and a failed reconciliation fences nothing.
        let runs = super::PreflightRuns::default();
        let older = runs.begin().unwrap();
        let fence = runs.reserve_reconcile().unwrap();
        runs.reconcile(&fence, "repo", &Default::default(), || Ok(()))
            .unwrap();
        let written = |token: &super::PreflightToken, repo: &str| {
            matches!(
                runs.persist_if_current(token, repo, |_| Ok(())),
                Ok(super::Persisted::Written(()))
            )
        };
        assert!(!written(&older, "repo"));
        assert!(written(&older, "other"));
        let newer = runs.begin().unwrap();
        assert!(written(&newer, "repo"));

        let fence = runs.reserve_reconcile().unwrap();
        runs.reconcile(&fence, "other", &Default::default(), || {
            Err("disk full".to_string())
        })
        .unwrap_err();
        assert!(written(&newer, "other"));
    }

    #[test]
    fn cancel_never_waits_on_persistence() {
        // AC-0198 (#434 review): `cancel_preflight` is synchronous, so it
        // must not block on a register write in progress; a cancel that
        // lands first still prevents the write.
        use std::sync::mpsc;
        let runs = std::sync::Arc::new(super::PreflightRuns::default());
        let token = runs.begin().unwrap();
        let (cancelled_tx, cancelled_rx) = mpsc::channel();
        runs.persist_if_current(&token, "repo", |_| {
            let runs = runs.clone();
            std::thread::spawn(move || {
                runs.cancel().unwrap();
                cancelled_tx.send(()).unwrap();
            });
            cancelled_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|_| "cancel waited for the write".to_string())
        })
        .unwrap();

        let mut wrote = false;
        let late = runs.persist_if_current(&token, "repo", |_| {
            wrote = true;
            Ok(())
        });
        assert_eq!(late.unwrap_err(), super::PREFLIGHT_CANCELLED);
        assert!(!wrote, "a cancelled run writes nothing");
    }

    #[test]
    fn a_cancel_during_the_register_write_rolls_it_back() {
        // AC-0215 (#493): a cancel that lands while the preflight's register
        // write is in progress — after the pre-write check, before commit —
        // rolls the write back: the register keeps its previous rows and the
        // scan reports that it was cancelled. Cancel still never waits.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let app = preflight_test_app(dir.path());
        let state = app.state::<super::AppState>();
        let runs = std::sync::Arc::new(super::PreflightRuns::default());
        let report = |line: u64| ingest::preflight::PreflightReport {
            unsupported: vec![ingest::preflight::PatternFinding {
                kind: "inline-eval".into(),
                path: "app.ts".into(),
                line,
                message: "m".into(),
                detector: "d".into(),
                request_adapter: None,
            }],
            ..Default::default()
        };
        let write = |token: &super::PreflightToken, line: u64, cancel_mid_write: bool| {
            runs.persist_if_current(token, "repo", |live| {
                // The cancel arrives from another thread once the batch is
                // staged, exactly when `live` is re-checked before commit.
                let live_after_cancel = || {
                    if cancel_mid_write {
                        let runs = runs.clone();
                        std::thread::spawn(move || runs.cancel().unwrap())
                            .join()
                            .unwrap();
                    }
                    live()
                };
                super::persist_preflight_findings_if(
                    &state,
                    "repo",
                    &report(line),
                    &live_after_cancel,
                )
            })
        };
        write(&runs.begin().unwrap(), 1, false).unwrap();

        let abandoned = runs.begin().unwrap();
        assert_eq!(
            write(&abandoned, 2, true).unwrap_err(),
            super::PREFLIGHT_CANCELLED
        );
        let lines: Vec<i64> = state
            .findings
            .lock()
            .unwrap()
            .list_for("repo")
            .unwrap()
            .into_iter()
            .map(|f| f.line)
            .collect();
        assert_eq!(
            lines,
            vec![1],
            "the cancelled run's findings were rolled back"
        );
    }

    #[test]
    fn a_cancel_after_the_commit_claim_loses_and_the_scan_completes() {
        // AC-0215 (#493 review): the commit decision is atomic with
        // cancellation. A cancel that lands right after the run claimed its
        // commit — the last step before `tx.commit()` — loses: the write
        // commits and the scan reports its result, never a cancellation it
        // did not honour. Either the cancel wins and nothing is written, or
        // the run completes; never both.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let app = preflight_test_app(dir.path());
        let state = app.state::<super::AppState>();
        let runs = super::PreflightRuns::default();
        let token = runs.begin().unwrap();
        let report = ingest::preflight::PreflightReport {
            unsupported: vec![ingest::preflight::PatternFinding {
                kind: "inline-eval".into(),
                path: "app.ts".into(),
                line: 1,
                message: "m".into(),
                detector: "d".into(),
                request_adapter: None,
            }],
            ..Default::default()
        };
        let cancel_won = std::cell::Cell::new(None);
        let result = runs.persist_if_current(&token, "repo", |claim_commit| {
            super::persist_preflight_findings_if(&state, "repo", &report, &|| {
                let claimed = claim_commit();
                // The cancel arrives between the claim and the commit.
                cancel_won.set(Some(runs.cancel().unwrap()));
                claimed
            })
        });

        assert_eq!(cancel_won.get(), Some(false), "the cancel lost the race");
        assert!(!token.is_cancelled());
        assert!(result.is_ok(), "the run completed: {result:?}");
        assert_eq!(
            state
                .findings
                .lock()
                .unwrap()
                .list_for("repo")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn preflight_register_writes_never_interleave() {
        // AC-0198 (#434 review): a superseding run's write waits for the
        // older run's write in progress, so the newer result lands last.
        use std::sync::atomic::{AtomicBool, Ordering};
        let runs = std::sync::Arc::new(super::PreflightRuns::default());
        let older = runs.begin().unwrap();
        let writing = std::sync::Arc::new(AtomicBool::new(false));
        let (inside_tx, inside_rx) = std::sync::mpsc::channel();
        let newer = {
            let runs = runs.clone();
            let writing = writing.clone();
            std::thread::spawn(move || {
                inside_rx.recv().unwrap();
                let newer = runs.begin().unwrap();
                runs.persist_if_current(&newer, "repo", |_| {
                    assert!(!writing.load(Ordering::SeqCst), "writes interleaved");
                    Ok(())
                })
            })
        };
        runs.persist_if_current(&older, "repo", |_| {
            writing.store(true, Ordering::SeqCst);
            inside_tx.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(100));
            writing.store(false, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
        newer.join().unwrap().unwrap();
    }

    #[test]
    fn plugin_lane_end_to_end_gate_accept_reingest_closes_finding() {
        // AC-0093 (#201): request → gate pass → accept → re-scan. An
        // uncovered language surfaces with the request-adapter action; a
        // gated + enabled plugin then counts as coverage (the finding
        // closes) and routes its claimed files during extraction, with
        // pinned facts identical across runs (AC-0069 extension).
        use tauri::Manager;
        const OK_ADAPTER: &[u8] = include_bytes!(
            "../../crates/adapters-plugin-host/tests/fixtures/compiled/ok-adapter.wasm"
        );

        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        let adapters = project.join(".cartograph/adapters");
        std::fs::create_dir_all(&adapters).unwrap();
        std::fs::write(project.join("app.rb"), "puts 'hi'\n").unwrap();
        std::fs::write(adapters.join("t0.plugin-fixture.wasm"), OK_ADAPTER).unwrap();
        std::fs::write(
            adapters.join("t0.plugin-fixture.golden.json"),
            serde_json::json!({
                "extensions": ["rb"],
                "cases": [{
                    "path": "src/lib.rs",
                    "source": "hello world",
                    "nodes": [
                        {"id": "golden:src/lib.rs", "label": "TestNode", "props": {"len": 11}}
                    ],
                    "edges": [{
                        "src": "golden:src/lib.rs",
                        "dst": "golden:src/lib.rs",
                        "label": "SELF",
                        "props": {}
                    }],
                }],
            })
            .to_string(),
        )
        .unwrap();

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let state_path = dir.path().join("state.db");
        app.manage(super::AppState {
            graph: std::sync::Mutex::new(
                SqliteGraphStore::open(dir.path().join("graph.db")).unwrap(),
            ),
            jobs: std::sync::Mutex::new(super::JobStore::open(&state_path).unwrap()),
            investigations: crate::investigations::InvestigationRuntime::default(),
            job_execution_locks: super::job_execution_host_tests::locks(&state_path),
            findings: std::sync::Mutex::new(super::FindingStore::open(&state_path).unwrap()),
            settings: std::sync::Mutex::new(
                super::settings::SettingsStore::open(&state_path).unwrap(),
            ),
            decisions: std::sync::Mutex::new(agents::DecisionLog::open(&state_path).unwrap()),
            proposals: std::sync::Mutex::new(
                agents::ProposalStore::open(state_path.with_file_name("proposals.sqlite")).unwrap(),
            ),
            extraction_caches: std::sync::Mutex::new(super::ExtractionCaches::default()),
            primary_sources: super::primary_source::PrimarySourceStore::open(
                state_path.parent().unwrap(),
            )
            .unwrap(),
            sources: test_source_registry(&state_path, &std::collections::BTreeSet::new()),
            metrics: std::sync::Mutex::new(
                super::metrics::MetricsStore::open(&state_path).unwrap(),
            ),
        });
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let root = super::paths::canonicalize(&project).unwrap();
        let root_key = root.display().to_string();
        let path_arg = project.to_string_lossy().into_owned();

        // Before: Ruby is uncovered, with the request-adapter action, and
        // the finding persists to the register.
        let before = super::preflight_uncancelled(&path_arg, &handle, &state).unwrap();
        let finding = before
            .unsupported
            .iter()
            .find(|f| f.kind == "uncovered-language" && f.message.contains("Ruby"))
            .expect("Ruby surfaces as uncovered");
        assert_eq!(finding.request_adapter.as_deref(), Some("Ruby"));
        let repo = state
            .sources
            .lock()
            .unwrap()
            .get_by_root(&root)
            .unwrap()
            .unwrap()
            .repo_key;
        assert!(
            state
                .findings
                .lock()
                .unwrap()
                .list_for(&repo)
                .unwrap()
                .iter()
                .any(|f| f.message.contains("Ruby"))
        );
        // Discovery alone never activates: nothing extracts yet.
        assert!(
            super::active_plugins_for_root(&handle, &state, &root)
                .unwrap()
                .is_empty()
        );

        // Gate the artifact (the request lane lands it here), then accept.
        let (_, execution) = super::start_job(&state, "plugin-gate:t0.plugin-fixture").unwrap();
        let report = super::plugin_gate_blocking("t0.plugin-fixture", &execution, &handle).unwrap();
        assert_eq!(report["passed"], serde_json::json!(true));
        let hash = core_prov::content_hash(OK_ADAPTER);
        state
            .settings
            .lock()
            .unwrap()
            .set_plugin_enabled(&root_key, "t0.plugin-fixture", &hash, true)
            .unwrap();

        // Re-scan: the plugin counts as coverage and the finding closes.
        let after = super::preflight_uncancelled(&path_arg, &handle, &state).unwrap();
        let ruby = after
            .languages
            .iter()
            .find(|l| l.language == "Ruby")
            .expect("Ruby still detected");
        assert_eq!(ruby.adapter.as_deref(), Some("t0.plugin-fixture"));
        assert!(
            !after
                .unsupported
                .iter()
                .any(|f| f.kind == "uncovered-language" && f.message.contains("Ruby"))
        );
        assert!(
            !state
                .findings
                .lock()
                .unwrap()
                .list_for(&repo)
                .unwrap()
                .iter()
                .any(|f| f.message.contains("Ruby"))
        );

        // Re-ingest routes the claimed file through the plugin: pinned
        // facts, identical across runs (plugin-active determinism).
        let active = super::active_plugins_for_root(&handle, &state, &root).unwrap();
        assert_eq!(active.len(), 1);
        let run = |cache: &mut super::RepoExtractionCache| {
            super::extract_tree_incremental(
                &root,
                &repo,
                "workdir",
                &[],
                &std::collections::BTreeMap::new(),
                None,
                None,
                &[],
                cache,
                &active,
                &mut |_| {},
            )
            .unwrap()
            .0
        };
        let first = run(&mut super::RepoExtractionCache::default());
        let second = run(&mut super::RepoExtractionCache::default());
        let plugin_node = first
            .nodes
            .iter()
            .find(|n| n.id == format!("{repo}:app.rb"))
            .expect("plugin fact joins the extraction");
        assert_eq!(
            plugin_node.props["plugin_artifact_hash"],
            serde_json::json!(hash)
        );
        // Host-filled provenance (#208 review): the routed fact satisfies
        // the tier/confidence invariant and cites its mediated source.
        assert_eq!(
            plugin_node.props["prov"]["tier"],
            serde_json::json!("Deterministic")
        );
        assert_eq!(
            plugin_node.props["prov"]["evidence"][0]["path"],
            serde_json::json!("app.rb")
        );
        assert_eq!(
            serde_json::to_string(&first.nodes).unwrap(),
            serde_json::to_string(&second.nodes).unwrap()
        );
        assert_eq!(
            serde_json::to_string(&first.edges).unwrap(),
            serde_json::to_string(&second.edges).unwrap()
        );
    }

    #[test]
    fn user_plugin_covers_roots_that_do_not_own_the_project_copy() {
        // #208 review: in a multi-root session, a project copy of an id in
        // root A must not shadow the gated+enabled user-level copy that
        // root B's coverage relies on — the active scan discovers per root.
        const OK_ADAPTER: &[u8] = include_bytes!(
            "../../crates/adapters-plugin-host/tests/fixtures/compiled/ok-adapter.wasm"
        );

        let dir = tempfile::tempdir().unwrap();
        let user_dir = dir.path().join("user-adapters");
        std::fs::create_dir_all(&user_dir).unwrap();
        std::fs::write(user_dir.join("t0.plugin-fixture.wasm"), OK_ADAPTER).unwrap();
        std::fs::write(
            user_dir.join("t0.plugin-fixture.golden.json"),
            serde_json::json!({ "extensions": ["rb"], "cases": [] }).to_string(),
        )
        .unwrap();
        // Root A owns a *different* project artifact under the same id;
        // root B has no project copy at all.
        let root_a = dir.path().join("a");
        let a_adapters = root_a.join(".cartograph/adapters");
        std::fs::create_dir_all(&a_adapters).unwrap();
        std::fs::write(a_adapters.join("t0.plugin-fixture.wasm"), b"other bytes").unwrap();
        let root_b = dir.path().join("b");
        std::fs::create_dir_all(&root_b).unwrap();

        let state_path = dir.path().join("state.db");
        let state = super::AppState {
            graph: std::sync::Mutex::new(
                SqliteGraphStore::open(dir.path().join("graph.db")).unwrap(),
            ),
            jobs: std::sync::Mutex::new(super::JobStore::open(&state_path).unwrap()),
            investigations: crate::investigations::InvestigationRuntime::default(),
            job_execution_locks: super::job_execution_host_tests::locks(&state_path),
            findings: std::sync::Mutex::new(super::FindingStore::open(&state_path).unwrap()),
            settings: std::sync::Mutex::new(
                super::settings::SettingsStore::open(&state_path).unwrap(),
            ),
            decisions: std::sync::Mutex::new(agents::DecisionLog::open(&state_path).unwrap()),
            proposals: std::sync::Mutex::new(
                agents::ProposalStore::open(state_path.with_file_name("proposals.sqlite")).unwrap(),
            ),
            extraction_caches: std::sync::Mutex::new(super::ExtractionCaches::default()),
            primary_sources: super::primary_source::PrimarySourceStore::open(
                state_path.parent().unwrap(),
            )
            .unwrap(),
            sources: test_source_registry(
                &state_path,
                &std::collections::BTreeSet::from([
                    root_a.display().to_string(),
                    root_b.display().to_string(),
                ]),
            ),
            metrics: std::sync::Mutex::new(
                super::metrics::MetricsStore::open(&state_path).unwrap(),
            ),
        };
        // Only the user copy is gated and enabled (user-scoped).
        let user_hash = core_prov::content_hash(OK_ADAPTER);
        {
            let mut settings = state.settings.lock().unwrap();
            settings
                .record_plugin_gate("t0.plugin-fixture", &user_hash, true, "{}")
                .unwrap();
            settings
                .set_plugin_enabled("user", "t0.plugin-fixture", &user_hash, true)
                .unwrap();
        }

        // Root B relies on the user copy — and gets it, even though root A
        // owns a project copy of the same id elsewhere in the session.
        let for_b = super::active_plugins_in(&state, &root_b, &user_dir).unwrap();
        assert_eq!(for_b.len(), 1);
        assert_eq!(for_b[0].content_hash, user_hash);
        assert!(for_b[0].path.starts_with(&user_dir));

        // Root A's own project copy shadows the id there, and that copy is
        // neither gated nor enabled: nothing extracts for root A.
        assert!(
            super::active_plugins_in(&state, &root_a, &user_dir)
                .unwrap()
                .is_empty()
        );
    }
}
