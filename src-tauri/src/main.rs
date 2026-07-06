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
    let root = std::fs::canonicalize(&path).map_err(|e| fail(e.to_string(), &state, job_id))?;
    let ts_id = adapters_lang_ts::SourceId {
        repo: "local",
        commit: "workdir",
    };
    let mut extraction = adapters_lang_ts::extract_dir(&root, &ts_id)
        .map_err(|e| fail(e.to_string(), &state, job_id))?;
    // Same tree, second layer: Terraform (US-0003). Both extractors are T0;
    // their node id schemes are disjoint (file:/sym:/ep: vs res:).
    let tf_id = iac::SourceId {
        repo: "local",
        commit: "workdir",
    };
    let tf = iac::extract_dir(&root, &tf_id).map_err(|e| fail(e.to_string(), &state, job_id))?;
    extraction.nodes.extend(tf.nodes);
    extraction.edges.extend(tf.edges);
    // Third layer over the same tree: channel stitching (US-0004). Event
    // sites from the TS pass resolve against env files present in the repo.
    let cfg =
        events::ConfigIndex::from_dir(&root).map_err(|e| fail(e.to_string(), &state, job_id))?;
    let ev_id = events::SourceId {
        repo: "local",
        commit: "workdir",
    };
    let stitched = events::stitch(&extraction.event_sites, &cfg, &ev_id);
    extraction.nodes.extend(stitched.nodes);
    extraction.edges.extend(stitched.edges);
    // Stitched edge sources can name symbols the TS pass did not node-ify
    // (class methods, non-handler arrows). Close over the combined facts so
    // every endpoint exists before the FK-enforcing store sees the edges.
    extraction.close_over_endpoints();

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
                id: format!("repo:{}", ts_id.repo),
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

/// The resource/topology map artifact as Mermaid text (SPEC-00 §7, M2 exit
/// gate). Deterministic for a given graph.
#[tauri::command]
fn export_topology(state: State<'_, AppState>) -> Result<String, String> {
    let graph = state.graph.lock().map_err(|e| e.to_string())?;
    let nodes = graph
        .nodes_with_label("Resource")
        .map_err(|e| e.to_string())?;
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

/// Nodes carrying `label` (e.g. `Endpoint`, `Repo`), ordered by id.
#[tauri::command]
fn list_nodes(label: String, state: State<'_, AppState>) -> Result<Vec<Node>, String> {
    let graph = state.graph.lock().map_err(|e| e.to_string())?;
    graph.nodes_with_label(&label).map_err(|e| e.to_string())
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
            read_evidence,
            export_topology,
            export_flows,
            list_flows
        ])
        .run(tauri::generate_context!())
        .expect("error while running Cartograph");
}

#[cfg(test)]
mod tests {
    use core_graph::{GraphStore, SqliteGraphStore};

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
// A class-method producer: the TS pass emits no Symbol node for methods,
// so the stitched edge source only exists via the post-stitch close-over.
export class Shipper {
  ship() { bus.emit('order.shipped'); }
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
        assert!(
            mmd.contains("res_aws_sqs_queue_orders -->|TRIGGERS| res_aws_lambda_function_fulfill")
        );
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
        // The class-method producer landed via a placeholder Symbol — the
        // edge inserted without violating the store's foreign keys.
        let symbols = store.nodes_with_label("Symbol").unwrap();
        assert!(
            symbols
                .iter()
                .any(|s| s.id == "sym:app.ts#ship" && s.props["placeholder"] == true)
        );

        // M3 exit gate: an end-to-end T0 flow from the endpoint trigger
        // through the channel to the consumer, exported as a dossier.
        let mut flow_nodes = Vec::new();
        for label in flowtracer::FLOW_NODE_LABELS {
            flow_nodes.extend(store.nodes_with_label(label).unwrap());
        }
        let flow_edges = store
            .edges_with_labels(flowtracer::FLOW_EDGE_LABELS)
            .unwrap();
        let flows = flowtracer::trace(&flow_nodes, &flow_edges);
        let dossier = spec::flow_dossier(&flows);
        assert!(dossier.contains("## POST /orders — Verified (score 1.00)"));
        assert!(dossier.contains("SUBSCRIBES [Confirmed]"));
        assert!(dossier.contains("chan:inproc-event:order.placed"));
    }
}
