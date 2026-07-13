//! Cartograph desktop shell (M0): boots the webview, owns the graph store and
//! the durable job spine, and exposes the first Tauri commands.

// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod evidence;
mod jobs;

use core_graph::{Edge, GraphStore, Node, SqliteGraphStore};
use jobs::{EvalResult, Job, JobStore};
use llm::LlmProvider;
use serde::Serialize;
use std::sync::Mutex;
use tauri::{Manager, State};

/// Stores managed by the Tauri runtime. Graph and state spine are separate
/// databases (ADR-0008): the graph is a disposable ingest artifact, the spine
/// holds durable state.
struct AppState {
    graph: Mutex<SqliteGraphStore>,
    jobs: Mutex<JobStore>,
    decisions: Mutex<agents::DecisionLog>,
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
    tf: LayerSummary,
}

impl LayerBreakdown {
    fn add(&mut self, other: Self) {
        self.ts.add(other.ts);
        self.tf.add(other.tf);
    }

    fn files(self) -> u64 {
        self.ts.files + self.tf.files
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

fn clear_graph_store(graph: &mut SqliteGraphStore) -> Result<GraphStats, String> {
    graph.clear().map_err(|e| e.to_string())?;
    Ok(GraphStats { nodes: 0, edges: 0 })
}

#[tauri::command]
fn clear_graph(state: State<'_, AppState>) -> Result<GraphStats, String> {
    let mut graph = state.graph.lock().map_err(|e| e.to_string())?;
    clear_graph_store(&mut graph)
}

#[tauri::command]
fn enqueue_job(kind: String, state: State<'_, AppState>) -> Result<Job, String> {
    let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    jobs.enqueue(&kind).map_err(|e| e.to_string())
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
}

/// The four-layer T0 pipeline over one tree: TypeScript, Terraform,
/// channel stitching, client fetch resolution — closed over so the
/// FK-enforcing store never sees a dangling endpoint.
fn extract_tree_with_summary(
    root: &std::path::Path,
    repo: &str,
    commit: &str,
    layers: &[String],
    manifest_env: &std::collections::BTreeMap<String, String>,
    state_json: Option<&std::path::Path>,
    otel_jsonl: &[std::path::PathBuf],
) -> Result<(adapters_lang_ts::Extraction, LayerBreakdown), String> {
    // Layer hints gate extractors (AC-0002): empty means everything; the
    // TS pass covers server/events/client, the HCL pass infra/cloud.
    let wants =
        |names: &[&str]| layers.is_empty() || names.iter().any(|n| layers.iter().any(|l| l == n));
    let ts_id = adapters_lang_ts::SourceId { repo, commit };
    let mut layers = LayerBreakdown::default();
    let mut extraction = if wants(&["server", "events", "client"]) {
        adapters_lang_ts::extract_dir(root, &ts_id).map_err(|e| e.to_string())?
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
    if wants(&["infra", "cloud"]) {
        let tf_id = iac::SourceId { repo, commit };
        let tf = iac::extract_dir(root, &tf_id).map_err(|e| e.to_string())?;
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
    extraction.close_over_endpoints();
    Ok((extraction, layers))
}

#[cfg(test)]
fn extract_tree(
    root: &std::path::Path,
    repo: &str,
    commit: &str,
    layers: &[String],
    manifest_env: &std::collections::BTreeMap<String, String>,
    state_json: Option<&std::path::Path>,
    otel_jsonl: &[std::path::PathBuf],
) -> Result<adapters_lang_ts::Extraction, String> {
    extract_tree_with_summary(
        root,
        repo,
        commit,
        layers,
        manifest_env,
        state_json,
        otel_jsonl,
    )
    .map(|(extraction, _)| extraction)
}

/// Load an extraction plus its `Repo` node (`repo:{identity}`, carrying the
/// tree root and commit so evidence reads resolve per repo).
fn load_into_graph(
    graph: &mut SqliteGraphStore,
    extraction: &adapters_lang_ts::Extraction,
    repo: &str,
    root: &std::path::Path,
    commit: &str,
) -> Result<(), String> {
    let repo_prov = core_prov::Provenance::new(
        core_prov::Tier::Deterministic,
        core_prov::ConfidenceTier::Confirmed,
        vec![],
        "app.ingest",
        root.to_string_lossy().as_bytes(),
    )
    .expect("within ceiling");
    graph
        .put_node(&Node {
            id: format!("repo:{repo}"),
            label: "Repo".into(),
            props: serde_json::json!({
                "root": root.to_string_lossy(),
                "commit": commit,
                "prov": serde_json::to_value(repo_prov).expect("serializes"),
            }),
        })
        .map_err(|e| e.to_string())?;
    for node in &extraction.nodes {
        graph.put_node(node).map_err(|e| e.to_string())?;
    }
    for edge in &extraction.edges {
        graph.put_edge(edge).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Join observed infra to the event layer: insert a `BACKS` edge wherever
/// an enriched `Resource`'s observed identity names a `Channel` that code
/// actually publishes or subscribes (SPEC-00 §4.1, M6). Runs over the
/// whole graph after every load — `put_edge` upserts, so it is idempotent
/// and later repos can back channels from earlier ones.
fn stitch_backings(graph: &mut SqliteGraphStore) -> Result<u64, String> {
    let resources = graph
        .nodes_with_label("Resource")
        .map_err(|e| e.to_string())?;
    let mut inserted = 0;
    for edge in dynamic::backing_candidates(&resources) {
        let channel_exists = graph
            .get_node(&edge.dst)
            .map_err(|e| e.to_string())?
            .is_some();
        if channel_exists {
            graph.put_edge(&edge).map_err(|e| e.to_string())?;
            inserted += 1;
        }
    }
    Ok(inserted)
}

/// Run T0 extraction over a local directory and load the facts into the
/// graph (US-0002 local path; GitHub clone ingest is `add_repo`).
#[tauri::command]
fn ingest_path(path: String, state: State<'_, AppState>) -> Result<IngestSummary, String> {
    let job_id = {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        let job = jobs
            .enqueue(&format!("ingest:{path}"))
            .map_err(|e| e.to_string())?;
        jobs.set_status(job.id, "running")
            .map_err(|e| e.to_string())?;
        job.id
    };
    let fail = |e: String, state: &State<'_, AppState>, job_id: i64| -> String {
        if let Ok(mut jobs) = state.jobs.lock() {
            let _ = jobs.set_status(job_id, "failed");
        }
        e
    };

    // Local unversioned tree: identified by directory basename (two dirs
    // with the same basename still collide — real identity is `add_repo`).
    let root = std::fs::canonicalize(&path).map_err(|e| fail(e.to_string(), &state, job_id))?;
    let repo = format!(
        "local/{}",
        root.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
    );
    let (extraction, layers) = extract_tree_with_summary(
        &root,
        &repo,
        "workdir",
        &[],
        &std::collections::BTreeMap::new(),
        None,
        &[],
    )
    .map_err(|e| fail(e, &state, job_id))?;
    {
        let mut graph = state
            .graph
            .lock()
            .map_err(|e| fail(e.to_string(), &state, job_id))?;
        load_into_graph(&mut graph, &extraction, &repo, &root, "workdir")
            .map_err(|e| fail(e, &state, job_id))?;
        stitch_backings(&mut graph).map_err(|e| fail(e, &state, job_id))?;
    }
    let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    jobs.set_status(job_id, "done").map_err(|e| e.to_string())?;
    Ok(IngestSummary {
        job_id,
        files: layers.files(),
        nodes: extraction.nodes.len() as u64,
        edges: extraction.edges.len() as u64,
        layers,
    })
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
}

/// Clone a GitHub repo (read-only, shallow) and ingest it with its real
/// identity — every fact's evidence carries owner/name@sha (US-0001,
/// AC-0001). Auth per the ADR-0009 ladder; failures carry remediation and
/// leave no partial clone (AC-0003).
#[tauri::command]
fn add_repo(
    url: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<AddRepoSummary, String> {
    let job_id = {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        let job = jobs
            .enqueue(&format!("add-repo:{url}"))
            .map_err(|e| e.to_string())?;
        jobs.set_status(job.id, "running")
            .map_err(|e| e.to_string())?;
        job.id
    };
    let fail = |e: String, state: &State<'_, AppState>, job_id: i64| -> String {
        if let Ok(mut jobs) = state.jobs.lock() {
            let _ = jobs.set_status(job_id, "failed");
        }
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
    let (extraction, layers) = extract_tree_with_summary(
        &cloned.path,
        &cloned.repo,
        &cloned.commit_sha,
        &[],
        &std::collections::BTreeMap::new(),
        None,
        &[],
    )
    .map_err(|e| fail(e, &state, job_id))?;
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
        stitch_backings(&mut graph).map_err(|e| fail(e, &state, job_id))?;
    }
    let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    jobs.set_status(job_id, "done").map_err(|e| e.to_string())?;
    Ok(AddRepoSummary {
        job_id,
        repo: cloned.repo,
        commit_sha: cloned.commit_sha,
        files: layers.files(),
        nodes: extraction.nodes.len() as u64,
        edges: extraction.edges.len() as u64,
        layers,
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
fn add_system(
    path: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<AddSystemSummary, String> {
    let job_id = {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        let job = jobs
            .enqueue(&format!("add-system:{path}"))
            .map_err(|e| e.to_string())?;
        jobs.set_status(job.id, "running")
            .map_err(|e| e.to_string())?;
        job.id
    };
    let fail = |e: String, state: &State<'_, AppState>, job_id: i64| -> String {
        if let Ok(mut jobs) = state.jobs.lock() {
            let _ = jobs.set_status(job_id, "failed");
        }
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
    let (mut files, mut nodes, mut edges) = (0u64, 0u64, 0u64);
    let mut layers = LayerBreakdown::default();
    for entry in &manifest.repos {
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
        // state_json travels with the manifest, so it resolves against the
        // manifest dir — same rule as local repo paths.
        let state_path = entry.state_json.as_ref().map(|p| base.join(p));
        let trace_paths: Vec<std::path::PathBuf> =
            entry.otel_jsonl.iter().map(|p| base.join(p)).collect();
        let (extraction, repo_layers) = extract_tree_with_summary(
            &root,
            &repo,
            &commit,
            &entry.layers,
            &manifest.env,
            state_path.as_deref(),
            &trace_paths,
        )
        .map_err(|e| fail(e, &state, job_id))?;
        files += repo_layers.files();
        nodes += extraction.nodes.len() as u64;
        edges += extraction.edges.len() as u64;
        layers.add(repo_layers);
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
    }
    {
        // After every repo is in: infra from one repo can back channels
        // published by another.
        let mut graph = state
            .graph
            .lock()
            .map_err(|e| fail(e.to_string(), &state, job_id))?;
        stitch_backings(&mut graph).map_err(|e| fail(e, &state, job_id))?;
    }
    let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    jobs.set_status(job_id, "done").map_err(|e| e.to_string())?;
    Ok(AddSystemSummary {
        job_id,
        repos,
        files,
        nodes,
        edges,
        layers,
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
        truncated: window.truncated,
    })
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let graph = SqliteGraphStore::open(data_dir.join("graph.db"))?;
            let state_path = data_dir.join("state.db");
            let jobs = JobStore::open(&state_path)?;
            let decisions = agents::DecisionLog::open(&state_path)?;
            app.manage(AppState {
                graph: Mutex::new(graph),
                jobs: Mutex::new(jobs),
                decisions: Mutex::new(decisions),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ping,
            graph_stats,
            clear_graph,
            enqueue_job,
            list_jobs,
            list_evals,
            record_agent_decision,
            list_agent_decisions,
            reapply_agent_decisions,
            record_assertion_decision,
            list_assertion_decisions,
            ingest_path,
            list_nodes,
            atlas_snapshot,
            read_evidence,
            export_topology,
            export_flows,
            list_flows,
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
            &[],
        )
        .unwrap();
        assert_eq!(summary.ts.files, 1);
        assert_eq!(summary.tf.files, 1);
        assert!(summary.tf.nodes > 0);
        assert_eq!(summary.files(), 2);
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
}
