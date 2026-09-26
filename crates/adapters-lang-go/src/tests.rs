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
    api.GET("/wrapped", middleware.Wrap(create))
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
            ("GET".into(), "/wrapped".into(), "gin".into()),
            ("POST".into(), "/orders".into(), "gin".into()),
        ]
    );
    assert!(edge_pairs(&extraction, "HANDLES").contains(&(
        "ep:local/go-service@GET:/health",
        "sym:local/go-service@main.go#health"
    )));
    let wrapped = extraction
        .edges
        .iter()
        .find(|edge| edge.src == "ep:local/go-service@GET:/wrapped" && edge.label == "HANDLES")
        .expect("unresolved handler stays explicit");
    assert!(wrapped.dst.starts_with("gap:go-handler:"));
    assert_eq!(wrapped.props["prov"]["confidence_tier"], "Gap");
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
fn ignored_go_mod_yields_no_module_path() {
    // AC-0225/T-0225 (#468, ADR-0034): a `go.mod` the root's own `.gitignore`
    // excludes is treated as absent — same as no module declaration at all —
    // rather than read anyway, matching the shared walk's ignore rules for
    // this one direct read outside it.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("pkg/helper")).unwrap();
    std::fs::write(dir.path().join(".gitignore"), "go.mod\n").unwrap();
    std::fs::write(dir.path().join("go.mod"), "module example.com/service\n").unwrap();
    std::fs::write(
        dir.path().join("pkg/helper/helper.go"),
        "package helper\n\nfunc Imported() int { return 1 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("main.go"),
        r#"package main

import "example.com/service/pkg/helper"

func handler() { helper.Imported() }
"#,
    )
    .unwrap();
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    // With no proven module path, the import cannot resolve to an internal
    // package, so the cross-file CALLS edge into helper.Imported never forms.
    assert!(
        !edge_pairs(&extraction, "CALLS")
            .iter()
            .any(|(_, dst)| dst.contains("pkg/helper/helper.go#Imported"))
    );
}

