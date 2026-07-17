//! Kotlin deterministic (T0) language adapter (SPEC-00 §3.3, #212).
//!
//! Classes, interfaces, objects, data classes, enums, and functions
//! (top-level, member, and extension) become Symbols; imports and
//! import-proven cross-file calls become the server graph, with unresolved
//! project-local targets failing closed to explicit Gaps. Annotation-proven
//! Spring Web mappings become Endpoint/HANDLES facts with class+method path
//! composition (Ktor's routing DSL is a follow-on, not v1). Adapters are per
//! language, not per compiler version: the grammar parses current syntax and
//! anything it cannot prove is simply not asserted. This tier never calls an
//! LLM and every emitted fact carries exact source-span provenance.

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node as TsNode, Parser, Query, QueryCursor};

const EXTRACTOR_ID: &str = "t0.adapter-kotlin";

/// Kotlin extraction errors.
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

/// Kotlin graph facts from one file or directory.
#[derive(Debug, Clone, Default)]
pub struct Extraction {
    /// Recovered nodes.
    pub nodes: Vec<Node>,
    /// Recovered edges.
    pub edges: Vec<Edge>,
    pending_calls: Vec<PendingCall>,
    declared_types: Vec<Declared>,
    declared_functions: Vec<Declared>,
}

/// A call proven by an import, resolvable only with the whole directory in
/// view (the import names a FQN; only the repo-wide index knows which file
/// declares it). `member` is `Some` for a call on an imported type/object
/// and `None` for a call to an imported top-level function.
#[derive(Debug, Clone)]
struct PendingCall {
    src: String,
    fqn: String,
    member: Option<String>,
    resolved_props: serde_json::Value,
    gap: (Node, Edge),
}

/// A declaration (type/object or top-level function) and the
/// fully-qualified name it answers to.
#[derive(Debug, Clone)]
struct Declared {
    fqn: String,
    path: String,
    qualified: String,
}

#[derive(Debug, Clone)]
struct CachedFile {
    source_hash: String,
    extraction: Extraction,
}

/// Reusable per-file Kotlin parse cache.
#[derive(Debug, Default)]
pub struct IncrementalCache {
    files: BTreeMap<String, CachedFile>,
}

/// Physical Kotlin source work performed by an incremental extraction.
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

/// Type-declaration kinds that contribute to a member's qualified name.
/// `companion_object` is deliberately transparent: `companion object { fun
/// make() }` inside `Outer` is called as `Outer.make()`, so the companion
/// contributes nothing to the chain.
const TYPE_KINDS: &[&str] = &["class_declaration", "object_declaration"];

/// The dot-joined chain of enclosing type names (outermost first) for a
/// node, e.g. `Outer.Inner` for a function inside a nested class.
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

/// One file's import surface: named bindings (simple name or alias → FQN)
/// plus wildcard-imported packages.
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
    let query = Query::new(language, "(import) @import").expect("static query");
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
        let (rest, alias) = match rest.rsplit_once(" as ") {
            Some((module, alias)) => (module.trim(), Some(alias.trim().to_string())),
            None => (rest, None),
        };
        let (module, wildcard) = match rest.strip_suffix(".*") {
            Some(package) => (package.to_string(), true),
            None => (rest.to_string(), false),
        };
        if module.is_empty() {
            continue;
        }
        if wildcard {
            imports.wildcard_packages.push(module.clone());
        } else {
            let simple = alias.unwrap_or_else(|| {
                module
                    .rsplit_once('.')
                    .map(|(_, simple)| simple.to_string())
                    .unwrap_or_else(|| module.clone())
            });
            imports.bindings.insert(simple, module.clone());
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

/// A mapping annotation's path argument, three-way. "No argument" and
/// "argument present but unprovable" are different facts: an absent path
/// is Spring's own documented default (`""`), while a present-but-dynamic
/// one (a `$` template, a constant reference, any non-literal expression)
/// is a runtime identity T0 cannot confirm — it must fail closed, never
/// collapse to the default.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PathArg {
    /// No path-designating argument: Spring defaults the path to `""`.
    Absent,
    /// A provable string literal.
    Literal(String),
    /// A path argument exists but is not a provable literal.
    Dynamic,
}

/// One recognized annotation use: simple name, the node carrying its
/// evidence span, and its path argument.
struct AnnotationUse<'t> {
    simple: String,
    node: TsNode<'t>,
    path: PathArg,
}

