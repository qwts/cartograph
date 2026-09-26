use super::*;
use core_graph::{Edge, Node};

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn id<'a>() -> SourceId<'a> {
    SourceId {
        repo: "local/demo",
        commit: "workdir",
    }
}

fn node<'a>(nodes: &'a [Node], id: &str) -> &'a Node {
    nodes
        .iter()
        .find(|node| node.id == id)
        .unwrap_or_else(|| panic!("missing node {id}"))
}

fn edge<'a>(edges: &'a [Edge], src: &str, dst: &str, label: &str) -> &'a Edge {
    edges
        .iter()
        .find(|edge| edge.src == src && edge.dst == dst && edge.label == label)
        .unwrap_or_else(|| panic!("missing edge {src} -{label}-> {dst}"))
}

#[test]
fn classes_methods_and_same_class_calls_carry_confirmed_provenance() {
    let source = br#"package com.demo;

public class Greeter {
    public String hello() {
        return greet();
    }

    private String greet() {
        return this.suffix();
    }

    private String suffix() {
        return "!";
    }
}
"#;
    let path = "src/main/java/com/demo/Greeter.java";
    let out = extract_source(source, path, &id()).unwrap();

    let class_sym = format!("sym:local/demo@{path}#Greeter");
    let hello = format!("sym:local/demo@{path}#Greeter.hello");
    let greet = format!("sym:local/demo@{path}#Greeter.greet");
    let suffix = format!("sym:local/demo@{path}#Greeter.suffix");
    assert_eq!(node(&out.nodes, &class_sym).props["kind"], "class");
    assert_eq!(node(&out.nodes, &hello).props["kind"], "method");

    // Unqualified and `this.` calls resolve within the class.
    edge(&out.edges, &hello, &greet, "CALLS");
    edge(&out.edges, &greet, &suffix, "CALLS");
    edge(
        &out.edges,
        &hello,
        &format!("file:local/demo@{path}"),
        "DEFINED_IN",
    );

    // Every fact is Confirmed T0 with a real span into this file.
    for fact in out
        .nodes
        .iter()
        .map(|n| &n.props)
        .chain(out.edges.iter().map(|e| &e.props))
    {
        let prov = &fact["prov"];
        assert_eq!(prov["tier"], "Deterministic");
        assert_eq!(prov["confidence_tier"], "Confirmed");
        assert_eq!(prov["extractor_id"], "t0.adapter-java");
        let end = prov["evidence"][0]["byte_end"].as_u64().unwrap();
        assert!(end <= source.len() as u64 && end > 0);
    }
}

#[test]
fn imported_class_calls_resolve_across_files_and_missing_targets_gap() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/src/main/java/com/demo/App.java",
        r#"package com.demo;

import com.demo.util.Store;
import com.demo.util.Missing;
import org.external.Lib;

public class App {
    void run() {
        Store.save();
        Missing.run();
        Lib.外部();
    }
}
"#,
    );
    write(
        dir.path(),
        "app/src/main/java/com/demo/util/Store.java",
        r#"package com.demo.util;

public class Store {
    public static void save() {}
}
"#,
    );
    let (out, stats) =
        extract_dir_incremental(dir.path(), &id(), &mut IncrementalCache::default()).unwrap();
    assert_eq!(stats.recomputed_files, 2);

    let src = "sym:local/demo@app/src/main/java/com/demo/App.java#App.run";
    let store_save = "sym:local/demo@app/src/main/java/com/demo/util/Store.java#Store.save";
    // Import-proven cross-file call resolves to the declaring file's symbol.
    let resolved = edge(&out.edges, src, store_save, "CALLS");
    assert_eq!(resolved.props["resolution"], "import-proven");

    // A declared-package import with no such class fails closed to a Gap.
    let gap = out
        .nodes
        .iter()
        .find(|node| node.label == "Gap")
        .expect("missing-target import gaps");
    assert_eq!(gap.props["reason"], "unresolved Java import target");
    assert_eq!(gap.props["callee"], "Missing.run");
    edge(&out.edges, src, &gap.id, "CALLS");

    // A call on a foreign-package import asserts nothing — no CALLS edge,
    // no extra Gap (the import itself is a Confirmed external, AC-0207).
    assert_eq!(
        out.nodes.iter().filter(|node| node.label == "Gap").count(),
        1
    );
}

#[test]
fn spring_endpoints_compose_class_and_method_paths_and_fail_closed_without_import() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/java/com/demo/web/UserController.java",
        r#"package com.demo.web;