#[test]
fn unignored_go_mod_still_proves_module_internal_imports() {
    // Control for the above: an untouched `.gitignore` that doesn't name
    // `go.mod` leaves module resolution exactly as it was before #468.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("pkg/helper")).unwrap();
    std::fs::write(dir.path().join(".gitignore"), "build/\n").unwrap();
    std::fs::write(dir.path().join("go.mod"), "module example.com/service\n").unwrap();
    std::fs::write(
        dir.path().join("pkg/helper/helper.go"),
        "package helper\n\nfunc Imported() int { return 1 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("main.go"),
        r#"package main

import "example.com/service/pkg/helper"

func handler() { helper.Imported() }
"#,
    )
    .unwrap();
    let extraction = extract_dir(dir.path(), &id()).unwrap();
    assert!(
        edge_pairs(&extraction, "CALLS")
            .iter()
            .any(|(_, dst)| dst.contains("pkg/helper/helper.go#Imported"))
    );
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
    std::fs::write(
        dir.path().join("ignored.go"),
        "//go:build ignore\n\npackage app\nfunc Ignored() {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("platform_windows.go"),
        "package app\nfunc PlatformOnly() {}\n",
    )
    .unwrap();
    let mut cache = IncrementalCache::default();
    let (first, first_stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(first_stats.recomputed_files, 2);
    assert!(!first.nodes.iter().any(|node| node.id.contains("Noise")));
    assert!(!first.nodes.iter().any(|node| node.id.contains("Ignored")));
    assert!(
        !first
            .nodes
            .iter()
            .any(|node| node.id.contains("PlatformOnly"))
    );
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

#[test]
fn gitignored_trees_are_not_collected() {
    // AC-0205 (#248): the shared walk honors the tree's own `.gitignore`.
    let dir = tempfile::tempdir().unwrap();
    for (path, body) in [
        (".gitignore", "gen/\n"),
        ("cmd/main.go", ""),
        ("gen/api.go", ""),
    ] {
        let file = dir.path().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, body).unwrap();
    }
    let mut files = Vec::new();
    collect_go_files(dir.path(), &mut files).unwrap();
    assert_eq!(files, ["cmd/main.go"]);
}

fn boundary_of(extraction: &Extraction, id: &str) -> (String, core_prov::ConfidenceTier) {
    let node = extraction
        .nodes
        .iter()
        .find(|node| node.id == id)
        .unwrap_or_else(|| panic!("missing node {id}"));
    let prov: Provenance = serde_json::from_value(node.props["prov"].clone())
        .unwrap_or_else(|_| panic!("{id} carries no provenance"));
    assert!(!prov.evidence.is_empty(), "{id} cites its import");
    (
        node.props["boundary"].as_str().unwrap().to_string(),
        prov.confidence_tier,
    )
}

#[test]
fn import_targets_carry_provenance_and_only_proven_externals_confirm() {
    // AC-0207 (#237): beneath the repository's own module paths an existing
    // package directory is internal and anything else an explicit Gap;
    // outside them, stdlib and foreign modules are Confirmed external.
    use core_prov::ConfidenceTier::{Confirmed, Gap};
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("go.mod"),
        "module example.com/svc\n\nreplace example.com/shared => ./shared\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("pkg/store")).unwrap();
    std::fs::write(
        dir.path().join("pkg/store/store_linux.go"),
        "package store\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("main.go"),
        "package main\n\nimport (\n\t\"fmt\"\n\t\"github.com/lib/pq\"\n\t\"example.com/svc/pkg/store\"\n\t\"example.com/svc/pkg/missing\"\n\t\"example.com/shared/x\"\n)\n\nfunc main() { fmt.Println(pq.X, store.Y) }\n",
    )
    .unwrap();
    let out = extract_dir(dir.path(), &id()).unwrap();
    let expect = |id: &str| boundary_of(&out, id);
    assert_eq!(expect("mod:fmt"), ("external".into(), Confirmed));
    assert_eq!(
        expect("mod:github.com/lib/pq"),
        ("external".into(), Confirmed)
    );
    // A package directory whose only file is platform-gated still exists.
    assert_eq!(
        expect("mod:example.com/svc/pkg/store"),
        ("internal".into(), Confirmed)
    );
    assert_eq!(
        expect("mod:example.com/svc/pkg/missing"),
        ("unresolved".into(), Gap)
    );
    // A locally replaced module is the repository's own, never external.
    assert_eq!(
        expect("mod:example.com/shared/x"),
        ("unresolved".into(), Gap)
    );
}

#[test]
fn a_local_replace_target_inside_the_repository_is_internal() {
    // AC-0207 (#477): a local `replace` keeps its target directory, resolved
    // against the declaring go.mod, so an existing package beneath it is a
    // Confirmed internal boundary; a missing one, or a target that leaves
    // the repository root (lexically or via a symlink), stays a Gap.
    use core_prov::ConfidenceTier::{Confirmed, Gap};
    let outer = tempfile::tempdir().unwrap();
    let root = outer.path().join("repo");
    let write = |path: &std::path::Path, body: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    };
    write(
        &root.join("svc/go.mod"),
        "module example.com/svc\n\nreplace (\n\texample.com/shared => ../shared\n\texample.com/outside => ../../outside\n\texample.com/linked => ./linked\n)\n",
    );
    write(&root.join("shared/go.mod"), "module example.com/shared\n");
    write(&root.join("shared/pkg/x.go"), "package pkg\n");
    // Real packages outside the repository root: never probed.
    write(&outer.path().join("outside/pkg/x.go"), "package pkg\n");
    write(&outer.path().join("linked/pkg/x.go"), "package pkg\n");
    #[cfg(unix)]
    std::os::unix::fs::symlink(outer.path().join("linked"), root.join("svc/linked")).unwrap();
    write(
        &root.join("svc/main.go"),
        "package main\n\nimport (\n\t\"example.com/shared/pkg\"\n\t\"example.com/shared/missing\"\n\t\"example.com/outside/pkg\"\n\t\"example.com/linked/pkg\"\n)\n",
    );
    let out = extract_dir(&root, &id()).unwrap();
    let expect = |id: &str| boundary_of(&out, id);
    assert_eq!(
        expect("mod:example.com/shared/pkg"),
        ("internal".into(), Confirmed)
    );
    assert_eq!(
        expect("mod:example.com/shared/missing"),
        ("unresolved".into(), Gap)
    );
    assert_eq!(
        expect("mod:example.com/outside/pkg"),
        ("unresolved".into(), Gap)
    );
    assert_eq!(
        expect("mod:example.com/linked/pkg"),
        ("unresolved".into(), Gap)
    );
}

