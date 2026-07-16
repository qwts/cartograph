//! Java deterministic (T0) language adapter (SPEC-00 §3.3, #168).
//!
//! Classes, interfaces, enums, records, and methods become Symbols; imports
//! and import-proven cross-file calls become the server graph, with
//! unresolved project-local targets failing closed to explicit Gaps.
//! Annotation-proven Spring Web mappings become Endpoint/HANDLES facts with
//! class+method path composition. Adapters are per language, not per JDK
//! version: the grammar parses current syntax and anything it cannot prove
//! is simply not asserted. This tier never calls an LLM and every emitted
//! fact carries exact source-span provenance.

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node as TsNode, Parser, Query, QueryCursor};

const EXTRACTOR_ID: &str = "t0.adapter-java";

/// Java extraction errors.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    /// tree-sitter grammar/version mismatch.
    #[error("language: {0}")]
    Language(#[from] tree_sitter::LanguageError),
    /// The parser returned no tree.
    #[error("parse produced no tree for {0}")]
    NoTree(String),
    /// Filesystem failure while walking a directory.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Identity of the source being recovered.
pub struct SourceId<'a> {
    /// Repository identity.
    pub repo: &'a str,
    /// Commit SHA, or `workdir` for an unversioned tree.
    pub commit: &'a str,
}

/// Java graph facts from one file or directory.
#[derive(Debug, Clone, Default)]
pub struct Extraction {
    /// Recovered nodes.
    pub nodes: Vec<Node>,
    /// Recovered edges.
    pub edges: Vec<Edge>,
    pending_calls: Vec<PendingCall>,
    declared_types: Vec<DeclaredType>,
}

/// A call to an imported type, resolvable only with the whole directory in
/// view (the import names a FQN; only the repo-wide type index knows which
/// file declares it).
#[derive(Debug, Clone)]
struct PendingCall {
    src: String,
    fqn: String,
    method: String,
    resolved_props: serde_json::Value,
    gap: (Node, Edge),
}

/// A type declaration and the fully-qualified name it answers to.
#[derive(Debug, Clone)]
struct DeclaredType {
    fqn: String,
    path: String,
    qualified: String,
}

#[derive(Debug, Clone)]
struct CachedFile {
    source_hash: String,
    extraction: Extraction,
}

/// Reusable per-file Java parse cache.
#[derive(Debug, Default)]
pub struct IncrementalCache {
    files: BTreeMap<String, CachedFile>,
}

/// Physical Java source work performed by an incremental extraction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IncrementalStats {
    /// New or byte-changed files parsed.
    pub recomputed_files: u64,
    /// Content-identical files reused.
    pub reused_files: u64,
    /// Cached files removed because the source disappeared.
    pub deleted_files: u64,
}

struct FileCx<'a> {
    source: &'a [u8],
    path: &'a str,
    id: &'a SourceId<'a>,
}

impl FileCx<'_> {
    fn text(&self, node: &TsNode<'_>) -> &str {
        node.utf8_text(self.source).unwrap_or("")
    }

    fn prov(&self, node: &TsNode<'_>, fact: &str) -> serde_json::Value {
        self.prov_with_confidence(node, ConfidenceTier::Confirmed, fact)
    }

    fn prov_with_confidence(
        &self,
        node: &TsNode<'_>,
        confidence: ConfidenceTier,
        fact: &str,
    ) -> serde_json::Value {
        let provenance = Provenance::new(
            Tier::Deterministic,
            confidence,
            vec![EvidenceRef {
                repo: self.id.repo.into(),
                path: self.path.into(),
                byte_start: node.start_byte() as u64,
                byte_end: node.end_byte() as u64,
                commit_sha: self.id.commit.into(),
            }],
            EXTRACTOR_ID,
            fact.as_bytes(),
        )
        .expect("Deterministic confidence is within its ceiling");
        serde_json::to_value(provenance).expect("provenance serializes")
    }
}

fn file_id(repo: &str, path: &str) -> String {
    format!("file:{repo}@{path}")
}

fn symbol_id(repo: &str, path: &str, name: &str) -> String {
    format!("sym:{repo}@{path}#{name}")
}

fn retarget_props_commit(props: &mut serde_json::Value, commit: &str) {
    let Ok(mut provenance) =
        serde_json::from_value::<Provenance>(props.get("prov").cloned().unwrap_or_default())
    else {
        return;
    };
    for evidence in &mut provenance.evidence {
        evidence.commit_sha = commit.into();
    }
    props["prov"] = serde_json::to_value(provenance).expect("provenance serializes");
}

