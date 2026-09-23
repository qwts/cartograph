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

    // A foreign-package import asserts nothing — no edge, no extra gap.
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