#[test]
fn a_root_replace_beside_the_target_go_mod_is_internal() {
    // AC-0207 (#477): the issue's shape — a root `replace … => ./shared`
    // listed before `shared/go.mod` — resolves to the package directory.
    use core_prov::ConfidenceTier::{Confirmed, Gap};
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("go.mod"),
        "module example.com/svc\n\nreplace example.com/shared => ./shared\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("shared/pkg")).unwrap();
    std::fs::write(
        dir.path().join("shared/go.mod"),
        "module example.com/shared\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("shared/pkg/x.go"), "package pkg\n").unwrap();
    std::fs::write(
        dir.path().join("main.go"),
        "package main\n\nimport (\n\t\"example.com/shared/pkg\"\n\t\"example.com/shared/missing\"\n)\n",
    )
    .unwrap();
    let out = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(
        boundary_of(&out, "mod:example.com/shared/pkg"),
        ("internal".into(), Confirmed)
    );
    assert_eq!(
        boundary_of(&out, "mod:example.com/shared/missing"),
        ("unresolved".into(), Gap)
    );
}

#[cfg(unix)]
#[test]
fn a_package_directory_symlinked_out_of_the_repository_is_a_gap() {
    // AC-0207 (#477 review): the complete package directory — and the Go
    // file proving it — must resolve inside the repository root, beneath a
    // `replace` target and beneath a discovered `go.mod` alike.
    use core_prov::ConfidenceTier::{Confirmed, Gap};
    use std::os::unix::fs::symlink;
    let outer = tempfile::tempdir().unwrap();
    let root = outer.path().join("repo");
    let write = |path: &std::path::Path, body: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    };
    write(
        &root.join("go.mod"),
        "module example.com/svc\n\nreplace example.com/shared => ./shared\n",
    );
    write(&root.join("shared/go.mod"), "module example.com/shared\n");
    write(&root.join("shared/ok/x.go"), "package ok\n");
    write(&outer.path().join("elsewhere/x.go"), "package elsewhere\n");
    symlink(outer.path().join("elsewhere"), root.join("shared/pkg")).unwrap();
    symlink(outer.path().join("elsewhere"), root.join("local")).unwrap();
    std::fs::create_dir_all(root.join("filelink")).unwrap();
    symlink(
        outer.path().join("elsewhere/x.go"),
        root.join("filelink/x.go"),
    )
    .unwrap();
    write(
        &root.join("main.go"),
        "package main\n\nimport (\n\t\"example.com/shared/ok\"\n\t\"example.com/shared/pkg\"\n\t\"example.com/svc/local\"\n\t\"example.com/svc/filelink\"\n)\n",
    );
    let out = extract_dir(&root, &id()).unwrap();
    let expect = |id: &str| boundary_of(&out, id);
    assert_eq!(
        expect("mod:example.com/shared/ok"),
        ("internal".into(), Confirmed)
    );
    for escaped in [
        "mod:example.com/shared/pkg",
        "mod:example.com/svc/local",
        "mod:example.com/svc/filelink",
    ] {
        assert_eq!(expect(escaped), ("unresolved".into(), Gap), "{escaped}");
    }
}