import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping("/api/users")
public class UserController {
    @GetMapping("/{id}")
    public String get() { return "u"; }

    @PostMapping
    public String create() { return "c"; }
}
"#,
    );
    // Lookalike annotations with no Spring import prove nothing.
    write(
        dir.path(),
        "src/main/java/com/demo/web/FakeController.java",
        r#"package com.demo.web;

import com.other.web.RestController;
import com.other.web.GetMapping;

@RestController
public class FakeController {
    @GetMapping("/nope")
    public String get() { return "n"; }
}
"#,
    );
    let (out, _) =
        extract_dir_incremental(dir.path(), &id(), &mut IncrementalCache::default()).unwrap();

    let get = node(&out.nodes, "ep:local/demo@GET:/api/users/{id}");
    assert_eq!(get.props["framework"], "spring");
    assert_eq!(get.props["path"], "/api/users/{id}");
    let post = node(&out.nodes, "ep:local/demo@POST:/api/users");
    assert_eq!(post.props["method"], "POST");
    edge(
        &out.edges,
        "ep:local/demo@GET:/api/users/{id}",
        "sym:local/demo@src/main/java/com/demo/web/UserController.java#UserController.get",
        "HANDLES",
    );

    // The unproven controller contributed no endpoints at all.
    assert_eq!(
        out.nodes
            .iter()
            .filter(|node| node.label == "Endpoint")
            .count(),
        2
    );
}

#[test]
fn wildcard_proof_is_per_annotation_package_not_per_vendor() {
    // #170 review: a wildcard of one Spring package must not prove an
    // annotation living in another. Here `stereotype.*` proves @Controller,
    // but the unimported (custom/lookalike) @GetMapping stays unproven —
    // no endpoint is asserted.
    let source = br#"package com.demo.web;

import org.springframework.stereotype.*;

@Controller
public class HomeController {
    @GetMapping("/home")
    public String home() { return "h"; }
}
"#;
    let out = extract_source(
        source,
        "src/main/java/com/demo/web/HomeController.java",
        &id(),
    )
    .unwrap();
    assert_eq!(
        out.nodes
            .iter()
            .filter(|node| node.label == "Endpoint")
            .count(),
        0
    );

    // And a named import from the wrong Spring package proves nothing either.
    let wrong = br#"package com.demo.web;

import org.springframework.stereotype.RestController;
import org.springframework.stereotype.GetMapping;

@RestController
public class WrongController {
    @GetMapping("/wrong")
    public String wrong() { return "w"; }
}
"#;
    let out = extract_source(
        wrong,
        "src/main/java/com/demo/web/WrongController.java",
        &id(),
    )
    .unwrap();
    assert_eq!(
        out.nodes
            .iter()
            .filter(|node| node.label == "Endpoint")
            .count(),
        0
    );
}

#[test]
fn duplicate_type_declarations_are_ambiguous_and_fail_closed() {
    // #170 review: the same FQN declared in two source roots (main + test)
    // must never resolve a Confirmed call to whichever file sorts last —
    // the import is ambiguous, so the call fails closed to a Gap.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/java/com/demo/App.java",
        r#"package com.demo;

import com.demo.util.Store;

public class App {
    void run() {
        Store.save();
    }
}
"#,
    );
    for root in ["src/main/java", "src/test/java"] {
        write(
            dir.path(),
            &format!("{root}/com/demo/util/Store.java"),
            r#"package com.demo.util;

public class Store {
    public static void save() {}
}
"#,
        );
    }
    let (out, _) =
        extract_dir_incremental(dir.path(), &id(), &mut IncrementalCache::default()).unwrap();

    let src = "sym:local/demo@src/main/java/com/demo/App.java#App.run";
    // No Confirmed CALLS edge to either declaration…
    assert!(!out.edges.iter().any(|edge| {
        edge.label == "CALLS" && edge.src == src && edge.dst.contains("Store.save")
    }));
    // …only an explicit Gap.
    let gap = out
        .nodes
        .iter()
        .find(|node| node.label == "Gap")
        .expect("ambiguous duplicate declarations gap");
    assert_eq!(gap.props["callee"], "Store.save");
    edge(&out.edges, src, &gap.id, "CALLS");
}