/// A string literal with no interpolation — `"/api/users"` yes,
/// `"/api/$version"` no. Templates are runtime identities, not asserted.
/// The grammar only materializes an `interpolation` node for `${expr}`;
/// a bare `$name` template is lexed as plain `string_content` pieces, so
/// any `$` in the text also fails closed.
fn pure_string_literal(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    if node.kind() != "string_literal" {
        return None;
    }
    let mut walk = node.walk();
    if node
        .named_children(&mut walk)
        .any(|child| child.kind() != "string_content")
    {
        return None;
    }
    let text = cx.text(&node).trim_matches('"').to_string();
    if text.contains('$') {
        return None;
    }
    Some(text)
}

/// The path argument of a mapping annotation's `value_arguments`: a bare
/// (positional) argument, or a `value =` / `path =` pair. A found argument
/// that is not a pure string literal is [`PathArg::Dynamic`] — present but
/// unprovable; other named arguments (`produces = …`, `method = …`) do not
/// designate a path and leave it [`PathArg::Absent`].
fn arguments_path(cx: &FileCx<'_>, arguments: TsNode<'_>) -> PathArg {
    let mut walk = arguments.walk();
    for argument in arguments.named_children(&mut walk) {
        if argument.kind() != "value_argument" {
            continue;
        }
        let children: Vec<TsNode<'_>> = {
            let mut inner = argument.walk();
            argument.named_children(&mut inner).collect()
        };
        match children.as_slice() {
            [value] => {
                return match pure_string_literal(cx, *value) {
                    Some(literal) => PathArg::Literal(literal),
                    None => PathArg::Dynamic,
                };
            }
            [key, value] if key.kind() == "identifier" => {
                if matches!(cx.text(key), "value" | "path") {
                    return match pure_string_literal(cx, *value) {
                        Some(literal) => PathArg::Literal(literal),
                        None => PathArg::Dynamic,
                    };
                }
            }
            _ => {}
        }
    }
    PathArg::Absent
}

/// Interpret one grammar `annotation` node: the simple name comes from the
/// last segment of its `user_type`, the path argument from its
/// `constructor_invocation` arguments when present.
fn read_annotation<'t>(cx: &FileCx<'_>, annotation: TsNode<'t>) -> Option<AnnotationUse<'t>> {
    let mut walk = annotation.walk();
    for child in annotation.named_children(&mut walk) {
        match child.kind() {
            "user_type" => {
                let simple = cx.text(&child);
                let simple = simple.rsplit('.').next().unwrap_or(simple).trim();
                return Some(AnnotationUse {
                    simple: simple.to_string(),
                    node: annotation,
                    path: PathArg::Absent,
                });
            }
            "constructor_invocation" => {
                let mut inner = child.walk();
                let mut simple = None;
                let mut path = PathArg::Absent;
                for part in child.named_children(&mut inner) {
                    match part.kind() {
                        "user_type" => {
                            let name = cx.text(&part);
                            simple =
                                Some(name.rsplit('.').next().unwrap_or(name).trim().to_string());
                        }
                        "value_arguments" => path = arguments_path(cx, part),
                        _ => {}
                    }
                }
                return simple.map(|simple| AnnotationUse {
                    simple,
                    node: annotation,
                    path,
                });
            }
            _ => {}
        }
    }
    None
}

/// Annotations attached to a declaration via its `modifiers` child.
fn declaration_annotations<'t>(cx: &FileCx<'_>, declaration: TsNode<'t>) -> Vec<AnnotationUse<'t>> {
    let mut found = Vec::new();
    let mut walk = declaration.walk();
    for child in declaration.named_children(&mut walk) {
        if child.kind() != "modifiers" {
            continue;
        }
        let mut inner = child.walk();
        for annotation in child.named_children(&mut inner) {
            if annotation.kind() != "annotation" {
                continue;
            }
            if let Some(read) = read_annotation(cx, annotation) {
                found.push(read);
            }
        }
    }
    found
}

