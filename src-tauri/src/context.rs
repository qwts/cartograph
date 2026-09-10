//! Shared recovered-context reads for the desktop transport (SPEC-01, H1).

use context_hub::{ContextSnapshot, QueryRequest, QueryResponse};
use core_graph::SqliteGraphStore;
use std::sync::Mutex;
use tauri::Manager;

/// Copy one coherent graph revision, then release storage before preparing the
/// immutable query snapshot. This adapter never projects accepted proposals.
fn query_graph(
    graph: &Mutex<SqliteGraphStore>,
    request: QueryRequest,
) -> Result<QueryResponse, String> {
    request
        .validate_limits()
        .map_err(|error| error.to_string())?;
    let (nodes, edges) = {
        let graph = graph.lock().map_err(|error| error.to_string())?;
        graph.read_snapshot().map_err(|error| error.to_string())?
    };
    ContextSnapshot::new(nodes, edges)
        .and_then(|snapshot| snapshot.query(request))
        .map_err(|error| error.to_string())
}

/// Bounded, read-only context endpoint. The graph copy, canonicalization and
/// selection all run on the worker; no database work occurs on the caller.
#[tauri::command]
pub(crate) async fn query_context(
    request: QueryRequest,
    app: tauri::AppHandle,
) -> Result<QueryResponse, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<crate::AppState>();
        query_graph(&state.graph, request)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_graph::{Edge, GraphStore, Node};
    use std::sync::Arc;

    fn request() -> QueryRequest {
        serde_json::from_value(serde_json::json!({
            "scope": {"type": "all"},
            "kind": null,
            "labels": [],
            "max_facts": 1,
            "max_bytes": 16384,
            "cursor": null
        }))
        .unwrap()
    }

    #[test]
    fn context_query_reads_on_worker_without_mutating_graph() {
        // AC-0105: exercise the storage adapter on the same worker boundary as
        // the command; paging and sanitization must leave source facts intact.
        let mut graph = SqliteGraphStore::open_in_memory().unwrap();
        graph
            .put_node(&Node {
                id: "order".into(),
                label: "Symbol".into(),
                props: serde_json::json!({"name": "order", "prov": {"invalid": true}}),
            })
            .unwrap();
        graph
            .put_edge(&Edge {
                src: "order".into(),
                dst: "order".into(),
                label: "CALLS".into(),
                props: serde_json::json!({}),
            })
            .unwrap();
        let before = (graph.all_nodes().unwrap(), graph.all_edges().unwrap());
        let graph = Arc::new(Mutex::new(graph));
        let worker_graph = Arc::clone(&graph);
        let caller = std::thread::current().id();
        let (worker, page) = tauri::async_runtime::block_on(crate::off_ui_thread(move || {
            Ok((
                std::thread::current().id(),
                query_graph(&worker_graph, request())?,
            ))
        }))
        .unwrap();
        assert_ne!(caller, worker);
        assert_eq!(
            serde_json::to_value(&page).unwrap()["view"],
            "recovered_graph"
        );
        assert_eq!(page.total_selected, 2);
        assert_eq!(page.facts.len(), 1);
        let mut next = request();
        next.cursor = page.next_cursor;
        let second = query_graph(&graph, next).unwrap();
        assert_eq!(second.snapshot_id, page.snapshot_id);
        assert_eq!(second.facts.len(), 1);
        assert!(second.next_cursor.is_none());
        let graph = graph.lock().unwrap();
        assert_eq!(
            before,
            (graph.all_nodes().unwrap(), graph.all_edges().unwrap())
        );
    }

    #[test]
    fn context_query_propagates_errors_without_mutating_graph() {
        // AC-0105: invalid requests fail through the worker/transport boundary.
        let graph = Mutex::new(SqliteGraphStore::open_in_memory().unwrap());
        let mut invalid = request();
        invalid.max_facts = 0;
        let error = tauri::async_runtime::block_on(crate::off_ui_thread(move || {
            let response = query_graph(&graph, invalid);
            assert_eq!(graph.lock().unwrap().node_count().unwrap(), 0);
            response
        }));
        assert!(error.is_err());
    }

    #[test]
    fn context_query_rejects_invalid_request_before_graph_access() {
        // AC-0105: even unavailable storage must not be touched to reject an
        // invalid budget. Otherwise malformed requests can rebuild a huge graph.
        let graph = Arc::new(Mutex::new(SqliteGraphStore::open_in_memory().unwrap()));
        let worker_graph = Arc::clone(&graph);
        assert!(
            std::thread::spawn(move || {
                let _guard = worker_graph.lock().unwrap();
                panic!("poison fixture storage");
            })
            .join()
            .is_err()
        );
        let mut invalid = request();
        invalid.max_facts = 0;
        let error = query_graph(&graph, invalid).unwrap_err();
        assert!(error.contains("max_facts"), "{error}");
    }
}