#[test]
fn named_spring_imports_prove_mappings_too() {
    let source = br#"package com.demo.web;

import org.springframework.web.bind.annotation.RestController;
import org.springframework.web.bind.annotation.DeleteMapping;

@RestController
public class AdminController {
    @DeleteMapping(path = "/admin/cache")
    public void purge() {}
}
"#;
    let path = "src/main/java/com/demo/web/AdminController.java";
    let out = extract_source(source, path, &id()).unwrap();
    let endpoint = node(&out.nodes, "ep:local/demo@DELETE:/admin/cache");
    assert_eq!(endpoint.props["method"], "DELETE");
    assert_eq!(
        endpoint.props["handler_sym"],
        format!("sym:{}@{path}#AdminController.purge", "local/demo")
    );
}

/// AC-0192 (#234): the petclinic shape. Array-initializer paths are Spring's
/// documented multi-path form; each literal is a real route, and a single
/// element must not collapse to `/`.
#[test]
fn array_initializer_mapping_paths_are_literal_routes_not_the_root() {
    let source = br#"package com.demo.web;

import org.springframework.stereotype.Controller;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RequestMapping;

@Controller
@RequestMapping({ "/clinic" })
public class VetController {
    @GetMapping({ "/vets" })
    public String showResourcesVetList() { return "v"; }

    @GetMapping(value = { "/vets.html", "/vets/all" })
    public String showVetList() { return "l"; }

    @GetMapping({})
    public String home() { return "h"; }
}
"#;
    let out = extract_source(source, "src/VetController.java", &id()).unwrap();
    let mut routes: Vec<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| node.props["path"].as_str().unwrap())
        .collect();
    routes.sort_unstable();
    // `{}` is an empty path list — Spring's default, the class base itself.
    assert_eq!(
        routes,
        [
            "/clinic",
            "/clinic/vets",
            "/clinic/vets.html",
            "/clinic/vets/all"
        ]
    );
    edge(
        &out.edges,
        "ep:local/demo@GET:/clinic/vets",
        "sym:local/demo@src/VetController.java#VetController.showResourcesVetList",
        "HANDLES",
    );
    for route in ["/clinic/vets.html", "/clinic/vets/all"] {
        edge(
            &out.edges,
            &format!("ep:local/demo@GET:{route}"),
            "sym:local/demo@src/VetController.java#VetController.showVetList",
            "HANDLES",
        );
    }
    // A composed route cites the class base and the method mapping, on both
    // the Endpoint and its HANDLES edge.
    let endpoint = out
        .nodes
        .iter()
        .find(|node| node.id == "ep:local/demo@GET:/clinic/vets")
        .unwrap();
    let handles = out
        .edges
        .iter()
        .find(|e| e.src == endpoint.id && e.label == "HANDLES")
        .unwrap();
    for prov in [&endpoint.props["prov"], &handles.props["prov"]] {
        assert_eq!(
            cited(source, prov),
            [
                "@RequestMapping({ \"/clinic\" })",
                "@GetMapping({ \"/vets\" })"
            ]
        );
    }
}

/// Source text of every evidence span in `prov`.
fn cited<'a>(source: &'a [u8], prov: &serde_json::Value) -> Vec<&'a str> {
    prov["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|span| {
            let start = span["byte_start"].as_u64().unwrap() as usize;
            let end = span["byte_end"].as_u64().unwrap() as usize;
            std::str::from_utf8(&source[start..end]).unwrap()
        })
        .collect()
}

/// AC-0192 (#234): a mapping whose path is present but not a provable
/// literal is a runtime identity. It becomes an explicit Gap bound to its
/// handler — never a Confirmed endpoint at the `/` default.
#[test]
fn non_literal_mapping_paths_fail_closed_to_route_gaps() {
    let source = br#"package com.demo.web;

import org.springframework.web.bind.annotation.*;

@RestController
public class DynamicController {
    @GetMapping(Paths.VETS)
    public String constant() { return "c"; }

    @GetMapping("/api" + "/owners")
    public String concatenated() { return "o"; }

    @PostMapping(path = { "/ok", Paths.OTHER })
    public String mixed() { return "m"; }

    @GetMapping
    public String root() { return "r"; }
}
"#;
    let out = extract_source(source, "src/DynamicController.java", &id()).unwrap();
    let endpoints: Vec<&str> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| node.id.as_str())
        .collect();
    // Only the path-less mapping is provable (Spring's documented default).
    assert_eq!(endpoints, ["ep:local/demo@GET:/"]);

    let gaps: Vec<&Node> = out
        .nodes
        .iter()
        .filter(|node| node.id.starts_with("gap:route:"))
        .collect();
    assert_eq!(gaps.len(), 3);
    for gap in &gaps {
        assert_eq!(gap.label, "Gap");
        assert_eq!(gap.props["reason"], "dynamic Spring mapping path");
        assert_eq!(gap.props["prov"]["confidence_tier"], "Gap");
    }
    for handler in ["constant", "concatenated", "mixed"] {
        let handler =
            format!("sym:local/demo@src/DynamicController.java#DynamicController.{handler}");
        assert!(
            out.edges.iter().any(|edge| edge.label == "HANDLES"
                && edge.dst == handler
                && edge.src.starts_with("gap:route:")),
            "{handler} must be handled by a route Gap"
        );
    }
}