/// Grammar-quirk recovery (verify-at-build, #212): depending on what
/// follows the class in the same file, `tree-sitter-kotlin-ng` sometimes
/// resolves class-level annotations into a *preceding sibling*
/// `annotated_expression` statement — `@RequestMapping("/x")` becomes an
/// annotation wrapping a parenthesized string expression — instead of the
/// class's own `modifiers`. The chain shape is unambiguous, so recover it:
/// each nesting level contributes one annotation, and a terminal
/// `(parenthesized_expression (string_literal))` is the innermost
/// annotation's path argument. Proof standards are unchanged — the
/// recovered names still need the Spring import to assert anything.
fn preceding_expression_annotations<'t>(
    cx: &FileCx<'_>,
    declaration: TsNode<'t>,
) -> Vec<AnnotationUse<'t>> {
    let mut found = Vec::new();
    let mut sibling = declaration.prev_named_sibling();
    while let Some(candidate) = sibling {
        if candidate.kind() != "annotated_expression" {
            break;
        }
        let mut level = candidate;
        loop {
            let mut next_level = None;
            let mut walk = level.walk();
            for child in level.named_children(&mut walk) {
                match child.kind() {
                    "annotation" => {
                        if let Some(read) = read_annotation(cx, child) {
                            found.push(read);
                        }
                    }
                    "annotated_expression" => next_level = Some(child),
                    "parenthesized_expression" => {
                        // The wrapped expression is the innermost
                        // annotation's argument: a pure literal is its
                        // path, anything else is a present-but-dynamic
                        // argument that must not collapse to the default.
                        let literal = {
                            let mut inner = child.walk();
                            child
                                .named_children(&mut inner)
                                .find_map(|expr| pure_string_literal(cx, expr))
                        };
                        if let Some(last) = found.last_mut()
                            && last.path == PathArg::Absent
                        {
                            last.path = match literal {
                                Some(literal) => PathArg::Literal(literal),
                                None => PathArg::Dynamic,
                            };
                        }
                    }
                    _ => {}
                }
            }
            match next_level {
                Some(next) => level = next,
                None => break,
            }
        }
        sibling = candidate.prev_named_sibling();
    }
    found
}

/// The exact Spring package each recognized annotation lives in. Proof is
/// per annotation, not per vendor: a wildcard of one Spring package must
/// never prove an annotation from another (#170 review, AC-0080 standard).
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

/// The declaration's kind, in Kotlin's own words: `class`, `interface`,
/// `object`, `data class`, `enum class`, `annotation class`.
fn declaration_kind(cx: &FileCx<'_>, declaration: TsNode<'_>) -> &'static str {
    if declaration.kind() == "object_declaration" {
        return "object";
    }
    let mut walk = declaration.walk();
    for child in declaration.children(&mut walk) {
        if child.kind() == "interface" {
            return "interface";
        }
        if child.kind() == "modifiers" {
            let mut inner = child.walk();
            for modifier in child.named_children(&mut inner) {
                if modifier.kind() == "class_modifier" {
                    match cx.text(&modifier) {
                        "data" => return "data class",
                        "enum" => return "enum class",
                        "annotation" => return "annotation class",
                        _ => {}
                    }
                }
            }
        }
    }
    "class"
}

