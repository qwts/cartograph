//! Cartograph desktop shell (M0): boots the webview, owns the graph store and
//! the durable job spine, and exposes the first Tauri commands.

// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod escalation;
mod evidence;
mod findings;
mod jobs;
mod metrics;
mod settings;

use core_graph::{Edge, GraphStore, Node, SqliteGraphStore};
use findings::{Finding, FindingStore, NewFinding};
use jobs::{EvalResult, Job, JobStore};
use llm::LlmProvider;
use serde::Serialize;
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};

/// Stores managed by the Tauri runtime. Graph and state spine are separate
/// databases (ADR-0008): the graph is a disposable ingest artifact, the spine
/// holds durable state.
struct AppState {
    graph: Mutex<SqliteGraphStore>,
    jobs: Mutex<JobStore>,
    findings: Mutex<FindingStore>,
    settings: Mutex<settings::SettingsStore>,
    decisions: Mutex<agents::DecisionLog>,
    extraction_caches: Mutex<ExtractionCaches>,
    /// Resolved filesystem roots of every ingested target this session —
    /// plugin discovery scans these, never the raw Connect input (#203).
    project_roots: Mutex<std::collections::BTreeSet<String>>,
    metrics: Mutex<metrics::MetricsStore>,
}

#[derive(Default)]
struct RepoExtractionCache {
    ts: adapters_lang_ts::IncrementalCache,
    python: adapters_lang_python::IncrementalCache,
    go: adapters_lang_go::IncrementalCache,
    java: adapters_lang_java::IncrementalCache,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct LayerBreakdown {
    ts: LayerSummary,
    python: LayerSummary,
    go: LayerSummary,
    java: LayerSummary,
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
        self.tf.add(other.tf);
        self.webext.add(other.webext);
        self.tools.add(other.tools);
    }