/// AC-0192 (#234): a non-literal class-level base poisons every mapping
/// under it — composing a literal tail onto an unknown base is still a guess.
#[test]
fn non_literal_class_base_fails_every_mapping_closed() {
    let source = br#"package com.demo.web;

import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping(Api.BASE)
public class BasedController {
    @GetMapping("/items")
    public String items() { return "i"; }
}
"#;
    let out = extract_source(source, "src/BasedController.java", &id()).unwrap();
    assert!(out.nodes.iter().all(|node| node.label != "Endpoint"));
    let gaps: Vec<&Node> = out
        .nodes
        .iter()
        .filter(|node| node.id.starts_with("gap:route:"))
        .collect();
    assert_eq!(gaps.len(), 1);
    // The evidence must point at the expression that made the route
    // unprovable (the class base), not only at the literal method mapping.
    assert_eq!(
        cited(source, &gaps[0].props["prov"]),
        ["@RequestMapping(Api.BASE)", "@GetMapping(\"/items\")"]
    );
}

/// AC-0192 (#432 review): an escaped literal's source spelling is not its
/// runtime value, so it must not become a Confirmed route.
#[test]
fn escaped_literal_mapping_paths_fail_closed() {
    let source = br#"package com.demo.web;

import org.springframework.web.bind.annotation.*;

@RestController
public class EscapedController {
    @GetMapping({ "/foo\u002Dbar" })
    public String unicode() { return "u"; }

    @GetMapping("/tab\there")
    public String tab() { return "t"; }
}
"#;
    let out = extract_source(source, "src/EscapedController.java", &id()).unwrap();
    assert!(out.nodes.iter().all(|node| node.label != "Endpoint"));
    assert_eq!(
        out.nodes
            .iter()
            .filter(|node| node.id.starts_with("gap:route:"))
            .count(),
        2
    );
}

#[test]
fn gitignored_trees_are_not_collected() {
    // AC-0205 (#248): the shared walk honors the tree's own `.gitignore`.
    let dir = tempfile::tempdir().unwrap();
    for (path, body) in [
        (".gitignore", "gen/\n"),
        ("src/App.java", ""),
        ("gen/Api.java", ""),
    ] {
        let file = dir.path().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, body).unwrap();
    }
    let mut files = Vec::new();
    collect_java_files(dir.path(), &mut files).unwrap();
    assert_eq!(files, ["src/App.java"]);
}

/// Sorted `METHOD path` of every Endpoint in `out`.
fn endpoint_routes(out: &Extraction) -> Vec<String> {
    let mut routes: Vec<String> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| {
            format!(
                "{} {}",
                node.props["method"].as_str().unwrap(),
                node.props["path"].as_str().unwrap()
            )
        })
        .collect();
    routes.sort_unstable();
    routes
}

// AC-0206 (#445): a trailing slash spelled in the source survives
// composition — Spring 6 matches `/a` and `/a/` as distinct routes — while
// segments still meet at exactly one `/`.
#[test]
fn spring_routes_keep_literal_trailing_slashes() {
    let slashed_base = br#"package com.demo.web;

import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping("/api/")
public class ApiController {
    @GetMapping
    public String root() { return "r"; }

    @PostMapping("/items")
    public String create() { return "c"; }

    @PutMapping({ "/x", "/x/" })
    public String put() { return "p"; }
}
"#;
    let out = extract_source(slashed_base, "src/ApiController.java", &id()).unwrap();
    assert_eq!(
        endpoint_routes(&out),
        ["GET /api/", "POST /api/items", "PUT /api/x", "PUT /api/x/"]
    );
    edge(
        &out.edges,
        "ep:local/demo@GET:/api/",
        "sym:local/demo@src/ApiController.java#ApiController.root",
        "HANDLES",
    );

    let slashed_tail = br#"package com.demo.web;

import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping("/v1")
public class V1Controller {
    @GetMapping("/")
    public String root() { return "r"; }

    @GetMapping("items/")
    public String items() { return "i"; }
}
"#;
    let out = extract_source(slashed_tail, "src/V1Controller.java", &id()).unwrap();
    assert_eq!(endpoint_routes(&out), ["GET /v1/", "GET /v1/items/"]);
    assert!(!out.nodes.iter().any(|node| node.id.contains("//")));

    // A run of trailing slashes keeps exactly one.
    let doubled = br#"package com.demo.web;

import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping("/v2//")
public class V2Controller {
    @GetMapping
    public String root() { return "r"; }

    @GetMapping("items//")
    public String items() { return "i"; }
}
"#;
    let out = extract_source(doubled, "src/V2Controller.java", &id()).unwrap();
    assert_eq!(endpoint_routes(&out), ["GET /v2/", "GET /v2/items/"]);
}