/// An extension function's receiver type name (`String` for
/// `fun String.shout()`): the base identifier of the `user_type` (or the
/// type inside a `nullable_type`) appearing before the function's name.
fn extension_receiver(cx: &FileCx<'_>, function: TsNode<'_>, name: TsNode<'_>) -> Option<String> {
    let mut walk = function.walk();
    for child in function.named_children(&mut walk) {
        if child.start_byte() >= name.start_byte() {
            break;
        }
        let receiver = match child.kind() {
            "user_type" => Some(child),
            "nullable_type" => {
                let mut inner = child.walk();
                child
                    .named_children(&mut inner)
                    .find(|inner_child| inner_child.kind() == "user_type")
            }
            _ => None,
        };
        if let Some(receiver) = receiver {
            let mut inner = receiver.walk();
            let base = receiver
                .named_children(&mut inner)
                .find(|part| part.kind() == "identifier")
                .map(|part| cx.text(&part).to_string());
            return base.or_else(|| Some(cx.text(&receiver).to_string()));
        }
    }
    None
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

/// Recover deterministic facts from one Kotlin source file.
pub fn extract_source(
    source: &[u8],
    path: &str,
    id: &SourceId<'_>,
) -> Result<Extraction, ExtractError> {
    let language: tree_sitter::Language = tree_sitter_kotlin_ng::LANGUAGE.into();
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
            "language": "kotlin",
            "prov": cx.prov(&root, &format!("File {path}")),
        }),
    });

    let package = {
        let query = Query::new(&language, "(package_header) @package").expect("static query");
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, root, source);
        let mut package = None;
        if let Some(found) = matches.next() {
            let raw = cx.text(&found.captures[0].node).replace('\n', " ");
            package = raw
                .trim()
                .strip_prefix("package")
                .map(|rest| rest.trim().trim_end_matches(';').trim().to_string())
                .filter(|name| !name.is_empty());
        }
        package
    };
    let imports = parse_imports(&cx, root, &language, &mut out);

    // Types: classes, interfaces, enums, data classes, objects — nested
    // chains included.
    let type_query = Query::new(
        &language,
        "[(class_declaration name: (identifier) @name) @decl
          (object_declaration name: (identifier) @name) @decl]",
    )
    .expect("static query");
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
        out.nodes.push(Node {
            id: symbol.clone(),
            label: "Symbol".into(),
            props: serde_json::json!({
                "name": qualified,
                "kind": declaration_kind(&cx, decl),
                "language": "kotlin",
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
            out.declared_types.push(Declared {
                fqn: format!("{package}.{qualified}"),
                path: path.to_string(),
                qualified,
            });
        }
    }

    // Functions: top-level, member, and extension, qualified by their
    // enclosing type chain and (for extensions) their receiver type.
    let function_query = Query::new(&language, "(function_declaration name: (identifier) @name)")
        .expect("static query");
    let mut functions_by_start: HashMap<usize, String> = HashMap::new();
    let mut local_functions: HashSet<String> = HashSet::new();
    let mut functions = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&function_query, root, source);
    while let Some(found) = matches.next() {
        let name_node = found.captures[0].node;
        let Some(function) = name_node.parent() else {
            continue;
        };
        let mut name = cx.text(&name_node).to_string();
        if name.is_empty() {
            // Grammar quirk (verify-at-build, #212): a function named by a
            // soft keyword (`fun dynamic()`, legal Kotlin) lexes as that
            // anonymous keyword token followed by an *empty* identifier.
            // Recover the name from the directly adjacent token; a
            // function whose name still cannot be proven is skipped
            // entirely — never an empty-named Symbol.
            name = name_node
                .prev_sibling()
                .filter(|prev| !prev.is_named() && prev.end_byte() == name_node.start_byte())
                .map(|prev| cx.text(&prev).to_string())
                .filter(|text| {
                    !text.is_empty()
                        && text.chars().all(|c| c.is_alphanumeric() || c == '_')
                        && !text.starts_with(|c: char| c.is_ascii_digit())
                })
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
        }
        let chain = enclosing_type_chain(&cx, function);
        let receiver = extension_receiver(&cx, function, name_node);
        let local_name = match &receiver {
            Some(receiver) => format!("{receiver}.{name}"),
            None => name.clone(),
        };
        let qualified = if chain.is_empty() {
            local_name.clone()
        } else {
            format!("{}.{local_name}", chain.join("."))
        };
        let symbol = symbol_id(id.repo, path, &qualified);
        functions_by_start.insert(function.start_byte(), symbol.clone());
        local_functions.insert(qualified.clone());
        let mut props = serde_json::json!({
            "name": qualified,
            "kind": "function",
            "language": "kotlin",
            "prov": cx.prov(&function, &format!("Symbol {symbol}")),
        });
        if let Some(receiver) = &receiver {
            props["receiver"] = serde_json::json!(receiver);
        }
        out.nodes.push(Node {
            id: symbol.clone(),
            label: "Symbol".into(),
            props,
        });
        out.edges.push(Edge {
            src: symbol.clone(),
            dst: file_id(id.repo, path),
            label: "DEFINED_IN".into(),
            props: serde_json::json!({
                "prov": cx.prov(&function, &format!("DEFINED_IN {symbol}")),
            }),
        });
        if chain.is_empty()
            && receiver.is_none()
            && let Some(package) = &package
        {
            out.declared_functions.push(Declared {
                fqn: format!("{package}.{name}"),
                path: path.to_string(),
                qualified: name.clone(),
            });
        }
        functions.push((function, symbol));
    }

    // Spring Web endpoints: annotation-proven controllers, class-level
    // @RequestMapping base path, function-level @{Get,Post,...}Mapping.
    for (function, handler) in &functions {
        let Some(class_decl) = ({
            let mut node = *function;
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
        let mut class_annotations = declaration_annotations(&cx, class_decl);
        class_annotations.extend(preceding_expression_annotations(&cx, class_decl));
        let is_controller = class_annotations.iter().any(|annotation| {
            matches!(annotation.simple.as_str(), "RestController" | "Controller")
                && spring_proven(&annotation.simple, &imports)
        });
        if !is_controller {
            continue;
        }
        // "No @RequestMapping" and "@RequestMapping with no path argument"
        // both mean Spring's default base (""); a present-but-dynamic base
        // poisons every mapping under it (None), failing them closed.
        let base = match class_annotations.iter().find(|annotation| {
            annotation.simple == "RequestMapping" && spring_proven(&annotation.simple, &imports)
        }) {
            None => Some(String::new()),
            Some(annotation) => match &annotation.path {
                PathArg::Absent => Some(String::new()),
                PathArg::Literal(path) => Some(path.clone()),
                PathArg::Dynamic => None,
            },
        };
        for annotation in declaration_annotations(&cx, *function) {
            let Some(http_method) = mapping_method(&annotation.simple) else {
                continue;
            };
            if !spring_proven(&annotation.simple, &imports) {
                continue;
            }
            let tail = match &annotation.path {
                PathArg::Absent => Some(String::new()),
                PathArg::Literal(path) => Some(path.clone()),
                PathArg::Dynamic => None,
            };
            let (Some(base), Some(tail)) = (base.clone(), tail) else {
                // The mapping is proven but its route is a runtime
                // identity (template/constant/expression path, on the
                // method or the class base). T0 cannot confirm the route,
                // so the endpoint is an explicit Gap, never a guessed or
                // default path presented as Confirmed (R-INT-4).
                let gap_id = format!(
                    "gap:route:{}@{}@{}",
                    id.repo,
                    path,
                    annotation.node.start_byte()
                );
                out.nodes.push(Node {
                    id: gap_id.clone(),
                    label: "Gap".into(),
                    props: serde_json::json!({
                        "method": http_method,
                        "handler_sym": handler,
                        "framework": "spring",
                        "language": "kotlin",
                        "reason": "dynamic Spring mapping path",
                        "attempted_tiers": ["T0"],
                        "prov": cx.prov_with_confidence(
                            &annotation.node,
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
                        "attempted_resolution": "literal-path",
                        "prov": cx.prov_with_confidence(
                            &annotation.node,
                            ConfidenceTier::Gap,
                            &format!("HANDLES {gap_id} -> {handler}"),
                        ),
                    }),
                });
                continue;
            };
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
                    "language": "kotlin",
                    "prov": cx.prov(&annotation.node, &format!("Endpoint {endpoint}")),
                }),
            });
            out.edges.push(Edge {
                src: endpoint.clone(),
                dst: handler.clone(),
                label: "HANDLES".into(),
                props: serde_json::json!({
                    "prov": cx.prov(
                        &annotation.node,
                        &format!("HANDLES {endpoint} -> {handler}"),
                    ),
                }),
            });
        }
    }

    // Calls: same-scope unqualified/this calls resolve locally; calls
    // proven by an import (a type/object receiver, or an imported top-level
    // function) resolve repo-wide at the directory join, failing closed to
    // an explicit Gap when the project-local target cannot be proven. A
    // receiver that is not an import binding (a local, a property, a call
    // result) is unproven and asserts nothing at T0.
    let call_query = Query::new(&language, "(call_expression) @call").expect("static query");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&call_query, root, source);
    while let Some(found) = matches.next() {
        let call = found.captures[0].node;
        let Some(callee) = call.named_child(0) else {
            continue;
        };
        let Some(src) = ({
            let mut node = call;
            let mut found = None;
            while let Some(parent) = node.parent() {
                if parent.kind() == "function_declaration"
                    && let Some(symbol) = functions_by_start.get(&parent.start_byte())
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
        let (receiver, name) = match callee.kind() {
            "identifier" => (None, cx.text(&callee).to_string()),
            "navigation_expression" => {
                let parts: Vec<TsNode<'_>> = {
                    let mut walk = callee.walk();
                    callee.named_children(&mut walk).collect()
                };
                let [receiver, name] = parts.as_slice() else {
                    continue;
                };
                if name.kind() != "identifier" {
                    continue;
                }
                (Some(*receiver), cx.text(name).to_string())
            }
            _ => continue,
        };
        let receiver_is_this = receiver.is_none_or(|receiver| receiver.kind() == "this_expression");
        if receiver_is_this {
            // Unqualified or `this.` call: resolve within the enclosing
            // type chain, then (for top-level code) the file's top level.
            let chain = enclosing_type_chain(&cx, call);
            let qualified = if chain.is_empty() {
                name.clone()
            } else {
                format!("{}.{name}", chain.join("."))
            };
            let resolved = if local_functions.contains(&qualified) {
                Some(qualified)
            } else if !chain.is_empty() && local_functions.contains(&name) {
                Some(name.clone())
            } else {
                None
            };
            if let Some(qualified) = resolved {
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
                continue;
            }
            if receiver.is_some() {
                continue;
            }
            // An unqualified call to an imported top-level function is
            // import-proven the same way a type receiver is.
            let Some(fqn) = imports.bindings.get(&name) else {
                continue;
            };
            push_pending(&cx, &mut out, src, call, fqn.clone(), None, name);
            continue;
        }
        let receiver = receiver.expect("non-this receiver");
        if receiver.kind() != "identifier" {
            continue;
        }
        let receiver_text = cx.text(&receiver).to_string();
        let Some(fqn) = imports.bindings.get(&receiver_text) else {
            continue;
        };
        let callee_name = format!("{receiver_text}.{name}");
        push_pending(
            &cx,
            &mut out,
            src,
            call,
            fqn.clone(),
            Some(name),
            callee_name,
        );
    }

    Ok(out)
}

/// Queue an import-proven call for the directory join, with its
/// fail-closed Gap alternative already built from this call site's span.
fn push_pending(
    cx: &FileCx<'_>,
    out: &mut Extraction,
    src: String,
    call: TsNode<'_>,
    fqn: String,
    member: Option<String>,
    callee: String,
) {
    let gap_id = format!("gap:call:{}@{}@{}", cx.id.repo, cx.path, call.start_byte());
    let gap_node = Node {
        id: gap_id.clone(),
        label: "Gap".into(),
        props: serde_json::json!({
            "callee": callee,
            "reason": "unresolved Kotlin import target",
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
        fqn,
        member,
        resolved_props: serde_json::json!({
            "resolution": "import-proven",
            "prov": cx.prov(&call, &format!("CALLS {callee}")),
        }),
        gap: (gap_node, gap_edge),
    });
}

fn collect_kotlin_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<String>,
) -> Result<(), ExtractError> {
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
            collect_kotlin_files(root, &path, out)?;
        } else if matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("kt" | "kts")
        ) {
            let relative = path.strip_prefix(root).expect("walk stays beneath root");
            out.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// Recover a Kotlin directory with content-addressed per-file parse reuse.
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
    collect_kotlin_files(root, root, &mut files)?;
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
        out.declared_functions.extend(extraction.declared_functions);
    }

    // Directory join: an imported FQN resolves only to a declaration this
    // repo makes exactly once — a duplicate FQN (the same class in two
    // source roots or modules) is ambiguous and fails closed to a Gap
    // instead of silently picking whichever file sorts last (the #170
    // standard). A declared-package import that cannot be proven is an
    // explicit Gap; a foreign package is outside T0 scope and asserts
    // nothing.
    let unique_by_fqn = |declared: &[Declared]| {
        let mut by_fqn: BTreeMap<String, Option<Declared>> = BTreeMap::new();
        for item in declared {
            by_fqn
                .entry(item.fqn.clone())
                .and_modify(|unique| *unique = None)
                .or_insert(Some(item.clone()));
        }
        by_fqn
    };
    let types_by_fqn = unique_by_fqn(&out.declared_types);
    let functions_by_fqn = unique_by_fqn(&out.declared_functions);
    let repo_packages: BTreeSet<String> = out
        .declared_types
        .iter()
        .chain(out.declared_functions.iter())
        .filter_map(|declared| {
            declared
                .fqn
                .rsplit_once('.')
                .map(|(package, _)| package.to_string())
        })
        .collect();
    let known = out
        .nodes
        .iter()
        .filter(|node| node.label == "Symbol")
        .map(|node| node.id.clone())
        .collect::<HashSet<_>>();
    for pending in std::mem::take(&mut out.pending_calls) {
        let resolved = match &pending.member {
            Some(member) => types_by_fqn.get(&pending.fqn).and_then(|unique| {
                unique.as_ref().map(|declared| {
                    symbol_id(
                        id.repo,
                        &declared.path,
                        &format!("{}.{member}", declared.qualified),
                    )
                })
            }),
            None => functions_by_fqn.get(&pending.fqn).and_then(|unique| {
                unique
                    .as_ref()
                    .map(|declared| symbol_id(id.repo, &declared.path, &declared.qualified))
            }),
        };
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

/// Recover a Kotlin directory without retaining an incremental cache.
pub fn extract_dir(root: &Path, id: &SourceId<'_>) -> Result<Extraction, ExtractError> {
    extract_dir_incremental(root, id, &mut IncrementalCache::default())
        .map(|(extraction, _)| extraction)
}

#[cfg(test)]
mod tests;
