//! Cartograph desktop shell (M0): boots the webview, owns the graph store and
//! the durable job spine, and exposes the first Tauri commands.

// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod evidence;
mod jobs;

use core_graph::{GraphStore, Node, SqliteGraphStore};
use jobs::{Job, JobStore};
use serde::Serialize;
use std::sync::Mutex;
use tauri::{Manager, State};

/// Stores managed by the Tauri runtime. Graph and state spine are separate
/// databases (ADR-0008): the graph is a disposable ingest artifact, the spine
/// holds durable state.
struct AppState {
    graph: Mutex<SqliteGraphStore>,
    jobs: Mutex<JobStore>,
}

#[derive(Serialize)]
struct GraphStats {
    nodes: u64,
    edges: u64,
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

#[derive(Serialize)]
struct IngestSummary {
    job_id: i64,
    files: u64,
    nodes: u64,
    edges: u64,
}

/// Run T0 TypeScript extraction over a local directory and load the facts
/// into the graph (M1 slice of US-0002; GitHub clone ingest is US-0001).
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

    // Local unversioned tree: repo/commit identify the workdir until the
    // GitHub ingest (US-0001) supplies real clone coordinates.
    let id = adapters_lang_ts::SourceId {
        repo: "local",
        commit: "workdir",
    };
    let root = std::fs::canonicalize(&path).map_err(|e| fail(e.to_string(), &state, job_id))?;
    let extraction = adapters_lang_ts::extract_dir(&root, &id)
        .map_err(|e| fail(e.to_string(), &state, job_id))?;

    let files = extraction
        .nodes
        .iter()
        .filter(|n| n.label == "File" && n.props.get("placeholder").is_none())
        .count() as u64;
    {
        let mut graph = state
            .graph
            .lock()
            .map_err(|e| fail(e.to_string(), &state, job_id))?;
        // Repo node records where the tree lives so evidence views can read
        // source later (survives restarts with the graph).
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
                id: format!("repo:{}", id.repo),
                label: "Repo".into(),
                props: serde_json::json!({
                    "root": root.to_string_lossy(),
                    "prov": serde_json::to_value(repo_prov).expect("serializes"),
                }),
            })
            .map_err(|e| fail(e.to_string(), &state, job_id))?;
        for node in &extraction.nodes {
            graph
                .put_node(node)
                .map_err(|e| fail(e.to_string(), &state, job_id))?;
        }
        for edge in &extraction.edges {
            graph
                .put_edge(edge)
                .map_err(|e| fail(e.to_string(), &state, job_id))?;
        }
    }
    let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    jobs.set_status(job_id, "done").map_err(|e| e.to_string())?;
    Ok(IngestSummary {
        job_id,
        files,
        nodes: extraction.nodes.len() as u64,
        edges: extraction.edges.len() as u64,
    })
}

/// Nodes carrying `label` (e.g. `Endpoint`, `Repo`), ordered by id.
#[tauri::command]
fn list_nodes(label: String, state: State<'_, AppState>) -> Result<Vec<Node>, String> {
    let graph = state.graph.lock().map_err(|e| e.to_string())?;
    graph.nodes_with_label(&label).map_err(|e| e.to_string())
}

#[derive(Serialize)]
struct EvidenceSource {
    text: String,
    truncated: bool,
}

/// Read-only source for an evidence span's file, confined to the ingest root
/// recorded on the `Repo` node (NG1: navigation, never edit).
#[tauri::command]
fn read_evidence(root: String, path: String) -> Result<EvidenceSource, String> {
    let (text, truncated) =
        evidence::read_source(std::path::Path::new(&root), &path).map_err(|e| e.to_string())?;
    Ok(EvidenceSource { text, truncated })
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let graph = SqliteGraphStore::open(data_dir.join("graph.db"))?;
            let jobs = JobStore::open(data_dir.join("state.db"))?;
            app.manage(AppState {
                graph: Mutex::new(graph),
                jobs: Mutex::new(jobs),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ping,
            graph_stats,
            enqueue_job,
            list_jobs,
            ingest_path,
            list_nodes,
            read_evidence
        ])
        .run(tauri::generate_context!())
        .expect("error while running Cartograph");
}
