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

// AC-0098: every Kotlin declaration kind — classes, interfaces, objects,
// data classes, enums, and top-level/member/extension functions — becomes a
// Confirmed T0 Symbol with exact evidence spans.
#[test]
fn declaration_kinds_and_functions_carry_confirmed_provenance() {
    let source = br#"package com.demo

class Greeter {
    fun hello(): String = greet()

    private fun greet(): String = this.suffix()

    private fun suffix(): String = "!"
}

interface Repo
object Config
data class User(val name: String)
enum class Role { ADMIN, USER }

fun topLevel() {
    listOf(1, 2, 3).map { it * 2 }
}

suspend fun refresh() {
    topLevel()
}

fun String.shout(): String = this.uppercase()
"#;
    let path = "src/main/kotlin/com/demo/Greeter.kt";
    let out = extract_source(source, path, &id()).unwrap();

    let sym = |name: &str| format!("sym:local/demo@{path}#{name}");
    assert_eq!(node(&out.nodes, &sym("Greeter")).props["kind"], "class");
    assert_eq!(node(&out.nodes, &sym("Repo")).props["kind"], "interface");
    assert_eq!(node(&out.nodes, &sym("Config")).props["kind"], "object");
    assert_eq!(node(&out.nodes, &sym("User")).props["kind"], "data class");
    assert_eq!(node(&out.nodes, &sym("Role")).props["kind"], "enum class");
    assert_eq!(
        node(&out.nodes, &sym("Greeter.hello")).props["kind"],
        "function"
    );
    assert_eq!(node(&out.nodes, &sym("topLevel")).props["kind"], "function");
    // `suspend` and trailing-lambda syntax parse (grammar verify-at-build);
    // the suspend function is a plain function symbol calling another
    // top-level function of the same file.
    edge(&out.edges, &sym("refresh"), &sym("topLevel"), "CALLS");
    // Extension functions are qualified by their receiver type.
    let shout = node(&out.nodes, &sym("String.shout"));
    assert_eq!(shout.props["kind"], "function");
    assert_eq!(shout.props["receiver"], "String");

    // Unqualified and `this.` calls resolve within the class.
    edge(
        &out.edges,
        &sym("Greeter.hello"),
        &sym("Greeter.greet"),
        "CALLS",
    );
    edge(
        &out.edges,
        &sym("Greeter.greet"),
        &sym("Greeter.suffix"),
        "CALLS",
    );
    edge(
        &out.edges,
        &sym("Greeter.hello"),
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
        assert_eq!(prov["extractor_id"], "t0.adapter-kotlin");
        let end = prov["evidence"][0]["byte_end"].as_u64().unwrap();
        assert!(end <= source.len() as u64 && end > 0);
    }
}

// AC-0098: import-proven cross-file calls — object/type receivers and
// imported top-level functions — join repo-wide; a declared-package import
// whose target cannot be proven fails closed to an explicit Gap; foreign
// packages assert nothing; unproven receivers (locals, properties) assert
// nothing.
#[test]
fn imported_calls_resolve_across_files_and_missing_targets_gap() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "app/src/main/kotlin/com/demo/App.kt",
        r#"package com.demo

import com.demo.util.Store
import com.demo.util.publish
import com.demo.util.Missing
import org.external.Lib