fn prov_of(node: &Node) -> Provenance {
    serde_json::from_value(node.props["prov"].clone())
        .unwrap_or_else(|_| panic!("{} carries no provenance", node.id))
}

#[test]
fn import_targets_carry_provenance_and_only_proven_externals_confirm() {
    // AC-0207 (#237): every closed-over endpoint cites the import that named
    // it; only a package the repository provably does not declare is a
    // Confirmed external boundary, in-repo types resolve to their File, and
    // an in-system target that does not resolve stays an explicit Gap.
    let dir = tempfile::tempdir().unwrap();
    let app = r#"package com.demo;

import com.demo.util.Store;
import com.demo.util.Missing;
import com.demo.util.*;
import com.demo.kt.Widget;
import com.Stray;
import jakarta.persistence.Entity;
import static org.junit.Assert.assertEquals;

public class App {}
"#;
    write(dir.path(), "src/main/java/com/demo/App.java", app);
    write(
        dir.path(),
        "src/main/java/com/demo/util/Store.java",
        "package com.demo.util;\n\npublic class Store {}\n",
    );
    // A Kotlin package in a mixed JVM tree is the repository's own.
    write(
        dir.path(),
        "src/main/kotlin/com/demo/kt/Widget.kt",
        "/* header */\n@file:JvmName(\"W\")\npackage com.demo.kt\n\nclass Widget\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let app_file = "file:local/demo@src/main/java/com/demo/App.java";

    // The in-repo type import resolves to the declaring File, Confirmed.
    let store = edge(
        &out.edges,
        app_file,
        "file:local/demo@src/main/java/com/demo/util/Store.java",
        "IMPORTS",
    );
    assert_eq!(store.props["resolution"], "import-proven");
    assert!(
        !out.nodes
            .iter()
            .any(|node| node.id == "mod:com.demo.util.Store")
    );

    let expect = |id: &str, boundary: &str, confidence: ConfidenceTier, statement: &str| {
        let node = node(&out.nodes, id);
        assert_eq!(node.label, "Module", "{id}");
        assert_eq!(node.props["placeholder"], true, "{id}");
        assert_eq!(node.props["boundary"], boundary, "{id}");
        assert!(node.props["reason"].as_str().is_some_and(|r| !r.is_empty()));
        let prov = prov_of(node);
        assert_eq!(prov.confidence_tier, confidence, "{id}");
        assert_eq!(prov.extractor_id, EXTRACTOR_ID);
        // The evidence is the exact import statement that named it.
        let span = &prov.evidence[0];
        assert_eq!(span.path, "src/main/java/com/demo/App.java");
        assert_eq!(
            &app[span.byte_start as usize..span.byte_end as usize],
            statement,
            "{id}"
        );
    };
    // #464 (ADR-0033): a proven-external import keys on its package, not the
    // imported type/member.
    expect(
        "mod:jakarta.persistence",
        "external",
        ConfidenceTier::Confirmed,
        "import jakarta.persistence.Entity;",
    );
    expect(
        "mod:org.junit",
        "external",
        ConfidenceTier::Confirmed,
        "import static org.junit.Assert.assertEquals;",
    );
    expect(
        "mod:com.demo.util",
        "internal",
        ConfidenceTier::Confirmed,
        "import com.demo.util.*;",
    );
    // Declared package, no such type: an unresolved in-system hop.
    expect(
        "mod:com.demo.util.Missing",
        "unresolved",
        ConfidenceTier::Gap,
        "import com.demo.util.Missing;",
    );
    // The Kotlin-declared package is in-system, never external.
    expect(
        "mod:com.demo.kt.Widget",
        "unresolved",
        ConfidenceTier::Gap,
        "import com.demo.kt.Widget;",
    );
    // An enclosing package of a declared one cannot be proven external.
    expect(
        "mod:com.Stray",
        "unresolved",
        ConfidenceTier::Gap,
        "import com.Stray;",
    );
    // No fact of any kind is left without valid provenance.
    for node in &out.nodes {
        prov_of(node).validate().unwrap();
    }
}

#[test]
fn foreign_headers_the_scan_cannot_read_never_confirm_externals() {
    // AC-0207 (#237 review): a multiline Kotlin file annotation does not
    // hide its package, and a header the scan cannot parse leaves every
    // undeclared import an explicit Gap rather than Confirmed external.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/java/com/demo/App.java",
        "package com.demo;\n\nimport com.other.kt.Widget;\nimport jakarta.persistence.Entity;\n\npublic class App {}\n",
    );
    write(
        dir.path(),
        "src/main/kotlin/com/other/kt/Widget.kt",
        "@file:JvmName(\n    \"Widgets\"\n)\n\npackage com.other.kt\n\nclass Widget\n",
    );
    let boundary = |out: &Extraction, id: &str| {
        let node = node(&out.nodes, id);
        (
            node.props["boundary"].as_str().unwrap().to_string(),
            prov_of(node).confidence_tier,
        )
    };
    let out = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(
        boundary(&out, "mod:com.other.kt.Widget"),
        ("unresolved".into(), ConfidenceTier::Gap)
    );
    assert_eq!(
        boundary(&out, "mod:jakarta.persistence"),
        ("external".into(), ConfidenceTier::Confirmed)
    );

    write(
        dir.path(),
        "src/main/kotlin/com/other/Broken.kt",
        "@file:JvmName(\n    \"never closed\"\n\npackage com.hidden\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(
        boundary(&out, "mod:jakarta.persistence.Entity"),
        ("unresolved".into(), ConfidenceTier::Gap)
    );
}

