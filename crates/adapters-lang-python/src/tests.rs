use super::*;

fn id() -> SourceId<'static> {
    SourceId {
        repo: "local/python-service",
        commit: "abc123",
    }
}

fn edge_pairs<'a>(extraction: &'a Extraction, label: &str) -> Vec<(&'a str, &'a str)> {
    extraction
        .edges
        .iter()
        .filter(|edge| edge.label == label)
        .map(|edge| (edge.src.as_str(), edge.dst.as_str()))
        .collect()
}

#[test]
fn fastapi_and_flask_routes_are_import_proven() {
    // AC-0053/T-0053: the receiver must be created by an import-proven
    // framework factory; matching decorator spellings on arbitrary objects
    // are not endpoints.
    let source = br#"
from fastapi import FastAPI
from flask import Flask

api = FastAPI()
web = Flask(__name__)
fake = object()

@api.get("/orders")
def list_orders():
    return []

@web.route("/health", methods=["GET", "POST"])
def health():
    return "ok"

@fake.get("/not-an-endpoint")
def lookalike():
    return None

prefix = "/v1"
@api.get(f"{prefix}/computed")
def computed_route():
    return None
"#;
    let extraction = extract_source(source, "app.py", &id()).unwrap();
    let mut endpoints = extraction
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| {
            (
                node.props["method"].as_str().unwrap().to_string(),
                node.props["path"].as_str().unwrap().to_string(),
                node.props["framework"].as_str().unwrap().to_string(),
            )
        })
        .collect::<Vec<_>>();
    endpoints.sort();
    assert_eq!(
        endpoints,
        vec![
            ("GET".into(), "/health".into(), "flask".into()),
            ("GET".into(), "/orders".into(), "fastapi".into()),
            ("POST".into(), "/health".into(), "flask".into()),
        ]
    );
    assert!(
        !endpoints
            .iter()
            .any(|(_, path, _)| path == "/not-an-endpoint")
    );
    assert!(
        !endpoints
            .iter()
            .any(|(_, path, _)| path.ends_with("/computed"))
    );
    assert!(edge_pairs(&extraction, "HANDLES").contains(&(
        "ep:local/python-service@GET:/orders",
        "sym:local/python-service@app.py#list_orders"
    )));
}

#[test]
fn calls_resolve_locally_and_across_files() {
    // AC-0053/T-0053: local calls resolve in the file pass; explicitly local
    // imports resolve only after the directory pass proves the target symbol.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("pkg")).unwrap();
    std::fs::write(dir.path().join("pkg/__init__.py"), "").unwrap();
    std::fs::write(
        dir.path().join("pkg/helper.py"),
        "def imported_helper():\n    return 1\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("pkg/app.py"),
        r#"
from .helper import imported_helper

def local_helper():
    return 2

def handler():
    local_helper()
    imported_helper()
"#,
    )
    .unwrap();

    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let handler = "sym:local/python-service@pkg/app.py#handler";
    assert!(
        edge_pairs(&extraction, "CALLS")
            .contains(&(handler, "sym:local/python-service@pkg/app.py#local_helper"))
    );
    assert!(edge_pairs(&extraction, "CALLS").contains(&(
        handler,
        "sym:local/python-service@pkg/helper.py#imported_helper"
    )));
    assert!(extraction.nodes.iter().all(|node| node.label != "Gap"));
}

#[test]
fn every_fact_has_confirmed_provenance_and_spans() {
    // AC-0053/T-0053: exact Python source spans are first-class T0 evidence.
    let source =
        b"from fastapi import FastAPI\napp = FastAPI()\n\n@app.get('/x')\ndef x():\n    return x\n";
    let extraction = extract_source(source, "api.py", &id()).unwrap();
    for props in extraction
        .nodes
        .iter()
        .filter(|node| node.props.get("placeholder").is_none())
        .map(|node| &node.props)
        .chain(extraction.edges.iter().map(|edge| &edge.props))
    {
        assert_eq!(props["prov"]["tier"], "Deterministic");
        assert_eq!(props["prov"]["confidence_tier"], "Confirmed");
        assert_eq!(props["prov"]["extractor_id"], EXTRACTOR_ID);
        assert_eq!(props["prov"]["evidence"][0]["path"], "api.py");
        let start = props["prov"]["evidence"][0]["byte_start"].as_u64().unwrap();
        let end = props["prov"]["evidence"][0]["byte_end"].as_u64().unwrap();
        assert!(end > start);
        assert!(end <= source.len() as u64);
    }
}

#[test]
fn incremental_reingest_is_deterministic() {
    // AC-0053/T-0053: unchanged Python contexts are reused and a byte change
    // recomputes only that file while directory joins rerun over the full set.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.py"), "def a():\n    return 1\n").unwrap();
    std::fs::write(dir.path().join("b.py"), "def b():\n    return 2\n").unwrap();
    std::fs::create_dir_all(dir.path().join("node_modules/vendor")).unwrap();
    std::fs::write(
        dir.path().join("node_modules/vendor/third_party.py"),
        "def vendored():\n    return 0\n",
    )
    .unwrap();
    let mut cache = IncrementalCache::default();
    let (first, first_stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(first_stats.recomputed_files, 2);
    let (same, same_stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(same_stats.recomputed_files, 0);
    assert_eq!(same_stats.reused_files, 2);
    assert_eq!(first.nodes, same.nodes);
    assert_eq!(first.edges, same.edges);

    std::fs::write(dir.path().join("b.py"), "def changed():\n    return 3\n").unwrap();
    let (changed, changed_stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(changed_stats.recomputed_files, 1);
    assert_eq!(changed_stats.reused_files, 1);
    assert!(
        changed
            .nodes
            .iter()
            .any(|node| node.id.ends_with("#changed"))
    );
    assert!(!changed.nodes.iter().any(|node| node.id.ends_with("#b")));

    std::fs::remove_file(dir.path().join("b.py")).unwrap();
    let (_, deleted_stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(deleted_stats.deleted_files, 1);
}