fn retarget_commit(extraction: &mut Extraction, commit: &str) {
    for node in &mut extraction.nodes {
        retarget_props_commit(&mut node.props, commit);
    }
    for edge in &mut extraction.edges {
        retarget_props_commit(&mut edge.props, commit);
    }
    for pending in &mut extraction.pending_calls {
        retarget_props_commit(&mut pending.resolved_props, commit);
        retarget_props_commit(&mut pending.gap.0.props, commit);
        retarget_props_commit(&mut pending.gap.1.props, commit);
    }
}

const TYPE_KINDS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
];

/// The dot-joined chain of enclosing type names (outermost first) for a
/// node, e.g. `Outer.Inner` for a method inside a nested class.
fn enclosing_type_chain(cx: &FileCx<'_>, mut node: TsNode<'_>) -> Vec<String> {
    let mut chain = Vec::new();
    while let Some(parent) = node.parent() {
        if TYPE_KINDS.contains(&parent.kind())
            && let Some(name) = parent.child_by_field_name("name")
        {
            chain.push(cx.text(&name).to_string());
        }
        node = parent;
    }
    chain.reverse();
    chain
}

/// One file's import surface: named bindings (simple name → FQN) plus
/// wildcard-imported packages. Static imports are ignored — a statically
/// imported method call is not provable to a type in v1.
#[derive(Debug, Default)]
struct Imports {
    bindings: HashMap<String, String>,
    wildcard_packages: Vec<String>,
}

fn parse_imports(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    language: &tree_sitter::Language,
    out: &mut Extraction,
) -> Imports {
    let query = Query::new(language, "(import_declaration) @import").expect("static query");
    let mut imports = Imports::default();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, cx.source);
    while let Some(found) = matches.next() {
        let statement = found.captures[0].node;
        let raw = cx.text(&statement).replace(['\n', '\\'], " ");
        let Some(rest) = raw.trim().strip_prefix("import") else {
            continue;
        };
        let rest = rest.trim().trim_end_matches(';').trim();
        let (is_static, rest) = match rest.strip_prefix("static ") {
            Some(tail) => (true, tail.trim()),
            None => (false, rest),
        };
        let (module, wildcard) = match rest.strip_suffix(".*") {
            Some(package) => (package.to_string(), true),
            None => (rest.to_string(), false),
        };
        if module.is_empty() {
            continue;
        }
        if !is_static {
            if wildcard {
                imports.wildcard_packages.push(module.clone());
            } else if let Some((_, simple)) = module.rsplit_once('.') {
                imports.bindings.insert(simple.to_string(), module.clone());
            }
        }
        out.edges.push(Edge {
            src: file_id(cx.id.repo, cx.path),
            dst: format!("mod:{module}"),
            label: "IMPORTS".into(),
            props: serde_json::json!({
                "specifier": module,
                "prov": cx.prov(&statement, &format!("IMPORTS {}", cx.text(&statement))),
            }),
        });
    }
    imports
}

/// Annotations attached to a declaration via its `modifiers` child:
/// `(simple name, annotation node)` pairs.
fn declaration_annotations<'t>(
    cx: &FileCx<'_>,
    declaration: TsNode<'t>,
) -> Vec<(String, TsNode<'t>)> {
    let mut found = Vec::new();
    let mut walk = declaration.walk();
    for child in declaration.named_children(&mut walk) {
        if child.kind() != "modifiers" {
            continue;
        }
        let mut inner = child.walk();
        for annotation in child.named_children(&mut inner) {
            if !matches!(annotation.kind(), "annotation" | "marker_annotation") {
                continue;
            }
            let Some(name) = annotation.child_by_field_name("name") else {
                continue;
            };
            let simple = cx.text(&name);
            let simple = simple.rsplit('.').next().unwrap_or(simple).to_string();
            found.push((simple, annotation));
        }
    }
    found
}