class App(private val helper: Store) {
    fun run() {
        Store.save()
        publish()
        Missing.run()
        Lib.emit()
        helper.reload()
    }
}
"#,
    );
    write(
        dir.path(),
        "app/src/main/kotlin/com/demo/util/Store.kt",
        r#"package com.demo.util

object Store {
    fun save() {}
}

fun publish() {}
"#,
    );
    let (out, stats) =
        extract_dir_incremental(dir.path(), &id(), &mut IncrementalCache::default()).unwrap();
    assert_eq!(stats.recomputed_files, 2);

    let src = "sym:local/demo@app/src/main/kotlin/com/demo/App.kt#App.run";
    let store_save = "sym:local/demo@app/src/main/kotlin/com/demo/util/Store.kt#Store.save";
    let publish = "sym:local/demo@app/src/main/kotlin/com/demo/util/Store.kt#publish";
    // Import-proven receiver call resolves to the declaring file's symbol.
    let resolved = edge(&out.edges, src, store_save, "CALLS");
    assert_eq!(resolved.props["resolution"], "import-proven");
    // An imported top-level function resolves the same way.
    let resolved = edge(&out.edges, src, publish, "CALLS");
    assert_eq!(resolved.props["resolution"], "import-proven");

    // A declared-package import with no such declaration fails closed.
    let gap = out
        .nodes
        .iter()
        .find(|node| node.label == "Gap")
        .expect("missing-target import gaps");
    assert_eq!(gap.props["reason"], "unresolved Kotlin import target");
    assert_eq!(gap.props["callee"], "Missing.run");
    edge(&out.edges, src, &gap.id, "CALLS");

    // A foreign-package import asserts nothing, and an unproven receiver
    // (the `helper` property) asserts nothing — no edge, no extra gap.
    assert_eq!(
        out.nodes.iter().filter(|node| node.label == "Gap").count(),
        1
    );
    assert!(
        !out.edges
            .iter()
            .any(|edge| edge.label == "CALLS" && edge.dst.contains("reload"))
    );
}

// AC-0098: duplicate FQNs (the same declaration in two source roots) are
// ambiguous and fail closed to a Gap — never resolved to whichever file
// sorts last (#170 standard).
#[test]
fn duplicate_declarations_are_ambiguous_and_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/kotlin/com/demo/App.kt",
        r#"package com.demo

import com.demo.util.Store

class App {
    fun run() {
        Store.save()
    }
}
"#,
    );
    for root in ["src/main/kotlin", "src/test/kotlin"] {
        write(
            dir.path(),
            &format!("{root}/com/demo/util/Store.kt"),
            r#"package com.demo.util

object Store {
    fun save() {}
}
"#,
        );
    }
    let (out, _) =
        extract_dir_incremental(dir.path(), &id(), &mut IncrementalCache::default()).unwrap();

    let src = "sym:local/demo@src/main/kotlin/com/demo/App.kt#App.run";
    assert!(!out.edges.iter().any(|edge| {
        edge.label == "CALLS" && edge.src == src && edge.dst.contains("Store.save")
    }));
    let gap = out
        .nodes
        .iter()
        .find(|node| node.label == "Gap")
        .expect("ambiguous duplicate declarations gap");
    assert_eq!(gap.props["callee"], "Store.save");
    edge(&out.edges, src, &gap.id, "CALLS");
}

// AC-0098: Spring Web endpoints in Kotlin compose class-level
// @RequestMapping with method-level mappings, and lookalike annotations
// without the proving import produce nothing. The controller fixture keeps
// trailing top-level declarations after the class on purpose: that is the
// shape that makes tree-sitter-kotlin-ng resolve the class annotations
// into a preceding `annotated_expression` statement (grammar quirk, #212),
// so this exercises the recovery path, not just the plain modifiers path.
#[test]
fn spring_endpoints_compose_class_and_method_paths_and_fail_closed_without_import() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/main/kotlin/com/demo/web/UserController.kt",
        r#"package com.demo.web

import org.springframework.web.bind.annotation.*

@RestController
@RequestMapping("/api/users")
class UserController {
    @GetMapping("/{id}")
    fun get(id: String): String = "u"

    @PostMapping
    fun create(): String = "c"
}