    fn files(self) -> u64 {
        self.ts.files + self.python.files + self.go.files + self.java.files + self.tf.files
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
    Ok(GraphStats {
        nodes: graph.node_count().map_err(|e| e.to_string())?,
        edges: graph.edge_count().map_err(|e| e.to_string())?,
    })
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

/// Discover plugin artifacts: `.cartograph/adapters/` inside every resolved
/// ingest root this session (never the raw Connect input — a GitHub URL or
/// manifest path is not a directory, #203 review), then the user-level
/// adapters directory. Project wins on id conflict. Enablement joins on the
/// exact artifact hash, so replaced bytes are disabled again. Discovery
/// never runs guest code.
fn discover_session_plugins<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
) -> Result<Vec<adapters_plugin_host::discovery::DiscoveredPlugin>, String> {
    let user_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("adapters");
    let roots: Vec<std::path::PathBuf> = state
        .project_roots
        .lock()
        .map_err(|e| e.to_string())?
        .iter()
        .map(std::path::PathBuf::from)
        .collect();
    Ok(adapters_plugin_host::discovery::discover(&roots, &user_dir))
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
    let mut statuses = Vec::with_capacity(discovered.len());
    for plugin in discovered {
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
    let job_id = {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        let job = jobs
            .enqueue(&format!("plugin-gate:{plugin_id}"))
            .map_err(|e| e.to_string())?;
        jobs.set_status(job.id, "running")
            .map_err(|e| e.to_string())?;
        let running = jobs.get(job.id).map_err(|e| e.to_string())?;
        emit_job(&app, &running);
        job.id
    };
    tauri::async_runtime::spawn_blocking(move || plugin_gate_blocking(&plugin_id, job_id, &app))
        .await
        .map_err(|e| e.to_string())?
}

/// The gate pipeline behind one already-running `plugin-gate:{id}` job —
/// shared by the command above and the Jobs retry path, so an interrupted
/// or failed gate reruns instead of dead-ending. Cancellation is honored
/// before the verdict persists: a cancelled job never changes the trusted
/// artifact state.
fn plugin_gate_blocking<R: tauri::Runtime>(
    plugin_id: &str,
    job_id: i64,
    app: &tauri::AppHandle<R>,
) -> Result<serde_json::Value, String> {
    let state = app.state::<AppState>();
    let fail = |error: String| -> String {
        report_failure(app, &state, job_id, &error);
        error
    };

    report_progress(app, &state, job_id, "discover", 10.0).map_err(&fail)?;
    let plugin = discover_session_plugins(app, &state)
        .map_err(&fail)?
        .into_iter()
        .find(|plugin| plugin.id == plugin_id)
        .ok_or_else(|| fail(format!("no discovered plugin with id {plugin_id}")))?;
    // Hash the bytes actually gated, not the discovery-time snapshot: the
    // verdict must bind to what ran even if the file changed in between.
    let wasm_bytes = std::fs::read(&plugin.path).map_err(|e| fail(e.to_string()))?;
    let content_hash = core_prov::content_hash(&wasm_bytes);

    report_progress(app, &state, job_id, "gate", 30.0).map_err(&fail)?;
    let corpus_path = plugin.path.with_extension("golden.json");
    let report = match std::fs::read_to_string(&corpus_path)
        .map_err(|e| e.to_string())
        .and_then(|text| {
            serde_json::from_str::<adapters_plugin_host::gate::GoldenCorpus>(&text)
                .map_err(|e| e.to_string())
        }) {
        Ok(corpus) => {
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
    if job_cancelled(&state, job_id) {
        return Err("cancelled".to_string());
    }

    report_progress(app, &state, job_id, "record", 90.0).map_err(&fail)?;
    let report_json = serde_json::to_value(&report).map_err(|e| fail(e.to_string()))?;
    {
        let mut settings_store = state.settings.lock().map_err(|e| e.to_string())?;
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
    let job = jobs
        .finish(job_id, &[format!("gate:{plugin_id}@{content_hash}")])
        .map_err(|e| e.to_string())?;
    emit_job(app, &job);
    if job.status != "done" {
        return Err("cancelled".to_string());
    }
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
}

/// What the current system contains, derived from the graph's own facts
/// (never from history logs, which survive a clear): each distinct repo
/// any node's evidence cites, with its recorded commit identity. Evidence
/// is the source so infra-only (Resource) and manifest-only (Extension)
/// repos count too, not just ones with File nodes (#187 review).
/// Deterministic: sorted by repo.
#[tauri::command]
fn system_contents(state: State<'_, AppState>) -> Result<Vec<SystemRepo>, String> {
    let graph = state.graph.lock().map_err(|e| e.to_string())?;
    let nodes = graph.all_nodes().map_err(|e| e.to_string())?;
    Ok(system_contents_of(&nodes))
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
        .map(|(repo, commit)| SystemRepo { repo, commit })
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

/// Persist one human accept/reject decision for a staged T3 proposal.
#[tauri::command]
fn record_agent_decision(
    proposal: agents::AgentProposal,
    decision: agents::ProposalDecision,
    note: Option<String>,
    state: State<'_, AppState>,
) -> Result<agents::DecisionRecord, String> {
    let mut decisions = state.decisions.lock().map_err(|error| error.to_string())?;
    decisions
        .record(&proposal, decision, note.as_deref())
        .map_err(|error| error.to_string())
}

/// All durable T3 curation decisions, newest first.
#[tauri::command]
fn list_agent_decisions(state: State<'_, AppState>) -> Result<Vec<agents::DecisionRecord>, String> {
    let decisions = state.decisions.lock().map_err(|error| error.to_string())?;
    decisions.list().map_err(|error| error.to_string())
}

/// Decisions whose exact evidence/candidate basis still matches a re-ingest.
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
    layers: LayerBreakdown,
    delta: DeltaSummary,
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
        let (mut extraction, stats) = adapters_lang_ts::extract_dir_incremental_with_progress(
            root,
            &ts_id,
            &mut cache.ts,
            &mut ts_progress,
        )
        .map_err(|e| e.to_string())?;
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
        nodes: extraction.nodes.len() as u64,
        edges: extraction.edges.len() as u64,
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
            nodes: webext.nodes.len() as u64,
            edges: webext.edges.len() as u64,
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
        layers.webext.nodes += idb.nodes.len() as u64;
        layers.webext.edges += idb.edges.len() as u64;
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
            nodes: python.nodes.len() as u64,
            edges: python.edges.len() as u64,
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
            nodes: go.nodes.len() as u64,
            edges: go.edges.len() as u64,
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
            nodes: java.nodes.len() as u64,
            edges: java.edges.len() as u64,
        };
        extraction.nodes.extend(java.nodes);
        extraction.edges.extend(java.edges);
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
            files: iac::terraform_file_count(root).map_err(|e| e.to_string())?,
            nodes: tf.nodes.len() as u64,
            edges: tf.edges.len() as u64,
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
    layers.ts.nodes += stitched.nodes.len() as u64;
    layers.ts.edges += stitched.edges.len() as u64;
    extraction.nodes.extend(stitched.nodes);
    extraction.edges.extend(stitched.edges);
    let endpoint_ids: Vec<String> = extraction
        .nodes
        .iter()
        .filter(|n| n.label == "Endpoint")
        .map(|n| n.id.clone())
        .collect();
    let fetched = events::stitch_fetches(&extraction.fetch_sites, &endpoint_ids, &cfg, &ev_id);
    layers.ts.nodes += fetched.nodes.len() as u64;
    layers.ts.edges += fetched.edges.len() as u64;
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
            nodes: tool_facts.nodes.len() as u64,
            edges: tool_facts.edges.len() as u64,
        };
        // Config files an adapter already owns (a `.ts`-authored vite
        // config, a webext manifest) keep the adapter's richer File node;
        // the DEFINED_IN edge targets the same id either way.
        let known_files: std::collections::BTreeSet<String> = extraction
            .nodes
            .iter()
            .filter(|node| node.label == "File")
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ReconcileStats {
    inserted_or_updated: u64,
    unchanged: u64,
    deleted: u64,
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

fn load_into_graph(
    graph: &mut SqliteGraphStore,
    extraction: &adapters_lang_ts::Extraction,
    repo: &str,
    root: &std::path::Path,
    commit: &str,
) -> Result<ReconcileStats, String> {
    let repo_prov = core_prov::Provenance::new(
        core_prov::Tier::Deterministic,
        core_prov::ConfidenceTier::Confirmed,
        vec![],
        "app.ingest",
        root.to_string_lossy().as_bytes(),
    )
    .expect("within ceiling");
    let repo_node = Node {
        id: format!("repo:{repo}"),
        label: "Repo".into(),
        props: serde_json::json!({
            "root": root.to_string_lossy(),
            "commit": commit,
            "prov": serde_json::to_value(repo_prov).expect("serializes"),
        }),
    };
    let mut current_nodes = extraction
        .nodes
        .iter()
        .cloned()
        .map(|node| (node.id.clone(), node))
        .collect::<std::collections::BTreeMap<_, _>>();
    current_nodes.insert(repo_node.id.clone(), repo_node);
    let current_edges = extraction
        .edges
        .iter()
        .cloned()
        .map(|edge| (edge_key(&edge), edge))
        .collect::<std::collections::BTreeMap<_, _>>();
    let existing_nodes = graph
        .all_nodes()
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|node| (node.id.clone(), node))
        .collect::<std::collections::BTreeMap<_, _>>();
    let existing_edges = graph
        .all_edges()
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|edge| (edge_key(&edge), edge))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut stats = ReconcileStats::default();
    let mut remaining_edge_keys = std::collections::BTreeSet::new();
    for (key, edge) in &existing_edges {
        if fact_owned_by_repo(&edge.props, repo) && !current_edges.contains_key(key) {
            graph
                .delete_edge(&edge.src, &edge.dst, &edge.label)
                .map_err(|error| error.to_string())?;
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
            graph.delete_node(id).map_err(|error| error.to_string())?;
            stats.deleted += 1;
        }
    }
    for node in current_nodes.values() {
        if existing_nodes.get(&node.id) == Some(node) {
            stats.unchanged += 1;
        } else {
            graph.put_node(node).map_err(|e| e.to_string())?;
            stats.inserted_or_updated += 1;
        }
    }
    for (key, edge) in &current_edges {
        if existing_edges.get(key) == Some(edge) {
            stats.unchanged += 1;
        } else {
            graph.put_edge(edge).map_err(|e| e.to_string())?;
            stats.inserted_or_updated += 1;
        }
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
    let mut hashes = graph
        .all_nodes()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter_map(|node| hash(&node.props).map(|hash| format!("node:{}:{hash}", node.id)))
        .chain(
            graph
                .all_edges()
                .map_err(|error| error.to_string())?
                .into_iter()
                .filter_map(|edge| {
                    hash(&edge.props)
                        .map(|hash| format!("edge:{}:{}:{}:{hash}", edge.src, edge.dst, edge.label))
                }),
        )
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
fn stitch_backings(graph: &mut SqliteGraphStore) -> Result<u64, String> {
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
        }
    }
    for edge in candidates.values() {
        graph.put_edge(edge).map_err(|error| error.to_string())?;
    }
    Ok(candidates.len() as u64)
}

/// Reconcile explicit found-ADR target ids against the complete graph.
/// Each repo is initially extracted in isolation, but decisions in a docs
/// repo may govern facts loaded later from another repo in the same system.
/// A rescan first drops links previously owned by that repo's found ADRs and
/// removes ADR nodes whose source file disappeared, so re-ingest cannot retain
/// declarations that are no longer present.
fn relink_found_adrs(graph: &mut SqliteGraphStore) -> Result<u64, String> {
    let repos = graph
        .nodes_with_label("Repo")
        .map_err(|error| error.to_string())?;
    let candidates = graph
        .all_nodes()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|node| node.label != "ADR")
        .collect::<Vec<_>>();
    let mut linked = 0;
    for repo_node in repos {
        let Some(repo) = repo_node.id.strip_prefix("repo:") else {
            continue;
        };
        let Some(root) = repo_node.props["root"].as_str().map(std::path::Path::new) else {
            continue;
        };
        if !root.is_dir() {
            continue;
        }
        let commit = repo_node.props["commit"].as_str().unwrap_or("workdir");
        let facts = spec::extract_found_adrs(root, repo, commit, &candidates)
            .map_err(|error| error.to_string())?;

        let adr_prefix = format!("adr:{repo}@");
        let existing_ids = graph
            .nodes_with_label("ADR")
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|node| {
                node.id.starts_with(&adr_prefix) && node.props["origin"].as_str() == Some("found")
            })
            .map(|node| node.id)
            .collect::<std::collections::BTreeSet<_>>();
        let current_ids = facts
            .nodes
            .iter()
            .map(|node| node.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        for adr_id in existing_ids.union(&current_ids) {
            graph
                .delete_edges_from_with_label(adr_id, "DECIDES")
                .map_err(|error| error.to_string())?;
        }
        for stale_id in existing_ids.difference(&current_ids) {
            graph
                .delete_node(stale_id)
                .map_err(|error| error.to_string())?;
        }
        for node in facts.nodes {
            graph.put_node(&node).map_err(|error| error.to_string())?;
        }
        for edge in facts.edges {
            graph.put_edge(&edge).map_err(|error| error.to_string())?;
            linked += 1;
        }
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
    let gaps = nodes.iter().filter(|node| spec::is_gap_node(node)).count() as u64
        + edges.iter().filter(|edge| spec::is_gap_edge(edge)).count() as u64;
    // Drift counts nodes only, matching the drift register's own headline
    // (`drift_register` counts drift nodes; CONFLICTS/DRIFTS_FROM edges are
    // supporting assertions of the same finding, not additional findings).
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
        (
            graph.all_nodes().map_err(|e| e.to_string())?,
            graph.all_edges().map_err(|e| e.to_string())?,
        )
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
#[tauri::command]
fn preflight(
    path: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<ingest::preflight::PreflightReport, String> {
    preflight_blocking(&path, &app, &state)
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
) -> Result<ingest::preflight::PreflightReport, String> {
    let root = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    if let Ok(mut roots) = state.project_roots.lock() {
        roots.insert(root.display().to_string());
    }
    let repo = format!(
        "local/{}",
        root.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
    );
    let claims: Vec<ingest::preflight::PluginCoverage> =
        active_plugins_for_root(app, state, &root)?
            .into_iter()
            .map(|plugin| ingest::preflight::PluginCoverage {
                plugin_id: plugin.plugin_id,
                extensions: plugin.extensions,
            })
            .collect();
    let report =
        ingest::preflight::preflight_with_plugins(&root, &claims).map_err(|e| e.to_string())?;
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
        .replace_for(&repo, ingest::preflight::DETECTOR_ID, &batch)
        .map_err(|e| e.to_string())?;
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
        (
            graph.all_nodes().map_err(|e| e.to_string())?,
            graph.all_edges().map_err(|e| e.to_string())?,
        )
    };
    // Only layers this ingest actually contained are in scope — a
    // zero-file layer's extractor did not run, so it reports null
    // coverage (not applicable), never a misleading 0%.
    let scope: std::collections::BTreeMap<String, u64> = [
        ("t0.adapter-ts", layers.ts.files),
        ("t0.adapter-python", layers.python.files),
        ("t0.adapter-go", layers.go.files),
        ("t0.adapter-java", layers.java.files),
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

/// A closure that loads the exact text of an evidence span, or None.
type SpanReader = Box<dyn Fn(&core_prov::EvidenceRef) -> Option<String> + Send>;

/// Whole-graph projection plus an evidence reader bound to the ingested
/// repo roots — the escalation assembly needs both.
fn graph_and_reader(state: &AppState) -> Result<(Vec<Node>, Vec<Edge>, SpanReader), String> {
    let graph = state.graph.lock().map_err(|e| e.to_string())?;
    let nodes = graph.all_nodes().map_err(|e| e.to_string())?;
    let edges = graph.all_edges().map_err(|e| e.to_string())?;
    let roots: std::collections::BTreeMap<String, String> = nodes
        .iter()
        .filter(|node| node.label == "Repo")
        .filter_map(|node| {
            let repo = node.id.strip_prefix("repo:")?.to_string();
            let root = node.props["root"].as_str()?.to_string();
            Some((repo, root))
        })
        .collect();
    let reader: SpanReader = Box::new(move |reference: &core_prov::EvidenceRef| {
        let root = roots.get(&reference.repo)?;
        // Exactly the cited bytes, sliced before any lossy conversion —
        // an escalation payload never carries more than its citations.
        evidence::read_span_exact(
            std::path::Path::new(root),
            &reference.path,
            &(reference.byte_start..reference.byte_end),
        )
        .ok()
    });
    Ok((nodes, edges, reader))
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
fn gap_strategies(
    gap_id: String,
    state: State<'_, AppState>,
) -> Result<escalation::GapStrategyReport, String> {
    let (nodes, edges, reader) = graph_and_reader(&state)?;
    let task = escalation::assemble_task(&nodes, &edges, &gap_id, "escalate:preview", &reader)?;
    let gap = nodes
        .iter()
        .find(|node| node.id == gap_id)
        .ok_or_else(|| format!("no gap named '{gap_id}'"))?;
    let cloud_allowed = {
        let settings_store = state.settings.lock().map_err(|e| e.to_string())?;
        settings_store
            .egress_policy()
            .map_err(|e| e.to_string())?
            .cloud_allowed(llm::AnalysisTier::Agentic)
    };
    // Exact payload size from the broker's own preview against a local
    // firewall — same redaction, zero egress.
    let firewall = llm::EgressFirewall::new(llm::EgressPolicy::default());
    let local = llm::OllamaProvider::local_default().map_err(|e| e.to_string())?;
    let preview = agents::AgentBroker::bounded_default()
        .preview(&local, &firewall, &task)
        .map_err(|e| e.to_string())?;
    let payload_bytes = serde_json::to_vec(&preview.payload)
        .map_err(|e| e.to_string())?
        .len() as u64;
    Ok(escalation::strategies(
        &task,
        gap,
        cloud_allowed,
        payload_bytes,
    ))
}

/// The exact redacted payload a cloud escalation would send (#120): the
/// one-action consent dialog renders this preview, and the grant is bound
/// to its hash. Zero egress — preview never invokes the provider.
#[tauri::command]
fn escalation_preview(
    gap_id: String,
    state: State<'_, AppState>,
) -> Result<llm::EgressPreview, String> {
    let (nodes, edges, reader) = graph_and_reader(&state)?;
    let task = escalation::assemble_task(
        &nodes,
        &edges,
        &gap_id,
        &format!("escalate:{gap_id}"),
        &reader,
    )?;
    let policy = {
        let settings_store = state.settings.lock().map_err(|e| e.to_string())?;
        settings_store.egress_policy().map_err(|e| e.to_string())?
    };
    let provider = escalation_provider("cloud")?;
    agents::AgentBroker::bounded_default()
        .preview(provider.as_ref(), &llm::EgressFirewall::new(policy), &task)
        .map_err(|e| e.to_string())
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
) -> Result<agents::AgentProposal, String> {
    let job_id = {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        let job = jobs
            .enqueue(&format!("escalate:{gap_id}:{mode}"))
            .map_err(|e| e.to_string())?;
        jobs.set_status(job.id, "running")
            .map_err(|e| e.to_string())?;
        let running = jobs.get(job.id).map_err(|e| e.to_string())?;
        emit_job(&app, &running);
        job.id
    };
    let fail = |error: String| -> String {
        report_failure(&app, &state, job_id, &error);
        error
    };

    report_progress(&app, &state, job_id, "context", 20.0).map_err(&fail)?;
    // Scoped so the span reader (non-trivial capture) drops before any await.
    let (task, facts_before) = {
        let (nodes, edges, reader) = graph_and_reader(&state).map_err(&fail)?;
        let facts = nodes.len() + edges.len();
        let task = escalation::assemble_task(
            &nodes,
            &edges,
            &gap_id,
            &format!("escalate:{gap_id}"),
            &reader,
        )
        .map_err(&fail)?;
        (task, facts)
    };

    report_progress(&app, &state, job_id, "model", 70.0).map_err(&fail)?;
    let policy = {
        let settings_store = state.settings.lock().map_err(|e| e.to_string())?;
        settings_store.egress_policy().map_err(|e| e.to_string())?
    };
    let provider = escalation_provider(&mode).map_err(&fail)?;
    let firewall = llm::EgressFirewall::new(policy);
    let broker = agents::AgentBroker::bounded_default();
    // A cloud run re-derives the preview and only proceeds when the user's
    // approved hash matches this exact payload (one-action consent).
    let consent = if mode == "cloud" {
        let preview = broker
            .preview(provider.as_ref(), &firewall, &task)
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
            .preview(provider.as_ref(), &firewall, &task)
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
    if job_cancelled(&state, job_id) {
        return Err("cancelled".to_string());
    }
    let proposal = tauri::async_runtime::spawn_blocking(move || {
        broker
            .propose(provider.as_ref(), &firewall, &task, consent.as_ref())
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|e| fail(e.to_string()))?
    .map_err(&fail)?;

    report_progress(&app, &state, job_id, "validate", 90.0).map_err(&fail)?;
    if payload_bytes > 0 {
        let mut settings_store = state.settings.lock().map_err(|e| e.to_string())?;
        settings_store
            .record_egress(&proposal.provenance.extractor_id, payload_bytes)
            .map_err(|e| e.to_string())?;
    }
    // Propose-only, provably: the graph projection is unchanged.
    {
        let graph = state.graph.lock().map_err(|e| e.to_string())?;
        let after = graph.all_nodes().map_err(|e| e.to_string())?.len()
            + graph.all_edges().map_err(|e| e.to_string())?.len();
        debug_assert_eq!(
            facts_before, after,
            "escalation must never mutate the graph"
        );
    }

    let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    let job = jobs
        .finish(job_id, &[format!("proposal:{gap_id}")])
        .map_err(|e| e.to_string())?;
    emit_job(&app, &job);
    if job.status != "done" {
        return Err("cancelled".to_string());
    }
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
) -> Result<agents::BatchOutcome, String> {
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
    let job_id = {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        let job = jobs
            .enqueue(&format!("escalate-class:{}:{mode}", gap_ids.len()))
            .map_err(|e| e.to_string())?;
        jobs.set_status(job.id, "running")
            .map_err(|e| e.to_string())?;
        let running = jobs.get(job.id).map_err(|e| e.to_string())?;
        emit_job(&app, &running);
        job.id
    };
    let fail = |error: String| -> String {
        report_failure(&app, &state, job_id, &error);
        error
    };

    report_progress(&app, &state, job_id, "context", 5.0).map_err(&fail)?;
    // Assemble every instance's task up front (the span reader borrows the
    // graph snapshot); per-instance assembly errors join the outcome as
    // failures instead of aborting the class.
    let (tasks, facts_before) = {
        let (nodes, edges, reader) = graph_and_reader(&state).map_err(&fail)?;
        let facts = nodes.len() + edges.len();
        let tasks: Vec<(String, Result<agents::AgentTask, String>)> = gap_ids
            .iter()
            .map(|gap_id| {
                let task = escalation::assemble_task(
                    &nodes,
                    &edges,
                    gap_id,
                    &format!("escalate:{gap_id}"),
                    &reader,
                );
                (gap_id.clone(), task)
            })
            .collect();
        (tasks, facts)
    };

    let policy = {
        let settings_store = state.settings.lock().map_err(|e| e.to_string())?;
        settings_store.egress_policy().map_err(|e| e.to_string())?
    };
    let provider = escalation_provider(&mode).map_err(&fail)?;
    let firewall = llm::EgressFirewall::new(policy);
    let broker = agents::AgentBroker::bounded_default();

    let batch_app = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let state = batch_app.state::<AppState>();
        let total = tasks.len();
        broker.propose_batch(
            provider.as_ref(),
            &firewall,
            tasks,
            || job_cancelled(&state, job_id),
            |index, _| {
                let _ = report_progress(
                    &batch_app,
                    &state,
                    job_id,
                    &format!("escalate {}/{total}", index + 1),
                    5.0 + (index as f64 / total as f64) * 90.0,
                );
            },
        )
    })
    .await
    .map_err(|e| fail(e.to_string()))?;

    // Propose-only, provably: the graph projection is unchanged.
    {
        let graph = state.graph.lock().map_err(|e| e.to_string())?;
        let after = graph.all_nodes().map_err(|e| e.to_string())?.len()
            + graph.all_edges().map_err(|e| e.to_string())?.len();
        debug_assert_eq!(
            facts_before, after,
            "class escalation must never mutate the graph"
        );
    }

    let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    let job = jobs
        .finish(
            job_id,
            &[format!(
                "proposals:{} failures:{}",
                outcome.proposals.len(),
                outcome.failures.len()
            )],
        )
        .map_err(|e| e.to_string())?;
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

/// A live "what it's doing right now" ping (#209) — current adapter/file
/// being read. Deliberately not part of the durable `Job` row: it's
/// best-effort and streamed far more often than a SQLite write per file
/// would tolerate, so it rides its own event instead of `job://changed`.
#[derive(Clone, Serialize)]
struct JobDetail<'a> {
    id: i64,
    detail: &'a str,
}

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
    job_id: i64,
    stage: &str,
    percent: f64,
) -> Result<(), String> {
    let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    let job = jobs
        .set_progress(job_id, stage, percent)
        .map_err(|e| e.to_string())?;
    emit_job(app, &job);
    Ok(())
}

/// Persist a job failure with its detail and notify the shell.
fn report_failure<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    job_id: i64,
    error: &str,
) {
    let Ok(mut jobs) = state.jobs.lock() else {
        return;
    };
    if let Ok(job) = jobs.fail(job_id, error) {
        emit_job(app, &job);
    }
}

/// True when the user cancelled the job — long work checks this between
/// stages and stops at the next safe boundary (content-addressed delta
/// ingest keeps the graph consistent, ADR-0014).
/// Complete `job_id` unless a concurrent cancel already won: `finish` only
/// transitions queued/running rows, so a cancelled job stays cancelled and
/// the pipeline reports it instead of returning a success summary (#166
/// review — with the UI responsive during adds, Cancel can race the worker).
fn finish_or_cancelled(
    state: &AppState,
    app: &tauri::AppHandle,
    job_id: i64,
) -> Result<(), String> {
    let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    let done = jobs.finish(job_id, &[]).map_err(|e| e.to_string())?;
    if done.status != "done" {
        return Err("cancelled".to_string());
    }
    emit_job(app, &done);
    Ok(())
}

fn job_cancelled(state: &AppState, job_id: i64) -> bool {
    state
        .jobs
        .lock()
        .ok()
        .and_then(|jobs| jobs.is_cancelled(job_id).ok())
        // Fail closed: an unreadable spine stops the worker too (#157
        // review) — a cancellation guard that defaults to "keep going"
        // is no guard at all.
        .unwrap_or(true)
}

/// The staged ingest pipeline behind `ingest_path` and `retry_job`: extract →
/// load → stitch, with progress events and cooperative cancellation.
fn run_ingest(
    path: &str,
    job_id: i64,
    app: &tauri::AppHandle,
    state: &AppState,
) -> Result<IngestSummary, String> {
    let fail = |error: String| -> String {
        report_failure(app, state, job_id, &error);
        error
    };
    let cancelled = || -> Result<(), String> {
        if job_cancelled(state, job_id) {
            // Cancelled by the user: status is already `cancelled`; the
            // pipeline just stops. Not a failure.
            return Err("cancelled".to_string());
        }
        Ok(())
    };

    // Local unversioned tree: identified by directory basename (two dirs
    // with the same basename still collide — real identity is `add_repo`).
    report_progress(app, state, job_id, "scan", 5.0)?;
    let root = std::fs::canonicalize(path).map_err(|e| fail(e.to_string()))?;
    if let Ok(mut roots) = state.project_roots.lock() {
        roots.insert(root.display().to_string());
    }
    let repo = format!(
        "local/{}",
        root.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
    );

    cancelled()?;
    report_progress(app, state, job_id, "extract", 15.0)?;
    let active_plugins = active_plugins_for_root(app, state, &root).map_err(&fail)?;
    let mut on_file = detail_throttle(app, job_id);
    let (extraction, layers, delta) = {
        let mut caches = state
            .extraction_caches
            .lock()
            .map_err(|e| fail(e.to_string()))?;
        let cache = caches.repos.entry(repo.clone()).or_default();
        extract_tree_incremental(
            &root,
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
        )
        .map_err(fail)?
    };

    cancelled()?;
    report_progress(app, state, job_id, "load", 70.0)?;
    {
        let mut graph = state.graph.lock().map_err(|e| fail(e.to_string()))?;
        load_into_graph(&mut graph, &extraction, &repo, &root, "workdir").map_err(&fail)?;
        report_progress(app, state, job_id, "stitch", 90.0)?;
        relink_found_adrs(&mut graph).map_err(&fail)?;
        stitch_backings(&mut graph).map_err(&fail)?;
    }

    record_ingest_metrics(
        state,
        job_id,
        &repo,
        "workdir",
        &layers,
        &std::collections::BTreeSet::from([repo.clone()]),
    )
    .map_err(&fail)?;

    // A cancel can land at any point after the last check; `finish` is
    // guarded to only transition a running job, so whichever outcome hit
    // the store first wins — read the row back to learn which.
    let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    let job = jobs
        .finish(job_id, &[format!("graph:{repo}@workdir")])
        .map_err(|e| e.to_string())?;
    emit_job(app, &job);
    if job.status != "done" {
        return Err("cancelled".to_string());
    }
    Ok(IngestSummary {
        job_id,
        files: layers.files(),
        nodes: extraction.nodes.len() as u64,
        edges: extraction.edges.len() as u64,
        layers,
        delta,
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

#[tauri::command]
async fn ingest_path(path: String, app: tauri::AppHandle) -> Result<IngestSummary, String> {
    off_ui_thread(move || ingest_path_blocking(path, app)).await
}

fn ingest_path_blocking(path: String, app: tauri::AppHandle) -> Result<IngestSummary, String> {
    let state = app.state::<AppState>();
    let job_id = {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        let job = jobs
            .enqueue(&format!("ingest:{path}"))
            .map_err(|e| e.to_string())?;
        jobs.set_status(job.id, "running")
            .map_err(|e| e.to_string())?;
        let running = jobs.get(job.id).map_err(|e| e.to_string())?;
        emit_job(&app, &running);
        job.id
    };
    run_ingest(&path, job_id, &app, &state)
}

/// Cancel a queued or running job; running work stops at its next stage
/// boundary (#117).
#[tauri::command]
fn cancel_job(id: i64, app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Job, String> {
    let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    let job = jobs.cancel(id).map_err(|e| e.to_string())?;
    emit_job(&app, &job);
    Ok(job)
}

/// Retry a failed or cancelled job, or resume an interrupted one: re-queues
/// the same row, then re-dispatches execution for kinds the shell can re-run
/// (`ingest:*` reuses the content-addressed cache, so a resume recomputes
/// only what the interrupted run didn't finish — ADR-0014).
#[tauri::command]
async fn retry_job(id: i64, app: tauri::AppHandle) -> Result<Job, String> {
    off_ui_thread(move || retry_job_blocking(id, app)).await
}

fn retry_job_blocking(id: i64, app: tauri::AppHandle) -> Result<Job, String> {
    let state = app.state::<AppState>();
    let kind = {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        let job = jobs.retry(id).map_err(|e| e.to_string())?;
        emit_job(&app, &job);
        job.kind
    };

    if let Some(path) = kind.strip_prefix("ingest:") {
        {
            let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
            jobs.set_status(id, "running").map_err(|e| e.to_string())?;
            let running = jobs.get(id).map_err(|e| e.to_string())?;
            emit_job(&app, &running);
        }
        let path = path.to_string();
        run_ingest(&path, id, &app, &state)?;
        let jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        return jobs.get(id).map_err(|e| e.to_string());
    }
    // A conformance gate re-runs whole (#206 review): the verdict re-binds
    // to whatever bytes are on disk now, which is exactly what a retry
    // after an interrupt or a fixed corpus should do.
    if let Some(plugin_id) = kind.strip_prefix("plugin-gate:") {
        {
            let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
            jobs.set_status(id, "running").map_err(|e| e.to_string())?;
            let running = jobs.get(id).map_err(|e| e.to_string())?;
            emit_job(&app, &running);
        }
        plugin_gate_blocking(plugin_id, id, &app)?;
        let jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        return jobs.get(id).map_err(|e| e.to_string());
    }
    if kind == "noop" {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        let job = jobs.finish(id, &[]).map_err(|e| e.to_string())?;
        emit_job(&app, &job);
        return Ok(job);
    }
    // add-repo / add-system re-dispatch needs their pipelines refactored
    // behind the same job-id seam; until then the caller is told explicitly
    // rather than silently doing nothing.
    let error = format!("retry re-dispatch not yet supported for kind '{kind}' — re-run the add");
    report_failure(&app, &state, id, &error);
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
    layers: LayerBreakdown,
    delta: DeltaSummary,
}

/// Clone a GitHub repo (read-only, shallow) and ingest it with its real
/// identity — every fact's evidence carries owner/name@sha (US-0001,
/// AC-0001). Auth per the ADR-0009 ladder; failures carry remediation and
/// leave no partial clone (AC-0003).
#[tauri::command]
async fn add_repo(url: String, app: tauri::AppHandle) -> Result<AddRepoSummary, String> {
    off_ui_thread(move || add_repo_blocking(url, app)).await
}

fn add_repo_blocking(url: String, app: tauri::AppHandle) -> Result<AddRepoSummary, String> {
    let state = app.state::<AppState>();
    let job_id = {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        let job = jobs
            .enqueue(&format!("add-repo:{url}"))
            .map_err(|e| e.to_string())?;
        jobs.set_status(job.id, "running")
            .map_err(|e| e.to_string())?;
        let running = jobs.get(job.id).map_err(|e| e.to_string())?;
        emit_job(&app, &running);
        job.id
    };
    let fail = |e: String, state: &State<'_, AppState>, job_id: i64| -> String {
        report_failure(&app, state, job_id, &e);
        e
    };

    let repos_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| fail(e.to_string(), &state, job_id))?
        .join("repos");
    let token = ingest::discover_token();
    let cloned = ingest::clone_repo(&url, &repos_dir, token.as_deref())
        .map_err(|e| fail(e.to_string(), &state, job_id))?;
    if job_cancelled(&state, job_id) {
        return Err("cancelled".to_string());
    }
    if let Ok(mut roots) = state.project_roots.lock() {
        roots.insert(cloned.path.display().to_string());
    }
    let active_plugins =
        active_plugins_for_root(&app, &state, &cloned.path).map_err(|e| fail(e, &state, job_id))?;
    let mut on_file = detail_throttle(&app, job_id);
    let (extraction, layers, delta) = {
        let mut caches = state
            .extraction_caches
            .lock()
            .map_err(|e| fail(e.to_string(), &state, job_id))?;
        let cache = caches.repos.entry(cloned.repo.clone()).or_default();
        extract_tree_incremental(
            &cloned.path,
            &cloned.repo,
            &cloned.commit_sha,
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
            cache,
            &active_plugins,
            &mut on_file,
        )
        .map_err(|e| fail(e, &state, job_id))?
    };
    if job_cancelled(&state, job_id) {
        return Err("cancelled".to_string());
    }
    {
        let mut graph = state
            .graph
            .lock()
            .map_err(|e| fail(e.to_string(), &state, job_id))?;
        load_into_graph(
            &mut graph,
            &extraction,
            &cloned.repo,
            &cloned.path,
            &cloned.commit_sha,
        )
        .map_err(|e| fail(e, &state, job_id))?;
        relink_found_adrs(&mut graph).map_err(|e| fail(e, &state, job_id))?;
        stitch_backings(&mut graph).map_err(|e| fail(e, &state, job_id))?;
    }
    record_ingest_metrics(
        &state,
        job_id,
        &cloned.repo,
        &cloned.commit_sha,
        &layers,
        &std::collections::BTreeSet::from([cloned.repo.clone()]),
    )
    .map_err(|e| fail(e, &state, job_id))?;
    finish_or_cancelled(&state, &app, job_id)?;
    Ok(AddRepoSummary {
        job_id,
        repo: cloned.repo,
        commit_sha: cloned.commit_sha,
        files: layers.files(),
        nodes: extraction.nodes.len() as u64,
        edges: extraction.edges.len() as u64,
        layers,
        delta,
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
    layers: LayerBreakdown,
    delta: DeltaSummary,
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

fn add_system_blocking(path: String, app: tauri::AppHandle) -> Result<AddSystemSummary, String> {
    let state = app.state::<AppState>();
    let job_id = {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        let job = jobs
            .enqueue(&format!("add-system:{path}"))
            .map_err(|e| e.to_string())?;
        jobs.set_status(job.id, "running")
            .map_err(|e| e.to_string())?;
        let running = jobs.get(job.id).map_err(|e| e.to_string())?;
        emit_job(&app, &running);
        job.id
    };
    let fail = |e: String, state: &State<'_, AppState>, job_id: i64| -> String {
        report_failure(&app, state, job_id, &e);
        e
    };

    let manifest_path =
        std::fs::canonicalize(&path).map_err(|e| fail(e.to_string(), &state, job_id))?;
    let manifest = ingest::manifest::SystemManifest::load(&manifest_path)
        .map_err(|e| fail(e.to_string(), &state, job_id))?;
    let base = manifest_dir(&manifest_path);
    let repos_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| fail(e.to_string(), &state, job_id))?
        .join("repos");
    let token = ingest::discover_token();

    let mut repos = Vec::new();
    let mut repo_identities = std::collections::BTreeSet::new();
    let (mut files, mut nodes, mut edges) = (0u64, 0u64, 0u64);
    let mut layers = LayerBreakdown::default();
    let mut delta = DeltaSummary::default();
    let mut on_file = detail_throttle(&app, job_id);
    for entry in &manifest.repos {
        if job_cancelled(&state, job_id) {
            return Err("cancelled".to_string());
        }
        let is_remote = manifest_entry_is_remote(&entry.url, base);
        let (root, repo, commit) = if is_remote {
            let cloned = ingest::clone_repo(&entry.url, &repos_dir, token.as_deref())
                .map_err(|e| fail(e.to_string(), &state, job_id))?;
            (cloned.path, cloned.repo, cloned.commit_sha)
        } else {
            let root = std::fs::canonicalize(base.join(&entry.url))
                .map_err(|e| fail(e.to_string(), &state, job_id))?;
            let name = root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (root, format!("local/{name}"), "workdir".to_string())
        };
        // Every manifest repo is a resolved ingest root: plugin discovery
        // scans them like any directly-ingested tree (#198).
        if let Ok(mut roots) = state.project_roots.lock() {
            roots.insert(root.display().to_string());
        }
        // state_json travels with the manifest, so it resolves against the
        // manifest dir — same rule as local repo paths.
        let state_path = entry.state_json.as_ref().map(|p| base.join(p));
        let pulumi_path = entry.pulumi_json.as_ref().map(|p| base.join(p));
        let trace_paths: Vec<std::path::PathBuf> =
            entry.otel_jsonl.iter().map(|p| base.join(p)).collect();
        let active_plugins =
            active_plugins_for_root(&app, &state, &root).map_err(|e| fail(e, &state, job_id))?;
        let (extraction, repo_layers, repo_delta) = {
            let mut caches = state
                .extraction_caches
                .lock()
                .map_err(|e| fail(e.to_string(), &state, job_id))?;
            let cache = caches.repos.entry(repo.clone()).or_default();
            extract_tree_incremental(
                &root,
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
            )
            .map_err(|e| fail(e, &state, job_id))?
        };
        files += repo_layers.files();
        nodes += extraction.nodes.len() as u64;
        edges += extraction.edges.len() as u64;
        layers.add(repo_layers);
        delta.add(
            repo_delta.recomputed_files,
            repo_delta.reused_files,
            repo_delta.deleted_files,
        );
        {
            let mut graph = state
                .graph
                .lock()
                .map_err(|e| fail(e.to_string(), &state, job_id))?;
            load_into_graph(&mut graph, &extraction, &repo, &root, &commit)
                .map_err(|e| fail(e, &state, job_id))?;
        }
        let sha12: String = commit.chars().take(12).collect();
        repos.push(format!("{repo}@{sha12}"));
        repo_identities.insert(repo.clone());
    }
    {
        // After every repo is in: infra from one repo can back channels
        // published by another.
        let mut graph = state
            .graph
            .lock()
            .map_err(|e| fail(e.to_string(), &state, job_id))?;
        relink_found_adrs(&mut graph).map_err(|e| fail(e, &state, job_id))?;
        stitch_backings(&mut graph).map_err(|e| fail(e, &state, job_id))?;
    }
    // One history record for the whole system; the per-repo identities are
    // the record's identity (a system has no single commit).
    record_ingest_metrics(
        &state,
        job_id,
        &repos.join(","),
        "system",
        &layers,
        &repo_identities,
    )
    .map_err(|e| fail(e, &state, job_id))?;
    finish_or_cancelled(&state, &app, job_id)?;
    Ok(AddSystemSummary {
        job_id,
        repos,
        files,
        nodes,
        edges,
        layers,
        delta,
    })
}

/// The resource/topology map artifact as Mermaid text (SPEC-00 §7, M2 exit
/// gate; channels join via observed BACKS edges at M6). Deterministic for
/// a given graph.
#[tauri::command]
fn export_topology(state: State<'_, AppState>) -> Result<String, String> {
    let graph = state.graph.lock().map_err(|e| e.to_string())?;
    let mut nodes = graph
        .nodes_with_label("Resource")
        .map_err(|e| e.to_string())?;
    nodes.extend(
        graph
            .nodes_with_label("Channel")
            .map_err(|e| e.to_string())?,
    );
    let edges = graph
        .edges_with_labels(spec::TOPOLOGY_EDGE_LABELS)
        .map_err(|e| e.to_string())?;
    Ok(spec::topology_mermaid(&nodes, &edges))
}

/// The flow-dossier artifact as Markdown (SPEC-00 §7, M3 exit gate):
/// every T0-traceable flow with per-hop tiers, Gap truncation, and score.
#[tauri::command]
fn export_flows(state: State<'_, AppState>) -> Result<String, String> {
    let graph = state.graph.lock().map_err(|e| e.to_string())?;
    let mut nodes = Vec::new();
    for label in flowtracer::FLOW_NODE_LABELS {
        nodes.extend(graph.nodes_with_label(label).map_err(|e| e.to_string())?);
    }
    let edges = graph
        .edges_with_labels(flowtracer::FLOW_EDGE_LABELS)
        .map_err(|e| e.to_string())?;
    let flows = flowtracer::trace(&nodes, &edges);
    Ok(spec::flow_dossier(&flows))
}

/// The traced flows as data (same graph slice as `export_flows`) — the UI
/// surfaces status and score per R-INT-2 without parsing the dossier.
#[tauri::command]
fn list_flows(state: State<'_, AppState>) -> Result<Vec<flowtracer::Flow>, String> {
    let graph = state.graph.lock().map_err(|e| e.to_string())?;
    let mut nodes = Vec::new();
    for label in flowtracer::FLOW_NODE_LABELS {
        nodes.extend(graph.nodes_with_label(label).map_err(|e| e.to_string())?);
    }
    let edges = graph
        .edges_with_labels(flowtracer::FLOW_EDGE_LABELS)
        .map_err(|e| e.to_string())?;
    Ok(flowtracer::trace(&nodes, &edges))
}

/// The anchor kinds the tracer sought in the current graph, with counts —
/// a zero-flow Inspector names what recovery looked for (#165, R-INT-4).
#[tauri::command]
fn list_flow_anchors(state: State<'_, AppState>) -> Result<Vec<flowtracer::AnchorProbe>, String> {
    let graph = state.graph.lock().map_err(|e| e.to_string())?;
    let mut nodes = Vec::new();
    for label in flowtracer::FLOW_NODE_LABELS {
        nodes.extend(graph.nodes_with_label(label).map_err(|e| e.to_string())?);
    }
    let edges = graph
        .edges_with_labels(flowtracer::FLOW_EDGE_LABELS)
        .map_err(|e| e.to_string())?;
    Ok(flowtracer::anchor_probes(&nodes, &edges))
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
        let mut nodes = Vec::new();
        for label in flowtracer::FLOW_NODE_LABELS {
            nodes.extend(
                graph
                    .nodes_with_label(label)
                    .map_err(|error| error.to_string())?,
            );
        }
        // Computed channel gaps can be backed only by a T0 IaC Resource.
        // Resources are semantic candidates, not flow nodes, and any Channel
        // they imply is materialized only in the ephemeral approved overlay.
        nodes.extend(
            graph
                .nodes_with_label("Resource")
                .map_err(|error| error.to_string())?,
        );
        let edges = graph
            .edges_with_labels(flowtracer::FLOW_EDGE_LABELS)
            .map_err(|error| error.to_string())?;
        (nodes, edges)
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
    let nodes = graph.all_nodes().map_err(|error| error.to_string())?;
    let edges = graph.all_edges().map_err(|error| error.to_string())?;
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
    Ok(AtlasSnapshot {
        nodes: graph.all_nodes().map_err(|error| error.to_string())?,
        edges: graph.all_edges().map_err(|error| error.to_string())?,
    })
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
/// ingest root recorded on the `Repo` node (NG1: navigation, never edit).
#[tauri::command]
fn read_evidence(
    root: String,
    path: String,
    byte_start: u64,
    byte_end: u64,
) -> Result<EvidenceSource, String> {
    let window = evidence::read_source(std::path::Path::new(&root), &path, &(byte_start..byte_end))
        .map_err(|e| e.to_string())?;
    Ok(EvidenceSource {
        text: window.text,
        window_start: window.window_start,
        window_start_line: window.window_start_line,
        truncated: window.truncated,
    })
}

/// Open a URL in the system browser — never inside the webview (#154).
fn open_external(url: &str) {
    #[cfg(target_os = "macos")]
    let launcher = "open";
    #[cfg(target_os = "linux")]
    let launcher = "xdg-open";
    #[cfg(target_os = "windows")]
    let launcher = "explorer";
    let _ = std::process::Command::new(launcher).arg(url).spawn();
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
            let mut jobs = JobStore::open(&state_path)?;
            // Jobs left running by a dead process become explicit
            // `interrupted` rows — resumable, never silently stuck (#117).
            jobs.recover_interrupted()?;
            let findings = FindingStore::open(&state_path)?;
            let tier_settings = settings::SettingsStore::open(&state_path)?;
            let decisions = agents::DecisionLog::open(&state_path)?;
            let recovery_metrics = metrics::MetricsStore::open(&state_path)?;
            app.manage(AppState {
                graph: Mutex::new(graph),
                jobs: Mutex::new(jobs),
                findings: Mutex::new(findings),
                settings: Mutex::new(tier_settings),
                decisions: Mutex::new(decisions),
                extraction_caches: Mutex::new(ExtractionCaches::default()),
                project_roots: Mutex::new(std::collections::BTreeSet::new()),
                metrics: Mutex::new(recovery_metrics),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
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
            record_agent_decision,
            list_agent_decisions,
            reapply_agent_decisions,
            record_assertion_decision,
            list_assertion_decisions,
            ingest_path,
            cancel_job,
            retry_job,
            preflight,
            findings_summary,
            list_findings,
            get_settings,
            set_tier_enabled,
            set_tier_provider,
            grant_cloud_consent,
            revoke_cloud_consent,
            egress_summary,
            cloud_disclosure,
            ingest_history,
            extractor_coverage,
            gap_strategies,
            escalation_preview,
            run_escalation,
            run_class_escalation,
            list_nodes,
            atlas_snapshot,
            read_evidence,
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
mod tests {
    use core_graph::{Edge, GraphStore, Node, SqliteGraphStore};
    use llm::{Embedding, Locality, ProviderCaps, ProviderError};

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
        let edges = vec![Edge {
            src: "svc:api".into(),
            dst: "adr:3".into(),
            label: "CONFLICTS".into(),
            props: serde_json::json!({ "prov": confirmed }),
        }];
        let summary = super::summarize_register(&nodes, &edges, 2, 1);
        assert_eq!(summary.gaps, 1);
        // One drift finding: the CONFLICTS edge supports the drift node, it
        // is not a second finding (parity with drift_register's count).
        assert_eq!(summary.drift, 1);
        assert_eq!(summary.unsupported, 2);
        assert_eq!(summary.no_evidence, 1);
        assert_eq!(summary.open_findings, 4); // 1 gap + 2 unsupported + 1 no-evidence
        assert_eq!(summary.graph_facts, 4);
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
        crate::relink_found_adrs(&mut store).unwrap();

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
        crate::relink_found_adrs(&mut store).unwrap();
        assert!(store.edges_with_labels(&["DECIDES"]).unwrap().is_empty());

        // Removing the source file reconciles the found ADR node as well.
        std::fs::remove_file(docs.join("docs/adr/ADR-0001-service.md")).unwrap();
        crate::relink_found_adrs(&mut store).unwrap();
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
                    commit: "a1b2c3d".into(),
                },
                crate::SystemRepo {
                    repo: "local/infra".into(),
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
        // a missing row reads as cancelled, so the worker stops instead of
        // continuing to write graph facts after cancellation.
        let dir = tempfile::tempdir().unwrap();
        let mut jobs = crate::jobs::JobStore::open(dir.path().join("state.db")).unwrap();
        let job = jobs.enqueue("ingest:/big").unwrap();
        jobs.set_status(job.id, "running").unwrap();
        jobs.cancel(job.id).unwrap();
        assert_eq!(jobs.clear_finished().unwrap(), 1);
        // The worker's next boundary check on the vanished row: cancelled.
        assert!(jobs.is_cancelled(job.id).unwrap());
    }

    #[test]
    fn cancelled_add_jobs_never_flip_back_to_done() {
        // #166 review: with the UI responsive during an add, Cancel can race
        // the worker. The terminal transition must be atomic — a cancelled
        // job stays cancelled and the pipeline reports it, never a success.
        let dir = tempfile::tempdir().unwrap();
        let mut jobs = crate::jobs::JobStore::open(dir.path().join("state.db")).unwrap();
        let job = jobs.enqueue("add-repo:https://example.test/a").unwrap();
        jobs.set_status(job.id, "running").unwrap();
        jobs.cancel(job.id).unwrap();
        // The worker's completion attempt must not overwrite the cancel.
        let after = jobs.finish(job.id, &[]).unwrap();
        assert_eq!(after.status, "cancelled");

        // And an uncancelled run completes normally through the same path.
        let ok = jobs.enqueue("add-repo:https://example.test/b").unwrap();
        jobs.set_status(ok.id, "running").unwrap();
        assert_eq!(jobs.finish(ok.id, &[]).unwrap().status, "done");
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
        assert_eq!(crate::stitch_backings(&mut store).unwrap(), 1);
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
        assert_eq!(crate::stitch_backings(&mut store).unwrap(), 0);
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

        let cloned = ingest::clone_repo(
            &format!("file://{}", bare.display()),
            &dir.path().join("clones"),
            None,
        )
        .unwrap();
        assert_eq!(cloned.repo, "local/shop");
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
        assert_eq!(ev["repo"], "local/shop");
        assert_eq!(ev["commit_sha"].as_str().unwrap(), cloned.commit_sha);

        // Repo node carries root + commit for per-repo evidence resolution.
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
        assert_eq!(repos[0].id, "repo:local/shop");
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
            let root = std::fs::canonicalize(dir.path().join(&entry.url)).unwrap();
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
            let root = std::fs::canonicalize(dir.path().join(&entry.url)).unwrap();
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
            let root = std::fs::canonicalize(dir.path().join(&entry.url)).unwrap();
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
        let backed = crate::stitch_backings(&mut store).unwrap();
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
        assert_eq!(crate::stitch_backings(&mut store).unwrap(), 1);
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
        assert_eq!(crate::stitch_backings(&mut store).unwrap(), 0);
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
            findings: std::sync::Mutex::new(super::FindingStore::open(&state_path).unwrap()),
            settings: std::sync::Mutex::new(
                super::settings::SettingsStore::open(&state_path).unwrap(),
            ),
            decisions: std::sync::Mutex::new(agents::DecisionLog::open(&state_path).unwrap()),
            extraction_caches: std::sync::Mutex::new(super::ExtractionCaches::default()),
            project_roots: std::sync::Mutex::new(roots),
            metrics: std::sync::Mutex::new(
                super::metrics::MetricsStore::open(&state_path).unwrap(),
            ),
        });
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let hash = core_prov::content_hash(OK_ADAPTER);

        // Enqueue a gate job, then cancel before the pipeline runs.
        let job_id = {
            let mut jobs = state.jobs.lock().unwrap();
            let job = jobs.enqueue("plugin-gate:t0.plugin-fixture").unwrap();
            jobs.set_status(job.id, "running").unwrap();
            jobs.cancel(job.id).unwrap();
            job.id
        };
        let result = super::plugin_gate_blocking("t0.plugin-fixture", job_id, &handle);
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

        // Retry re-queues the same durable row and re-dispatches the gate
        // (the `plugin-gate:` branch of retry_job_blocking runs exactly
        // this sequence over the same helper).
        {
            let mut jobs = state.jobs.lock().unwrap();
            jobs.retry(job_id).unwrap();
            jobs.set_status(job_id, "running").unwrap();
        }
        super::plugin_gate_blocking("t0.plugin-fixture", job_id, &handle)
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
            findings: std::sync::Mutex::new(super::FindingStore::open(&state_path).unwrap()),
            settings: std::sync::Mutex::new(
                super::settings::SettingsStore::open(&state_path).unwrap(),
            ),
            decisions: std::sync::Mutex::new(agents::DecisionLog::open(&state_path).unwrap()),
            extraction_caches: std::sync::Mutex::new(super::ExtractionCaches::default()),
            project_roots: std::sync::Mutex::new(std::collections::BTreeSet::new()),
            metrics: std::sync::Mutex::new(
                super::metrics::MetricsStore::open(&state_path).unwrap(),
            ),
        });
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let root = std::fs::canonicalize(&project).unwrap();
        let root_key = root.display().to_string();
        let path_arg = project.to_string_lossy().into_owned();

        // Before: Ruby is uncovered, with the request-adapter action, and
        // the finding persists to the register.
        let before = super::preflight_blocking(&path_arg, &handle, &state).unwrap();
        let finding = before
            .unsupported
            .iter()
            .find(|f| f.kind == "uncovered-language" && f.message.contains("Ruby"))
            .expect("Ruby surfaces as uncovered");
        assert_eq!(finding.request_adapter.as_deref(), Some("Ruby"));
        let repo = format!(
            "local/{}",
            root.file_name().map(|n| n.to_string_lossy()).unwrap()
        );
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
        let job_id = {
            let mut jobs = state.jobs.lock().unwrap();
            let job = jobs.enqueue("plugin-gate:t0.plugin-fixture").unwrap();
            jobs.set_status(job.id, "running").unwrap();
            job.id
        };
        let report = super::plugin_gate_blocking("t0.plugin-fixture", job_id, &handle).unwrap();
        assert_eq!(report["passed"], serde_json::json!(true));
        let hash = core_prov::content_hash(OK_ADAPTER);
        state
            .settings
            .lock()
            .unwrap()
            .set_plugin_enabled(&root_key, "t0.plugin-fixture", &hash, true)
            .unwrap();

        // Re-scan: the plugin counts as coverage and the finding closes.
        let after = super::preflight_blocking(&path_arg, &handle, &state).unwrap();
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
            findings: std::sync::Mutex::new(super::FindingStore::open(&state_path).unwrap()),
            settings: std::sync::Mutex::new(
                super::settings::SettingsStore::open(&state_path).unwrap(),
            ),
            decisions: std::sync::Mutex::new(agents::DecisionLog::open(&state_path).unwrap()),
            extraction_caches: std::sync::Mutex::new(super::ExtractionCaches::default()),
            project_roots: std::sync::Mutex::new(std::collections::BTreeSet::from([
                root_a.display().to_string(),
                root_b.display().to_string(),
            ])),
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
