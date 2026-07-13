use super::*;

fn id() -> SourceId<'static> {
    SourceId {
        repo: "local/go-service",
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
fn http_chi_and_gin_routes_are_import_proven() {
    // AC-0054/T-0054: registration packages and router factories must be
    // import-proven; matching methods on arbitrary receivers are ignored.
    let source = br#"
package main

import (
    nethttp "net/http"
    "github.com/go-chi/chi/v5"
    web "github.com/gin-gonic/gin"
    fake "example.com/fake"
)

func health(w nethttp.ResponseWriter, r *nethttp.Request) {}
func orders(w nethttp.ResponseWriter, r *nethttp.Request) {}
func create(c *web.Context) {}
func ignored() {}

func routes() {
    nethttp.HandleFunc("GET /health", health)
    router := chi.NewRouter()
    router.Get("/orders", orders)
    api := web.Default()
    api.POST("/orders", create)
    lookalike := fake.NewRouter()
    lookalike.Get("/ignored", ignored)
    dynamic := "/dynamic"
    router.Get(dynamic, ignored)
}
"#;
    let extraction = extract_source(source, "main.go", &id()).unwrap();
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
            ("GET".into(), "/health".into(), "net/http".into()),
            ("GET".into(), "/orders".into(), "chi".into()),
            ("POST".into(), "/orders".into(), "gin".into()),
        ]
    );
    assert!(edge_pairs(&extraction, "HANDLES").contains(&(
        "ep:local/go-service@GET:/health",
        "sym:local/go-service@main.go#health"
    )));
}

#[test]
fn calls_resolve_locally_and_across_packages() {
    // AC-0054/T-0054: local calls resolve in the file pass and imported
    // module packages resolve only when their target declaration is present.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("cmd/api")).unwrap();
    std::fs::create_dir_all(dir.path().join("pkg/helper")).unwrap();
    std::fs::write(dir.path().join("go.mod"), "module example.com/service\n").unwrap();
    std::fs::write(
        dir.path().join("pkg/helper/helper.go"),
        "package helper\n\nfunc Imported() int { return 1 }\nfunc Handler() {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("cmd/api/main.go"),
        r#"package main

import (
    "net/http"
    "example.com/service/pkg/helper"
)

func local() int { return 2 }
func handler() {
    local()
    helper.Imported()
    http.HandleFunc("POST /imported", helper.Handler)
}
"#,
    )
    .unwrap();
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    let handler = "sym:local/go-service@cmd/api/main.go#handler";
    assert!(
        edge_pairs(&extraction, "CALLS")
            .contains(&(handler, "sym:local/go-service@cmd/api/main.go#local"))
    );
    assert!(edge_pairs(&extraction, "CALLS").contains(&(
        handler,
        "sym:local/go-service@pkg/helper/helper.go#Imported"
    )));
    assert!(edge_pairs(&extraction, "HANDLES").contains(&(
        "ep:local/go-service@POST:/imported",
        "sym:local/go-service@pkg/helper/helper.go#Handler"
    )));
}

#[test]
fn every_fact_has_confirmed_provenance_and_spans() {
    // AC-0054/T-0054: exact Go source spans are first-class T0 evidence.
    let source = b"package main\n\nimport \"net/http\"\n\nfunc h(w http.ResponseWriter, r *http.Request) {}\nfunc routes() { http.HandleFunc(\"/x\", h) }\n";
    let extraction = extract_source(source, "main.go", &id()).unwrap();
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
        assert_eq!(props["prov"]["evidence"][0]["path"], "main.go");
        let start = props["prov"]["evidence"][0]["byte_start"].as_u64().unwrap();
        let end = props["prov"]["evidence"][0]["byte_end"].as_u64().unwrap();
        assert!(end > start);
        assert!(end <= source.len() as u64);
    }
}

#[test]
fn incremental_reingest_is_deterministic_and_skips_noise() {
    // AC-0054/T-0054: unchanged Go contexts are reused, byte changes are
    // isolated, and vendored/test/build sources never enter application facts.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.go"),
        "package app\nfunc A() int { return 1 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("b.go"),
        "package app\nfunc B() int { return 2 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("a_test.go"),
        "package app\nfunc TestNoise() {}\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("vendor/example.com/noise")).unwrap();
    std::fs::write(
        dir.path().join("vendor/example.com/noise/noise.go"),
        "package noise\nfunc Noise() {}\n",
    )
    .unwrap();
    let mut cache = IncrementalCache::default();
    let (first, first_stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(first_stats.recomputed_files, 2);
    assert!(!first.nodes.iter().any(|node| node.id.contains("Noise")));
    let (same, same_stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(same_stats.recomputed_files, 0);
    assert_eq!(same_stats.reused_files, 2);
    assert_eq!(first.nodes, same.nodes);
    assert_eq!(first.edges, same.edges);

    std::fs::write(dir.path().join("go.mod"), "module example.com/changed\n").unwrap();
    let (_, module_stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(module_stats.recomputed_files, 2);
    assert_eq!(module_stats.reused_files, 0);

    std::fs::write(
        dir.path().join("b.go"),
        "package app\nfunc Changed() int { return 3 }\n",
    )
    .unwrap();
    let (changed, changed_stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(changed_stats.recomputed_files, 1);
    assert_eq!(changed_stats.reused_files, 1);
    assert!(
        changed
            .nodes
            .iter()
            .any(|node| node.id.ends_with("#Changed"))
    );
    assert!(!changed.nodes.iter().any(|node| node.id.ends_with("#B")));

    std::fs::remove_file(dir.path().join("b.go")).unwrap();
    let (_, deleted_stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(deleted_stats.deleted_files, 1);
}