/// The literal path argument of a mapping annotation: a bare string, or a
/// `value =` / `path =` pair. Anything non-literal is not asserted.
fn annotation_literal_path(cx: &FileCx<'_>, annotation: TsNode<'_>) -> Option<String> {
    let arguments = annotation.child_by_field_name("arguments")?;
    let mut walk = arguments.walk();
    for argument in arguments.named_children(&mut walk) {
        match argument.kind() {
            "string_literal" => return Some(cx.text(&argument).trim_matches('"').to_string()),
            "element_value_pair" => {
                let (Some(key), Some(value)) = (
                    argument.child_by_field_name("key"),
                    argument.child_by_field_name("value"),
                ) else {
                    continue;
                };
                if matches!(cx.text(&key), "value" | "path") && value.kind() == "string_literal" {
                    return Some(cx.text(&value).trim_matches('"').to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// The exact Spring package each recognized annotation lives in. Proof is
/// per annotation, not per vendor: a wildcard of one Spring package must
/// never prove an annotation from another (#170 review, AC-0080).
fn spring_annotation_package(name: &str) -> Option<&'static str> {
    match name {
        "RestController" | "RequestMapping" | "GetMapping" | "PostMapping" | "PutMapping"
        | "DeleteMapping" | "PatchMapping" => Some("org.springframework.web.bind.annotation"),
        "Controller" => Some("org.springframework.stereotype"),
        _ => None,
    }
}

/// A Spring annotation is proven only by its import: a named import of
/// exactly `{declaring package}.{name}`, or a wildcard import of exactly
/// its declaring package. Lookalikes — including same-named annotations
/// from other packages, Spring or not — prove nothing.
fn spring_proven(name: &str, imports: &Imports) -> bool {
    let Some(package) = spring_annotation_package(name) else {
        return false;
    };
    if let Some(fqn) = imports.bindings.get(name) {
        return fqn == &format!("{package}.{name}");
    }
    imports
        .wildcard_packages
        .iter()
        .any(|wildcard| wildcard == package)
}

fn mapping_method(annotation_name: &str) -> Option<&'static str> {
    match annotation_name {
        "GetMapping" => Some("GET"),
        "PostMapping" => Some("POST"),
        "PutMapping" => Some("PUT"),
        "DeleteMapping" => Some("DELETE"),
        "PatchMapping" => Some("PATCH"),
        _ => None,
    }
}

fn join_route(base: &str, tail: &str) -> String {
    let base = base.trim_end_matches('/');
    let tail = tail.trim_start_matches('/');
    match (base.is_empty(), tail.is_empty()) {
        (true, true) => "/".to_string(),
        (true, false) => format!("/{tail}"),
        (false, true) => base.to_string(),
        (false, false) => format!("{base}/{tail}"),
    }
}

fn close_over_placeholders(extraction: &mut Extraction) {
    let mut known = extraction
        .nodes
        .iter()
        .map(|node| node.id.clone())
        .collect::<HashSet<_>>();
    let mut placeholders = Vec::new();
    for edge in &extraction.edges {
        for id in [&edge.src, &edge.dst] {
            if known.contains(id) {
                continue;
            }
            let label = match id.split(':').next() {
                Some("file") => "File",
                Some("sym") => "Symbol",
                Some("mod") => "Module",
                Some("ep") => "Endpoint",
                Some("gap") => "Gap",
                _ => "Unknown",
            };
            placeholders.push(Node {
                id: id.clone(),
                label: label.into(),
                props: serde_json::json!({"placeholder": true}),
            });
            known.insert(id.clone());
        }
    }
    extraction.nodes.extend(placeholders);
}

/// Recover deterministic facts from one Java source file.
pub fn extract_source(
    source: &[u8],
    path: &str,
    id: &SourceId<'_>,
) -> Result<Extraction, ExtractError> {
    let language: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language)?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| ExtractError::NoTree(path.into()))?;
    let root = tree.root_node();
    let cx = FileCx { source, path, id };
    let mut out = Extraction::default();
    out.nodes.push(Node {
        id: file_id(id.repo, path),
        label: "File".into(),
        props: serde_json::json!({
            "path": path,
            "language": "java",
            "prov": cx.prov(&root, &format!("File {path}")),
        }),
    });

    let package = {
        let query = Query::new(
            &language,
            "(package_declaration [(scoped_identifier) (identifier)] @package)",
        )
        .expect("static query");
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, root, source);
        let mut package = None;
        if let Some(found) = matches.next() {
            package = Some(cx.text(&found.captures[0].node).to_string());
        }
        package
    };
    let imports = parse_imports(&cx, root, &language, &mut out);

    // Types: classes, interfaces, enums, records — nested chains included.
    let type_query = {
        let clauses = TYPE_KINDS
            .iter()
            .map(|kind| format!("({kind} name: (identifier) @name) @decl"))
            .collect::<Vec<_>>()
            .join(" ");
        Query::new(&language, &format!("[{clauses}]")).expect("static query")
    };
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&type_query, root, source);
    while let Some(found) = matches.next() {
        let (mut decl, mut name) = (None, None);
        for capture in found.captures {
            match type_query.capture_names()[capture.index as usize] {
                "decl" => decl = Some(capture.node),
                "name" => name = Some(cx.text(&capture.node).to_string()),
                _ => {}
            }
        }
        let (Some(decl), Some(name)) = (decl, name) else {
            continue;
        };
        let mut chain = enclosing_type_chain(&cx, decl);
        chain.push(name.clone());
        let qualified = chain.join(".");
        let symbol = symbol_id(id.repo, path, &qualified);
        let kind = decl.kind().trim_end_matches("_declaration");
        out.nodes.push(Node {
            id: symbol.clone(),
            label: "Symbol".into(),
            props: serde_json::json!({
                "name": qualified,
                "kind": kind,
                "language": "java",
                "prov": cx.prov(&decl, &format!("Symbol {symbol}")),
            }),
        });
        out.edges.push(Edge {
            src: symbol.clone(),
            dst: file_id(id.repo, path),
            label: "DEFINED_IN".into(),
            props: serde_json::json!({
                "prov": cx.prov(&decl, &format!("DEFINED_IN {symbol}")),
            }),
        });
        if let Some(package) = &package {
            out.declared_types.push(DeclaredType {
                fqn: format!("{package}.{qualified}"),
                path: path.to_string(),
                qualified,
            });
        }
    }

    // Methods and constructors, qualified by their enclosing type chain.
    let method_query = Query::new(
        &language,
        "[(method_declaration name: (identifier) @name) @method
          (constructor_declaration name: (identifier) @name) @method]",
    )
    .expect("static query");
    let mut methods_by_start: HashMap<usize, String> = HashMap::new();
    let mut local_methods: HashSet<String> = HashSet::new();
    let mut methods = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&method_query, root, source);
    while let Some(found) = matches.next() {
        let (mut method, mut name) = (None, None);
        for capture in found.captures {
            match method_query.capture_names()[capture.index as usize] {
                "method" => method = Some(capture.node),
                "name" => name = Some(cx.text(&capture.node).to_string()),
                _ => {}
            }
        }
        let (Some(method), Some(name)) = (method, name) else {
            continue;
        };
        let chain = enclosing_type_chain(&cx, method);
        if chain.is_empty() {
            continue;
        }
        let qualified = format!("{}.{name}", chain.join("."));
        let symbol = symbol_id(id.repo, path, &qualified);
        methods_by_start.insert(method.start_byte(), symbol.clone());
        local_methods.insert(qualified.clone());
        out.nodes.push(Node {
            id: symbol.clone(),
            label: "Symbol".into(),
            props: serde_json::json!({
                "name": qualified,
                "kind": "method",
                "language": "java",
                "prov": cx.prov(&method, &format!("Symbol {symbol}")),
            }),
        });
        out.edges.push(Edge {
            src: symbol.clone(),
            dst: file_id(id.repo, path),
            label: "DEFINED_IN".into(),
            props: serde_json::json!({
                "prov": cx.prov(&method, &format!("DEFINED_IN {symbol}")),
            }),
        });
        methods.push((method, symbol, chain, name));
    }

    // Spring Web endpoints: annotation-proven controllers, class-level
    // @RequestMapping base path, method-level @{Get,Post,...}Mapping.
    for (method, handler, chain, _) in &methods {
        if method.kind() != "method_declaration" {
            continue;
        }
        let Some(class_decl) = ({
            let mut node = *method;
            let mut found = None;
            while let Some(parent) = node.parent() {
                if parent.kind() == "class_declaration" {
                    found = Some(parent);
                    break;
                }
                node = parent;
            }
            found
        }) else {
            continue;
        };
        let class_annotations = declaration_annotations(&cx, class_decl);
        let is_controller = class_annotations.iter().any(|(name, _)| {
            matches!(name.as_str(), "RestController" | "Controller")
                && spring_proven(name, &imports)
        });
        if !is_controller {
            continue;
        }
        let base = class_annotations
            .iter()
            .find(|(name, _)| name == "RequestMapping" && spring_proven(name, &imports))
            .and_then(|(_, node)| annotation_literal_path(&cx, *node))
            .unwrap_or_default();
        for (name, annotation) in declaration_annotations(&cx, *method) {
            let Some(http_method) = mapping_method(&name) else {
                continue;
            };
            if !spring_proven(&name, &imports) {
                continue;
            }
            let tail = annotation_literal_path(&cx, annotation).unwrap_or_default();
            let route = join_route(&base, &tail);
            let endpoint = format!("ep:{}@{http_method}:{route}", id.repo);
            out.nodes.push(Node {
                id: endpoint.clone(),
                label: "Endpoint".into(),
                props: serde_json::json!({
                    "method": http_method,
                    "path": route,
                    "handler_sym": handler,
                    "framework": "spring",
                    "language": "java",
                    "prov": cx.prov(&annotation, &format!("Endpoint {endpoint}")),
                }),
            });
            out.edges.push(Edge {
                src: endpoint.clone(),
                dst: handler.clone(),
                label: "HANDLES".into(),
                props: serde_json::json!({
                    "prov": cx.prov(&annotation, &format!("HANDLES {endpoint} -> {handler}")),
                }),
            });
        }
        let _ = chain;
    }

    // Calls: same-class unqualified/this calls resolve locally; calls on an
    // imported type resolve repo-wide at the directory join, failing closed
    // to an explicit Gap when the project-local target cannot be proven.
    let call_query = Query::new(
        &language,
        "(method_invocation name: (identifier) @name) @call",
    )
    .expect("static query");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&call_query, root, source);
    while let Some(found) = matches.next() {
        let (mut call, mut name) = (None, None);
        for capture in found.captures {
            match call_query.capture_names()[capture.index as usize] {
                "call" => call = Some(capture.node),
                "name" => name = Some(cx.text(&capture.node).to_string()),
                _ => {}
            }
        }
        let (Some(call), Some(name)) = (call, name) else {
            continue;
        };
        let Some(src) = ({
            let mut node = call;
            let mut found = None;
            while let Some(parent) = node.parent() {
                if let Some(symbol) = methods_by_start.get(&parent.start_byte())
                    && matches!(
                        parent.kind(),
                        "method_declaration" | "constructor_declaration"
                    )
                {
                    found = Some(symbol.clone());
                    break;
                }
                node = parent;
            }
            found
        }) else {
            continue;
        };
        let object = call
            .child_by_field_name("object")
            .map(|object| cx.text(&object).to_string());
        match object.as_deref() {
            None | Some("this") => {
                let chain = enclosing_type_chain(&cx, call);
                if chain.is_empty() {
                    continue;
                }
                let qualified = format!("{}.{name}", chain.join("."));
                if !local_methods.contains(&qualified) {
                    continue;
                }
                let dst = symbol_id(id.repo, path, &qualified);
                if dst != src {
                    out.edges.push(Edge {
                        src,
                        dst,
                        label: "CALLS".into(),
                        props: serde_json::json!({
                            "prov": cx.prov(&call, &format!("CALLS {qualified}")),
                        }),
                    });
                }
            }
            Some(object_text) => {
                let Some(fqn) = imports.bindings.get(object_text) else {
                    continue;
                };
                let callee = format!("{object_text}.{name}");
                let gap_id = format!("gap:call:{}@{}@{}", id.repo, path, call.start_byte());
                let gap_node = Node {
                    id: gap_id.clone(),
                    label: "Gap".into(),
                    props: serde_json::json!({
                        "callee": callee,
                        "reason": "unresolved Java import target",
                        "attempted_tiers": ["T0"],
                        "prov": cx.prov_with_confidence(
                            &call,
                            ConfidenceTier::Gap,
                            &format!("Gap {gap_id}"),
                        ),
                    }),
                };
                let gap_edge = Edge {
                    src: src.clone(),
                    dst: gap_id.clone(),
                    label: "CALLS".into(),
                    props: serde_json::json!({
                        "attempted_resolution": "import-fqn",
                        "prov": cx.prov_with_confidence(
                            &call,
                            ConfidenceTier::Gap,
                            &format!("CALLS -> {gap_id}"),
                        ),
                    }),
                };
                out.pending_calls.push(PendingCall {
                    src,
                    fqn: fqn.clone(),
                    method: name,
                    resolved_props: serde_json::json!({
                        "resolution": "import-proven",
                        "prov": cx.prov(&call, &format!("CALLS {callee}")),
                    }),
                    gap: (gap_node, gap_edge),
                });
            }
        }
    }

    Ok(out)
}

