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

pub mod jvm;

use jvm::{classify_import, foreign_packages, in_system};

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
    /// Package declarations, one per file that has one.
    packages: Vec<String>,
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
        self.prov_spanning(&[*node], confidence, fact)
    }

    /// Provenance citing every node that establishes the fact — e.g. a route
    /// Gap cites both the unprovable class base and the method mapping.
    fn prov_spanning(
        &self,
        nodes: &[TsNode<'_>],
        confidence: ConfidenceTier,
        fact: &str,
    ) -> serde_json::Value {
        let evidence = nodes
            .iter()
            .map(|node| EvidenceRef {
                repo: self.id.repo.into(),
                path: self.path.into(),
                byte_start: node.start_byte() as u64,
                byte_end: node.end_byte() as u64,
                commit_sha: self.id.commit.into(),
            })
            .collect();
        let provenance = Provenance::new(
            Tier::Deterministic,
            confidence,
            evidence,
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
        let statement = found.captures()[0].node;
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

/// A mapping annotation's path argument, three-way — the Kotlin adapter's
/// model (#234). "No argument" and "argument present but unprovable" are
/// different facts: an absent path is Spring's documented default (`""`),
/// while a present-but-dynamic one (a constant, a concatenation, any
/// non-literal expression) is a runtime identity T0 cannot confirm — it must
/// fail closed, never collapse to the default.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PathArg {
    /// No path-designating argument: Spring defaults the path to `""`.
    Absent,
    /// One or more provable string literals; Spring maps each of them.
    Literal(Vec<String>),
    /// A path argument exists but is not a provable literal.
    Dynamic,
}

impl PathArg {
    /// The path segments this argument designates, or `None` when unprovable.
    fn segments(self) -> Option<Vec<String>> {
        match self {
            PathArg::Absent => Some(vec![String::new()]),
            PathArg::Literal(paths) => Some(paths),
            PathArg::Dynamic => None,
        }
    }
}

/// A single-line string literal whose source spelling is its runtime value.
/// Escapes (`-`, `\t`, …) are not decoded: the source bytes would not
/// be the route, so such literals fail closed rather than confirm a spelling
/// Spring never maps. Text blocks are rejected for the same reason — the
/// compiler re-indents their content.
fn string_literal(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    if node.kind() != "string_literal" {
        return None;
    }
    let mut walk = node.walk();
    if node
        .named_children(&mut walk)
        .any(|child| child.kind() != "string_fragment")
    {
        return None;
    }
    let text = cx.text(&node).trim_matches('"');
    // Unicode escapes are resolved before lexing, so the grammar may not
    // surface them as `escape_sequence` nodes; a backslash in the source
    // spelling is never a literal route character.
    (!text.contains('\\')).then(|| text.to_string())
}

fn is_comment(node: TsNode<'_>) -> bool {
    matches!(node.kind(), "line_comment" | "block_comment")
}

/// A path element value: one literal, or an array initializer whose every
/// element is a literal (`{ "/a", "/b" }`; an empty `{}` is the default path).
fn path_value(cx: &FileCx<'_>, value: TsNode<'_>) -> PathArg {
    if value.kind() != "element_value_array_initializer" {
        return match string_literal(cx, value) {
            Some(path) => PathArg::Literal(vec![path]),
            None => PathArg::Dynamic,
        };
    }
    let mut walk = value.walk();
    let paths: Option<Vec<String>> = value
        .named_children(&mut walk)
        .filter(|element| !is_comment(*element))
        .map(|element| string_literal(cx, element))
        .collect();
    match paths {
        Some(paths) if paths.is_empty() => PathArg::Absent,
        Some(paths) => PathArg::Literal(paths),
        None => PathArg::Dynamic,
    }
}

/// The path argument of a mapping annotation: a bare (positional) value,
/// which Java binds to `value`, or a `value =` / `path =` pair. Other named
/// elements (`produces = …`) do not designate a path.
fn annotation_path(cx: &FileCx<'_>, annotation: TsNode<'_>) -> PathArg {
    let Some(arguments) = annotation.child_by_field_name("arguments") else {
        return PathArg::Absent;
    };
    let mut walk = arguments.walk();
    for argument in arguments.named_children(&mut walk) {
        if is_comment(argument) {
            continue;
        }
        if argument.kind() != "element_value_pair" {
            return path_value(cx, argument);
        }
        let (Some(key), Some(value)) = (
            argument.child_by_field_name("key"),
            argument.child_by_field_name("value"),
        ) else {
            continue;
        };
        if matches!(cx.text(&key), "value" | "path") {
            return path_value(cx, value);
        }
    }
    PathArg::Absent
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

/// Compose a class-level base with a method-level path (AC-0206, #445).
/// Segments meet at exactly one `/`, but a trailing slash spelled in the
/// source is kept (as one `/`): Spring 6 matches `/a` and `/a/` as distinct
/// routes, so `"/api/"` + `""` is `/api/` and `"/api"` + `"/"` is `/api/`.
fn join_route(base: &str, tail: &str) -> String {
    let route = if tail.is_empty() {
        base.to_string()
    } else {
        format!(
            "{}/{}",
            base.trim_end_matches('/'),
            tail.trim_start_matches('/')
        )
    };
    if route.ends_with('/') || route.is_empty() {
        format!("{}/", route.trim_end_matches('/'))
    } else {
        route
    }
}

/// Retarget each `IMPORTS` edge whose complete target this repository
/// declares exactly once to the declaring `File` — the same shape as a
/// resolved TS relative import. The target is proven when it is a declared
/// type (nested types included), or a member whose `Symbol` the declaring
/// type defines. A prefix alone never proves it (`a.Foo.Missing` is not
/// `a.Foo`): unproven targets keep their `mod:` id and are classified by
/// [`classify_import`].
fn resolve_repo_imports(
    edges: &mut [Edge],
    repo: &str,
    types_by_fqn: &BTreeMap<&str, Option<&DeclaredType>>,
    known_symbols: &HashSet<String>,
) {
    for edge in edges {
        if edge.label != "IMPORTS" {
            continue;
        }
        let Some(module) = edge.dst.strip_prefix("mod:") else {
            continue;
        };
        let declared = types_by_fqn.get(module).copied().flatten().or_else(|| {
            let (owner, member) = module.rsplit_once('.')?;
            let owner = types_by_fqn.get(owner).copied().flatten()?;
            known_symbols
                .contains(&symbol_id(
                    repo,
                    &owner.path,
                    &format!("{}.{member}", owner.qualified),
                ))
                .then_some(owner)
        });
        if let Some(declared) = declared {
            edge.dst = file_id(repo, &declared.path);
            edge.props["resolution"] = "import-proven".into();
        }
    }
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
            package = Some(cx.text(&found.captures()[0].node).to_string());
        }
        package
    };
    out.packages.extend(package.clone());
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
        for capture in found.captures() {
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
        for capture in found.captures() {
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
        // "No @RequestMapping" and "@RequestMapping with no path argument"
        // both mean Spring's default base (""); a present-but-dynamic base
        // poisons every mapping under it (None), failing them closed.
        let base_mapping = class_annotations
            .iter()
            .find(|(name, _)| name == "RequestMapping" && spring_proven(name, &imports))
            .map(|(_, node)| *node);
        let bases = match base_mapping {
            None => Some(vec![String::new()]),
            Some(node) => annotation_path(&cx, node).segments(),
        };
        for (name, annotation) in declaration_annotations(&cx, *method) {
            let Some(http_method) = mapping_method(&name) else {
                continue;
            };
            if !spring_proven(&name, &imports) {
                continue;
            }
            let tails = annotation_path(&cx, annotation).segments();
            let (Some(bases), Some(tails)) = (bases.as_ref(), tails) else {
                // The mapping is proven but its route is a runtime identity
                // (constant/expression path, on the method or the class
                // base). T0 cannot confirm the route, so the endpoint is an
                // explicit Gap, never a default path presented as Confirmed
                // (R-INT-4).
                let gap_id = format!("gap:route:{}@{}@{}", id.repo, path, annotation.start_byte());
                // Cite what made the route unprovable: the class base when it
                // is dynamic, and always the method mapping that binds the
                // handler, so the evidence shows the actual expression.
                let evidence: Vec<TsNode<'_>> = match (bases.is_none(), base_mapping) {
                    (true, Some(base)) => vec![base, annotation],
                    _ => vec![annotation],
                };
                out.nodes.push(Node {
                    id: gap_id.clone(),
                    label: "Gap".into(),
                    props: serde_json::json!({
                        "method": http_method,
                        "handler_sym": handler,
                        "framework": "spring",
                        "language": "java",
                        "reason": "dynamic Spring mapping path",
                        "attempted_tiers": ["T0"],
                        "prov": cx.prov_spanning(
                            &evidence,
                            ConfidenceTier::Gap,
                            &format!("Gap {gap_id}"),
                        ),
                    }),
                });
                out.edges.push(Edge {
                    src: gap_id.clone(),
                    dst: handler.clone(),
                    label: "HANDLES".into(),
                    props: serde_json::json!({
                        "reason": "dynamic Spring mapping path",
                        "attempted_resolution": "literal-path",
                        "prov": cx.prov_spanning(
                            &evidence,
                            ConfidenceTier::Gap,
                            &format!("HANDLES {gap_id} -> {handler}"),
                        ),
                    }),
                });
                continue;
            };
            // Sorted and de-duplicated so emission order never depends on
            // source order (determinism); `{ "/a", "/a/" }` stays two routes.
            let routes: BTreeSet<String> = bases
                .iter()
                .flat_map(|base| tails.iter().map(move |tail| join_route(base, tail)))
                .collect();
            // A composed route depends on the class base as much as on the
            // method mapping, so both are cited (R-INT-1 traceability).
            let evidence: Vec<TsNode<'_>> = base_mapping
                .into_iter()
                .chain(std::iter::once(annotation))
                .collect();
            for route in routes {
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
                        "prov": cx.prov_spanning(
                            &evidence,
                            ConfidenceTier::Confirmed,
                            &format!("Endpoint {endpoint}"),
                        ),
                    }),
                });
                out.edges.push(Edge {
                    src: endpoint.clone(),
                    dst: handler.clone(),
                    label: "HANDLES".into(),
                    props: serde_json::json!({
                        "prov": cx.prov_spanning(
                            &evidence,
                            ConfidenceTier::Confirmed,
                            &format!("HANDLES {endpoint} -> {handler}"),
                        ),
                    }),
                });
            }
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
        for capture in found.captures() {
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
                        "reason": "unresolved Java import target",
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

fn collect_java_files(root: &Path, out: &mut Vec<String>) -> Result<(), ExtractError> {
    let skip = |name: &str| {
        name.starts_with('.')
            || matches!(
                name,
                "target" | "build" | "out" | "node_modules" | "dist" | "generated"
            )
    };
    for file in source_walk::files(root, &skip, source_walk::Gitignores::Honor)? {
        if file
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            == Some("java")
        {
            out.push(file.rel);
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
    collect_java_files(root, &mut files)?;
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
    // Files parse on parallel workers (#236); results merge here in sorted
    // order, so the output is byte-identical to a serial run.
    // Workers see only each cached file's hash; the merge owns the cache and
    // replaces entries one at a time, exactly as the serial loop did, so a
    // re-ingest never holds a second copy of the cache.
    let previous: std::collections::BTreeMap<String, String> = cache
        .files
        .iter()
        .map(|(path, cached)| (path.clone(), cached.source_hash.clone()))
        .collect();
    let merged = source_walk::parallel::map_ordered(
        &files,
        |path| {
            let source = std::fs::read(root.join(path))?;
            let source_hash = core_prov::content_hash(&source);
            let reusable = previous.get(path).is_some_and(|hash| *hash == source_hash);
            let fresh = if reusable {
                None
            } else {
                Some(extract_source(&source, path, id)?)
            };
            Ok::<_, ExtractError>((source_hash, fresh))
        },
        |path, (source_hash, fresh)| {
            on_file(path);
            let extraction = match fresh {
                Some(extraction) => {
                    stats.recomputed_files += 1;
                    extraction
                }
                None => {
                    stats.reused_files += 1;
                    let mut extraction = cache.files[path].extraction.clone();
                    retarget_commit(&mut extraction, id.commit);
                    extraction
                }
            };
            cache.files.insert(
                path.to_string(),
                CachedFile {
                    source_hash,
                    extraction: extraction.clone(),
                },
            );
            out.nodes.extend(extraction.nodes);
            out.edges.extend(extraction.edges);
            out.pending_calls.extend(extraction.pending_calls);
            out.declared_types.extend(extraction.declared_types);
            out.packages.extend(extraction.packages);
            Ok(())
        },
    );
    merged?;

    // Directory join: an imported FQN resolves only to a type this repo
    // declares exactly once — a duplicate FQN (the same class in two source
    // roots or modules) is ambiguous and fails closed to a Gap instead of
    // silently picking whichever file sorts last (#170 review). A call on a
    // declared-package import that cannot be proven is an explicit Gap; a
    // call on a foreign package asserts nothing, while the import itself
    // closes over a proven external boundary (#237, `jvm::classify_import`).
    let mut types_by_fqn: BTreeMap<&str, Option<&DeclaredType>> = BTreeMap::new();
    for declared in &out.declared_types {
        types_by_fqn
            .entry(declared.fqn.as_str())
            .and_modify(|unique| *unique = None)
            .or_insert(Some(declared));
    }
    // Every package this repository declares — Kotlin sources included, so
    // a mixed JVM tree never mistakes its own Kotlin package for external.
    let mut repo_packages: BTreeSet<String> = out.packages.iter().cloned().collect();
    let foreign = foreign_packages(root, &["kt", "kts"])?;
    repo_packages.extend(foreign.packages);
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
                if in_system(&pending.fqn, &repo_packages) {
                    let (node, edge) = pending.gap;
                    out.nodes.push(node);
                    out.edges.push(edge);
                }
            }
        }
    }
    resolve_repo_imports(&mut out.edges, id.repo, &types_by_fqn, &known);
    let Extraction { nodes, edges, .. } = &mut out;
    core_graph::placeholder::close_over_endpoints(
        nodes,
        edges,
        EXTRACTOR_ID,
        core_graph::placeholder::label_for_id,
        |endpoint, _| {
            let module = endpoint.strip_prefix("mod:")?;
            Some(classify_import(module, &repo_packages, foreign.complete))
        },
    );
    Ok((out, stats))
}

/// Recover a Java directory without retaining an incremental cache.
pub fn extract_dir(root: &Path, id: &SourceId<'_>) -> Result<Extraction, ExtractError> {
    extract_dir_incremental(root, id, &mut IncrementalCache::default())
        .map(|(extraction, _)| extraction)
}

#[cfg(test)]
mod tests;