#[test]
fn a_version_qualified_replace_never_confirms_internal() {
    // AC-0207 (#477 review): `replace m v1.0.0 => …` applies only to that
    // version and no selected version is known, so a module with a
    // version-qualified replace is never proven internal — not through the
    // replacement target, nor through an in-repository go.mod declaring it.
    // Foreign modules pinned the same way stay Confirmed external.
    use core_prov::ConfidenceTier::{Confirmed, Gap};
    let dir = tempfile::tempdir().unwrap();
    let write = |rel: &str, body: &str| {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    };
    write(
        "go.mod",
        "module example.com/svc\n\nreplace (\n\texample.com/shared v1.0.0 => ./shared-v1\n\texample.com/other v1.0.0 => example.com/fork v1.0.0\n\tgolang.org/x/net v0.1.0 => golang.org/x/net v0.2.0\n)\n",
    );
    write("shared-v1/go.mod", "module example.com/shared\n");
    write("shared-v1/pkg/x.go", "package pkg\n");
    write("other/go.mod", "module example.com/other\n");
    write("other/pkg/x.go", "package pkg\n");
    // A nested module beneath a pinned path is no proof either.
    write("shared-v1/sub/go.mod", "module example.com/shared/sub\n");
    write("shared-v1/sub/x.go", "package sub\n");
    write(
        "main.go",
        "package main\n\nimport (\n\t\"example.com/shared/pkg\"\n\t\"example.com/other/pkg\"\n\t\"example.com/shared/sub\"\n\t\"golang.org/x/net/http2\"\n)\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let expect = |id: &str| boundary_of(&out, id);
    assert_eq!(
        expect("mod:example.com/shared/pkg"),
        ("unresolved".into(), Gap)
    );
    assert_eq!(
        expect("mod:example.com/other/pkg"),
        ("unresolved".into(), Gap)
    );
    assert_eq!(
        expect("mod:example.com/shared/sub"),
        ("unresolved".into(), Gap)
    );
    assert_eq!(
        expect("mod:golang.org/x/net/http2"),
        ("external".into(), Confirmed)
    );
}

#[test]
fn without_go_mod_no_import_is_proven_external() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("main.go"),
        "package main\n\nimport \"fmt\"\n\nfunc main() { fmt.Println() }\n",
    )
    .unwrap();
    let out = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(
        boundary_of(&out, "mod:fmt"),
        ("unresolved".into(), core_prov::ConfidenceTier::Gap)
    );
}

#[test]
fn go_mod_comments_and_dot_segments_never_confirm_a_boundary() {
    // AC-0207 (#237 review): a trailing comment on the `module` directive is
    // not part of the path, so in-module imports never read as external; and
    // an import path spelling `..` probes nothing outside the repository.
    use core_prov::ConfidenceTier::{Confirmed, Gap};
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("go.mod"),
        "module example.com/svc // service root\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("pkg/store")).unwrap();
    std::fs::write(dir.path().join("pkg/store/store.go"), "package store\n").unwrap();
    std::fs::write(
        dir.path().join("main.go"),
        "package main\n\nimport (\n\t\"example.com/svc/pkg/store\"\n\t\"example.com/svc/pkg/gone\"\n\t\"example.com/svc/../svc/pkg/store\"\n)\n",
    )
    .unwrap();
    let out = extract_dir(dir.path(), &id()).unwrap();
    assert!(
        out.nodes
            .iter()
            .all(|node| node.id != "mod:example.com/svc/pkg/store"
                || boundary_of(&out, &node.id) == ("internal".into(), Confirmed))
    );
    assert_eq!(
        boundary_of(&out, "mod:example.com/svc/pkg/gone"),
        ("unresolved".into(), Gap)
    );
    assert_eq!(
        boundary_of(&out, "mod:example.com/svc/../svc/pkg/store"),
        ("unresolved".into(), Gap)
    );
}