fn collect_java_files(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), ExtractError> {
    let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name.starts_with('.')
                || matches!(
                    name.as_ref(),
                    "target" | "build" | "out" | "node_modules" | "dist" | "generated"
                )
            {
                continue;
            }
            collect_java_files(root, &path, out)?;
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("java") {
            let relative = path.strip_prefix(root).expect("walk stays beneath root");
            out.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// Recover a Java directory with content-addressed per-file parse reuse.
pub fn extract_dir_incremental(
    root: &Path,
    id: &SourceId<'_>,
    cache: &mut IncrementalCache,
) -> Result<(Extraction, IncrementalStats), ExtractError> {
    extract_dir_incremental_with_progress(root, id, cache, &mut |_| {})
}

/// Same as [`extract_dir_incremental`], calling `on_file` with each file's
/// repo-relative path as it's read (#209 live progress hook).
pub fn extract_dir_incremental_with_progress(
    root: &Path,
    id: &SourceId<'_>,
    cache: &mut IncrementalCache,
    on_file: &mut dyn FnMut(&str),
) -> Result<(Extraction, IncrementalStats), ExtractError> {
    let mut files = Vec::new();
    collect_java_files(root, root, &mut files)?;
    files.sort();
    let active = files.iter().cloned().collect::<BTreeSet<_>>();
    let mut stats = IncrementalStats {
        deleted_files: cache
            .files
            .keys()
            .filter(|path| !active.contains(*path))
            .count() as u64,
        ..IncrementalStats::default()
    };
    cache.files.retain(|path, _| active.contains(path));
    let mut out = Extraction::default();
    for path in files {
        on_file(&path);
        let source = std::fs::read(root.join(&path))?;
        let source_hash = core_prov::content_hash(&source);
        let extraction = if let Some(cached) = cache
            .files
            .get(&path)
            .filter(|cached| cached.source_hash == source_hash)
        {
            stats.reused_files += 1;
            let mut extraction = cached.extraction.clone();
            retarget_commit(&mut extraction, id.commit);
            extraction
        } else {
            stats.recomputed_files += 1;
            extract_source(&source, &path, id)?
        };
        cache.files.insert(
            path,
            CachedFile {
                source_hash,
                extraction: extraction.clone(),
            },
        );
        out.nodes.extend(extraction.nodes);
        out.edges.extend(extraction.edges);
        out.pending_calls.extend(extraction.pending_calls);
        out.declared_types.extend(extraction.declared_types);
    }

    // Directory join: an imported FQN resolves only to a type this repo
    // declares exactly once — a duplicate FQN (the same class in two source
    // roots or modules) is ambiguous and fails closed to a Gap instead of
    // silently picking whichever file sorts last (#170 review). A
    // declared-package import that cannot be proven is an explicit Gap; a
    // foreign package is outside T0 scope and asserts nothing.
    let mut types_by_fqn: BTreeMap<&str, Option<&DeclaredType>> = BTreeMap::new();
    for declared in &out.declared_types {
        types_by_fqn
            .entry(declared.fqn.as_str())
            .and_modify(|unique| *unique = None)
            .or_insert(Some(declared));
    }
    let repo_packages: BTreeSet<&str> = out
        .declared_types
        .iter()
        .filter_map(|declared| declared.fqn.rsplit_once('.').map(|(package, _)| package))
        .collect();
    let known = out
        .nodes
        .iter()
        .filter(|node| node.label == "Symbol")
        .map(|node| node.id.clone())
        .collect::<HashSet<_>>();
    for pending in std::mem::take(&mut out.pending_calls) {
        let resolved = types_by_fqn
            .get(pending.fqn.as_str())
            .copied()
            .flatten()
            .map(|declared| {
                symbol_id(
                    id.repo,
                    &declared.path,
                    &format!("{}.{}", declared.qualified, pending.method),
                )
            });
        match resolved {
            Some(dst) if known.contains(&dst) => out.edges.push(Edge {
                src: pending.src,
                dst,
                label: "CALLS".into(),
                props: pending.resolved_props,
            }),
            _ => {
                let package = pending.fqn.rsplit_once('.').map(|(package, _)| package);
                if package.is_some_and(|package| repo_packages.contains(package)) {
                    let (node, edge) = pending.gap;
                    out.nodes.push(node);
                    out.edges.push(edge);
                }
            }
        }
    }
    close_over_placeholders(&mut out);
    Ok((out, stats))
}

/// Recover a Java directory without retaining an incremental cache.
pub fn extract_dir(root: &Path, id: &SourceId<'_>) -> Result<Extraction, ExtractError> {
    extract_dir_incremental(root, id, &mut IncrementalCache::default())
        .map(|(extraction, _)| extraction)
}

#[cfg(test)]
mod tests;