#[test]
fn only_a_complete_import_target_retargets_to_its_file() {
    // AC-0207 (#237 review): `a.Store.Missing` is not proven by `a.Store`;
    // a declared member is. An unproven member import stays a Gap.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/a/App.java",
        "package a;\n\nimport static a.Store.save;\nimport static a.Store.missing;\nimport a.Store.Missing;\n\npublic class App {}\n",
    );
    write(
        dir.path(),
        "src/a/Store.java",
        "package a;\n\npublic class Store {\n    public static void save() {}\n}\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let store = "file:local/demo@src/a/Store.java";
    let into_store = out
        .edges
        .iter()
        .filter(|edge| edge.label == "IMPORTS" && edge.dst == store)
        .count();
    assert_eq!(into_store, 1, "only the declared member retargets");
    for unproven in ["mod:a.Store.missing", "mod:a.Store.Missing"] {
        let node = node(&out.nodes, unproven);
        assert_eq!(node.props["boundary"], "unresolved", "{unproven}");
        assert_eq!(prov_of(node).confidence_tier, ConfidenceTier::Gap);
    }
}

#[test]
fn external_imports_from_one_package_share_its_module_id_and_keep_their_specifier() {
    // AC-0226 (#464, ADR-0033): two types proven external from the same
    // package key their `IMPORTS` edge on that shared package id rather than
    // the imported type, and each edge still names exactly what it imported.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/java/com/demo/App.java",
        "package com.demo;\n\nimport jakarta.persistence.Entity;\nimport jakarta.persistence.Table;\n\npublic class App {}\n",
    );
    let out = extract_dir(dir.path(), &id()).unwrap();
    let app_file = "file:local/demo@src/main/java/com/demo/App.java";
    let module_edges: Vec<&Edge> = out
        .edges
        .iter()
        .filter(|edge| edge.label == "IMPORTS" && edge.src == app_file)
        .collect();
    assert_eq!(module_edges.len(), 2);
    assert!(
        module_edges
            .iter()
            .all(|edge| edge.dst == "mod:jakarta.persistence"),
        "{module_edges:?}"
    );
    let specifiers: BTreeSet<&str> = module_edges
        .iter()
        .map(|edge| edge.props["specifier"].as_str().unwrap())
        .collect();
    assert_eq!(
        specifiers,
        BTreeSet::from(["jakarta.persistence.Entity", "jakarta.persistence.Table"])
    );
    assert_eq!(
        out.nodes
            .iter()
            .filter(|node| node.id == "mod:jakarta.persistence")
            .count(),
        1,
        "one shared Module node, not one per imported type"
    );
}
