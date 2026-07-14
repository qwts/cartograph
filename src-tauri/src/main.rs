//! Cartograph desktop shell (M0): boots the webview, owns the graph store and
//! the durable job spine, and exposes the first Tauri commands.

// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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
    metrics: Mutex<metrics::MetricsStore>,
}

#[derive(Default)]
struct RepoExtractionCache {
    ts: adapters_lang_ts::IncrementalCache,
    python: adapters_lang_python::IncrementalCache,
    go: adapters_lang_go::IncrementalCache,
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
    tf: LayerSummary,
}

impl LayerBreakdown {
    fn add(&mut self, other: Self) {
        self.ts.add(other.ts);
        self.python.add(other.python);
        self.go.add(other.go);
        self.tf.add(other.tf);
    }

    fn files(self) -> u64 {
        self.ts.files + self.python.files + self.go.files + self.tf.files
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
        let (mut extraction, stats) =
            adapters_lang_ts::extract_dir_incremental(root, &ts_id, &mut cache.ts)
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
    if wants_server {
        let python_id = adapters_lang_python::SourceId { repo, commit };
        let (python, stats) =
            adapters_lang_python::extract_dir_incremental(root, &python_id, &mut cache.python)
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
        let (go, stats) = adapters_lang_go::extract_dir_incremental(root, &go_id, &mut cache.go)
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
    }
    if wants_infra {
        let tf_id = iac::SourceId { repo, commit };
        let (tf, stats) =
            iac::extract_dir_incremental(root, &tf_id, &mut cache.tf).map_err(|e| e.to_string())?;
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
    state: State<'_, AppState>,
) -> Result<ingest::preflight::PreflightReport, String> {
    let root = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
    let repo = format!(
        "local/{}",
        root.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
    );
    let report = ingest::preflight::preflight(&root).map_err(|e| e.to_string())?;
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
        ("t0.iac-terraform", layers.tf.files),
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

/// Notify the shell of a job transition (`job://changed`); the Jobs surface
/// and the global progress bar stay live without polling (#117).
fn emit_job(app: &tauri::AppHandle, job: &Job) {
    let _ = app.emit("job://changed", job);
}

/// Record stage + percent for a running job and notify the shell.
fn report_progress(
    app: &tauri::AppHandle,
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
fn report_failure(app: &tauri::AppHandle, state: &AppState, job_id: i64, error: &str) {
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
fn job_cancelled(state: &AppState, job_id: i64) -> bool {
    state
        .jobs
        .lock()
        .ok()
        .and_then(|jobs| jobs.is_cancelled(job_id).ok())
        .unwrap_or(false)
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
    let repo = format!(
        "local/{}",
        root.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
    );

    cancelled()?;
    report_progress(app, state, job_id, "extract", 15.0)?;
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
#[tauri::command]
fn ingest_path(
    path: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<IngestSummary, String> {
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
fn retry_job(id: i64, app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Job, String> {
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
        )
        .map_err(|e| fail(e, &state, job_id))?
    };
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
    let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    jobs.set_status(job_id, "done").map_err(|e| e.to_string())?;
    let done = jobs.get(job_id).map_err(|e| e.to_string())?;
    emit_job(&app, &done);
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
        let pulumi_path = entry.pulumi_json.as_ref().map(|p| base.join(p));
        let trace_paths: Vec<std::path::PathBuf> =
            entry.otel_jsonl.iter().map(|p| base.join(p)).collect();
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
    let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    jobs.set_status(job_id, "done").map_err(|e| e.to_string())?;
    let done = jobs.get(job_id).map_err(|e| e.to_string())?;
    emit_job(&app, &done);
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

fn main() {
    tauri::Builder::default()
        .setup(|app| {
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
                metrics: Mutex::new(recovery_metrics),
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
}