data class UserDto(val name: String)
"#,
    );
    // Lookalike annotations with no Spring import prove nothing.
    write(
        dir.path(),
        "src/main/kotlin/com/demo/web/FakeController.kt",
        r#"package com.demo.web

import com.other.web.RestController
import com.other.web.GetMapping

@RestController
class FakeController {
    @GetMapping("/nope")
    fun get(): String = "n"
}

object Marker
"#,
    );
    let (out, _) =
        extract_dir_incremental(dir.path(), &id(), &mut IncrementalCache::default()).unwrap();

    let get = node(&out.nodes, "ep:local/demo@GET:/api/users/{id}");
    assert_eq!(get.props["framework"], "spring");
    assert_eq!(get.props["language"], "kotlin");
    assert_eq!(get.props["path"], "/api/users/{id}");
    let post = node(&out.nodes, "ep:local/demo@POST:/api/users");
    assert_eq!(post.props["method"], "POST");
    edge(
        &out.edges,
        "ep:local/demo@GET:/api/users/{id}",
        "sym:local/demo@src/main/kotlin/com/demo/web/UserController.kt#UserController.get",
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

// AC-0098 (per-package proof, the AC-0080 standard): a wildcard of one
// Spring package must not prove an annotation living in another, and a
// named import from the wrong Spring package proves nothing either.
#[test]
fn wildcard_proof_is_per_annotation_package_not_per_vendor() {
    let source = br#"package com.demo.web

import org.springframework.stereotype.*

@Controller
class HomeController {
    @GetMapping("/home")
    fun home(): String = "h"
}
"#;
    let out = extract_source(
        source,
        "src/main/kotlin/com/demo/web/HomeController.kt",
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

    let wrong = br#"package com.demo.web

import org.springframework.stereotype.RestController
import org.springframework.stereotype.GetMapping

@RestController
class WrongController {
    @GetMapping("/wrong")
    fun wrong(): String = "w"
}
"#;
    let out = extract_source(
        wrong,
        "src/main/kotlin/com/demo/web/WrongController.kt",
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

// AC-0098: named Spring imports (including `path =` named arguments) prove
// mappings; a mapping whose path argument is present but not a provable
// literal (a `$` template, a constant reference) is a runtime identity —
// it becomes an explicit route Gap, never a Confirmed endpoint with a
// guessed, interpolated, or defaulted path (#218 review).
#[test]
fn named_spring_imports_prove_mappings_and_dynamic_paths_gap() {
    let source = br#"package com.demo.web

import org.springframework.web.bind.annotation.RestController
import org.springframework.web.bind.annotation.DeleteMapping
import org.springframework.web.bind.annotation.GetMapping
import org.springframework.web.bind.annotation.PostMapping

@RestController
class AdminController {
    @DeleteMapping(path = "/admin/cache")
    fun purge() {}

    @GetMapping("/admin/$suffix")
    fun dynamic(): String = "d"

    @PostMapping(path = ROUTE)
    fun constant(): String = "c"
}
"#;
    let path = "src/main/kotlin/com/demo/web/AdminController.kt";
    let out = extract_source(source, path, &id()).unwrap();
    let endpoint = node(&out.nodes, "ep:local/demo@DELETE:/admin/cache");
    assert_eq!(endpoint.props["method"], "DELETE");
    assert_eq!(
        endpoint.props["handler_sym"],
        format!("sym:{}@{path}#AdminController.purge", "local/demo")
    );
    // The literal mapping is the only Confirmed endpoint: the template and
    // constant-reference paths must not surface as endpoints at all — not
    // interpolated, not collapsed to the class base or "/".
    assert_eq!(
        out.nodes
            .iter()
            .filter(|node| node.label == "Endpoint")
            .count(),
        1
    );
    // Each unprovable mapping is an explicit Gap that still names its
    // handler, so the partial fact is visible rather than silently absent.
    let gaps: Vec<_> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Gap")
        .collect();
    assert_eq!(gaps.len(), 2);
    for (gap, method, handler) in [
        (gaps[0], "GET", "AdminController.dynamic"),
        (gaps[1], "POST", "AdminController.constant"),
    ] {
        assert_eq!(gap.props["reason"], "dynamic Spring mapping path");
        assert_eq!(gap.props["method"], method);
        assert_eq!(
            gap.props["handler_sym"],
            format!("sym:local/demo@{path}#{handler}")
        );
        assert_eq!(gap.props["prov"]["confidence_tier"], "Gap");
        edge(
            &out.edges,
            &gap.id,
            &format!("sym:local/demo@{path}#{handler}"),
            "HANDLES",
        );
    }
}

// AC-0098 (#218 review): a *class-level* @RequestMapping whose path is a
// runtime identity poisons every mapping beneath it — even fully literal
// method paths cannot compose into a Confirmed route, so each one fails
// closed to a route Gap. An argument list with no path-designating
// argument, by contrast, is Spring's own documented default ("") and
// composes normally.
#[test]
fn dynamic_class_base_paths_fail_closed_and_non_path_arguments_default() {
    let source = br#"package com.demo.web

import org.springframework.web.bind.annotation.*

@RestController
@RequestMapping(BASE)
class DynController {
    @GetMapping("/literal")
    fun get(): String = "g"
}
"#;
    let out = extract_source(
        source,
        "src/main/kotlin/com/demo/web/DynController.kt",
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
    let gap = out
        .nodes
        .iter()
        .find(|node| node.label == "Gap")
        .expect("dynamic base gap");
    assert_eq!(gap.props["reason"], "dynamic Spring mapping path");

    // Named non-path arguments (produces, consumes, …) do not designate a
    // path: the mapping keeps Spring's default "" and composes Confirmed.
    let defaulted = br#"package com.demo.web

import org.springframework.web.bind.annotation.*

@RestController
@RequestMapping("/api")
class JsonController {
    @GetMapping(produces = "application/json")
    fun all(): String = "[]"
}
"#;
    let out = extract_source(
        defaulted,
        "src/main/kotlin/com/demo/web/JsonController.kt",
        &id(),
    )
    .unwrap();
    let endpoint = node(&out.nodes, "ep:local/demo@GET:/api");
    assert_eq!(endpoint.props["path"], "/api");
    assert!(!out.nodes.iter().any(|node| node.label == "Gap"));
}

// AC-0098: content-identical files are reused from the incremental cache
// (with provenance retargeted to the new commit), changed files are
// re-parsed, and deleted files leave the cache.
#[test]
fn incremental_cache_reuses_unchanged_files_and_retargets_commit() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "src/A.kt",
        "package com.demo\n\nclass A {\n    fun a() {}\n}\n",
    );
    write(
        dir.path(),
        "src/B.kt",
        "package com.demo\n\nclass B {\n    fun b() {}\n}\n",
    );
    let mut cache = IncrementalCache::default();
    let (first, stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(stats.recomputed_files, 2);
    assert_eq!(stats.reused_files, 0);

    let second_id = SourceId {
        repo: "local/demo",
        commit: "abc123",
    };
    let (second, stats) = extract_dir_incremental(dir.path(), &second_id, &mut cache).unwrap();
    assert_eq!(stats.recomputed_files, 0);
    assert_eq!(stats.reused_files, 2);
    assert_eq!(first.nodes.len(), second.nodes.len());
    let a = node(&second.nodes, "sym:local/demo@src/A.kt#A");
    assert_eq!(a.props["prov"]["evidence"][0]["commit_sha"], "abc123");

    // A changed file re-parses; a deleted file leaves the cache.
    write(
        dir.path(),
        "src/A.kt",
        "package com.demo\n\nclass A {\n    fun a2() {}\n}\n",
    );
    std::fs::remove_file(dir.path().join("src/B.kt")).unwrap();
    let (third, stats) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(stats.recomputed_files, 1);
    assert_eq!(stats.reused_files, 0);
    assert_eq!(stats.deleted_files, 1);
    node(&third.nodes, "sym:local/demo@src/A.kt#A.a2");
    assert!(!third.nodes.iter().any(|node| node.id.contains("B.kt")));
}

// AC-0098 (#209 hook): the progress callback sees every Kotlin file once,
// as a repo-relative path, in the deterministic sorted walk order — cache
// hits included.
#[test]
fn progress_hook_reports_each_file_in_sorted_order() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/b/Late.kt", "package b\n\nclass Late\n");
    write(dir.path(), "src/a/Early.kt", "package a\n\nclass Early\n");
    write(dir.path(), "build.gradle.kts", "plugins { }\n");
    let mut cache = IncrementalCache::default();
    let mut seen = Vec::new();
    let mut on_file = |path: &str| seen.push(path.to_string());
    extract_dir_incremental_with_progress(dir.path(), &id(), &mut cache, &mut on_file).unwrap();
    assert_eq!(
        seen,
        ["build.gradle.kts", "src/a/Early.kt", "src/b/Late.kt"]
    );

    // Reused files still report — the hook narrates reads, not parses.
    let mut seen_again = Vec::new();
    let mut on_file = |path: &str| seen_again.push(path.to_string());
    extract_dir_incremental_with_progress(dir.path(), &id(), &mut cache, &mut on_file).unwrap();
    assert_eq!(seen, seen_again);
}
