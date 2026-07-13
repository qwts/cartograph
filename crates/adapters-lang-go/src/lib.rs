//! Go deterministic (T0) language adapter (SPEC-00 §3.3, M10).
//!
//! Import-proven `net/http`, chi, and gin registrations become
//! Endpoint/HANDLES facts. Functions, imports, and direct calls become the
//! server graph, with Go-module-aware directory joins for local packages.

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node as TsNode, Parser, Query, QueryCursor};

const EXTRACTOR_ID: &str = "t0.adapter-go";

/// Go extraction errors.
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

/// Go graph facts from one file or directory.
#[derive(Debug, Clone, Default)]
pub struct Extraction {
    /// Recovered nodes.
    pub nodes: Vec<Node>,
    /// Recovered edges.
    pub edges: Vec<Edge>,
    pending_calls: Vec<PendingCall>,
    pending_endpoints: Vec<PendingEndpoint>,
}

#[derive(Debug, Clone)]
struct PendingCall {
    src: String,
    package_dir: String,
    exported: String,
    props: serde_json::Value,
}

#[derive(Debug, Clone)]
struct PendingEndpoint {
    node: Node,
    edge: Edge,
    package_dir: String,
    exported: String,
}

#[derive(Debug, Clone)]
struct CachedFile {
    source_hash: String,
    extraction: Extraction,
}

/// Reusable per-file Go parse cache (AC-0054).
#[derive(Debug, Default)]
pub struct IncrementalCache {
    files: BTreeMap<String, CachedFile>,
}

/// Physical Go source work performed by an incremental extraction.
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
        retarget_props_commit(&mut pending.props, commit);
    }
    for pending in &mut extraction.pending_endpoints {
        retarget_props_commit(&mut pending.node.props, commit);
        retarget_props_commit(&mut pending.edge.props, commit);
    }
}

#[derive(Debug, Clone)]
struct ImportBinding {
    path: String,
    package_dir: Option<String>,
}

fn local_package_dir(import_path: &str, module_path: Option<&str>) -> Option<String> {
    let module_path = module_path?;
    if import_path == module_path {
        return Some(String::new());
    }
    import_path
        .strip_prefix(module_path)
        .and_then(|suffix| suffix.strip_prefix('/'))
        .map(str::to_string)
}

fn unquote(text: &str) -> Option<String> {
    let text = text.trim();
    ((text.starts_with('"') && text.ends_with('"'))
        || (text.starts_with('`') && text.ends_with('`')))
    .then(|| text[1..text.len() - 1].to_string())
}

fn default_import_name(import_path: &str) -> &str {
    match import_path {
        "net/http" => "http",
        "github.com/go-chi/chi" | "github.com/go-chi/chi/v5" => "chi",
        "github.com/gin-gonic/gin" => "gin",
        _ => import_path.rsplit('/').next().unwrap_or(import_path),
    }
}

fn parse_imports(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    language: &tree_sitter::Language,
    module_path: Option<&str>,
    out: &mut Extraction,
) -> HashMap<String, ImportBinding> {
    let query = Query::new(language, "(import_spec) @spec").expect("static query");
    let mut bindings = HashMap::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, cx.source);
    while let Some(found) = matches.next() {
        let spec = found.captures[0].node;
        let Some(path_node) = spec.child_by_field_name("path") else {
            continue;
        };
        let Some(import_path) = unquote(cx.text(&path_node)) else {
            continue;
        };
        let explicit = spec
            .child_by_field_name("name")
            .map(|name| cx.text(&name).to_string());
        let local = explicit.unwrap_or_else(|| default_import_name(&import_path).to_string());
        if local != "_" && local != "." {
            bindings.insert(
                local,
                ImportBinding {
                    package_dir: local_package_dir(&import_path, module_path),
                    path: import_path.clone(),
                },
            );
        }
        out.edges.push(Edge {
            src: file_id(cx.id.repo, cx.path),
            dst: format!("mod:{import_path}"),
            label: "IMPORTS".into(),
            props: serde_json::json!({
                "specifier": import_path,
                "prov": cx.prov(&spec, &format!("IMPORTS {}", cx.text(&spec))),
            }),
        });
    }
    bindings
}

fn package_dir(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|parent| parent.to_string_lossy().replace('\\', "/"))
        .filter(|parent| parent != ".")
        .unwrap_or_default()
}

fn receiver_type(cx: &FileCx<'_>, function: TsNode<'_>) -> Option<String> {
    let receiver = function.child_by_field_name("receiver")?;
    cx.text(&receiver)
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .rfind(|piece| !piece.is_empty())
        .map(str::to_string)
}

fn function_name(cx: &FileCx<'_>, function: TsNode<'_>) -> Option<(String, String)> {
    let simple = cx.text(&function.child_by_field_name("name")?).to_string();
    let qualified = receiver_type(cx, function)
        .map(|receiver| format!("{receiver}.{simple}"))
        .unwrap_or_else(|| simple.clone());
    Some((simple, qualified))
}

fn enclosing_function(mut node: TsNode<'_>, functions: &HashMap<usize, String>) -> Option<String> {
    while let Some(parent) = node.parent() {
        if matches!(parent.kind(), "function_declaration" | "method_declaration")
            && let Some(id) = functions.get(&parent.start_byte())
        {
            return Some(id.clone());
        }
        node = parent;
    }
    None
}

fn call_arguments(call: TsNode<'_>) -> Vec<TsNode<'_>> {
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut walk = arguments.walk();
    arguments.named_children(&mut walk).collect()
}

fn literal_string(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    matches!(
        node.kind(),
        "interpreted_string_literal" | "raw_string_literal"
    )
    .then(|| unquote(cx.text(&node)))
    .flatten()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Framework {
    HttpMux,
    Chi,
    Gin,
}

enum HandlerTarget {
    Resolved(String),
    Package {
        package_dir: String,
        exported: String,
    },
    Gap {
        expression: String,
    },
}

fn framework_factory(callee: &str, bindings: &HashMap<String, ImportBinding>) -> Option<Framework> {
    let (base, member) = callee.split_once('.')?;
    let path = &bindings.get(base)?.path;
    match (path.as_str(), member) {
        ("net/http", "NewServeMux") => Some(Framework::HttpMux),
        ("github.com/go-chi/chi", "NewRouter") | ("github.com/go-chi/chi/v5", "NewRouter") => {
            Some(Framework::Chi)
        }
        ("github.com/gin-gonic/gin", "Default" | "New") => Some(Framework::Gin),
        _ => None,
    }
}

fn assignment_receivers(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    language: &tree_sitter::Language,
    bindings: &HashMap<String, ImportBinding>,
) -> HashMap<String, Framework> {
    let query = Query::new(language, "(short_var_declaration) @assignment").expect("static query");
    let mut receivers = HashMap::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, cx.source);
    while let Some(found) = matches.next() {
        let assignment = found.captures[0].node;
        let raw = cx.text(&assignment);
        let Some((left, right)) = raw.split_once(":=") else {
            continue;
        };
        let variable = left.trim();
        let callee = right.trim().split('(').next().unwrap_or("").trim();
        if variable
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            && let Some(framework) = framework_factory(callee, bindings)
        {
            receivers.insert(variable.to_string(), framework);
        }
    }
    receivers
}

fn route_pattern(pattern: String) -> (String, String) {
    if let Some((method, path)) = pattern.split_once(' ')
        && !method.is_empty()
        && method
            .chars()
            .all(|character| character.is_ascii_uppercase())
        && path.starts_with('/')
    {
        return (method.into(), path.into());
    }
    ("ANY".into(), pattern)
}

fn endpoint_registration(
    cx: &FileCx<'_>,
    call: TsNode<'_>,
    callee: &str,
    bindings: &HashMap<String, ImportBinding>,
    receivers: &HashMap<String, Framework>,
    locals: &HashMap<String, String>,
) -> Option<(String, String, HandlerTarget, &'static str)> {
    let (base, member) = callee.split_once('.')?;
    let arguments = call_arguments(call);
    let route = arguments
        .first()
        .copied()
        .and_then(|argument| literal_string(cx, argument))?;
    let handler_name = arguments.get(1).map(|argument| cx.text(argument))?;
    let handler = if let Some(handler) = locals.get(handler_name) {
        HandlerTarget::Resolved(handler.clone())
    } else if let Some((base, exported)) = handler_name.split_once('.')
        && let Some(package_dir) = bindings
            .get(base)
            .and_then(|binding| binding.package_dir.clone())
    {
        HandlerTarget::Package {
            package_dir,
            exported: exported.to_string(),
        }
    } else {
        HandlerTarget::Gap {
            expression: handler_name.to_string(),
        }
    };
    if bindings.get(base).map(|binding| binding.path.as_str()) == Some("net/http")
        && matches!(member, "Handle" | "HandleFunc")
    {
        let (method, path) = route_pattern(route);
        return Some((method, path, handler, "net/http"));
    }
    let framework = receivers.get(base)?;
    match framework {
        Framework::HttpMux if matches!(member, "Handle" | "HandleFunc") => {
            let (method, path) = route_pattern(route);
            Some((method, path, handler, "net/http"))
        }
        Framework::Chi => {
            const METHODS: &[&str] = &["Get", "Post", "Put", "Delete", "Patch", "Options", "Head"];
            METHODS
                .contains(&member)
                .then(|| (member.to_ascii_uppercase(), route, handler, "chi"))
        }
        Framework::Gin => {
            const METHODS: &[&str] = &["GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "HEAD"];
            METHODS
                .contains(&member)
                .then(|| (member.into(), route, handler, "gin"))
        }
        _ => None,
    }
}

fn close_over(extraction: &mut Extraction) {
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

fn extract_source_with_module(
    source: &[u8],
    path: &str,
    id: &SourceId<'_>,
    module_path: Option<&str>,
) -> Result<Extraction, ExtractError> {
    let language: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
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
            "language": "go",
            "prov": cx.prov(&root, &format!("File {path}")),
        }),
    });
    let bindings = parse_imports(&cx, root, &language, module_path, &mut out);
    let receivers = assignment_receivers(&cx, root, &language, &bindings);

    let function_query = Query::new(
        &language,
        "[(function_declaration) (method_declaration)] @function",
    )
    .expect("static query");
    let mut functions_by_start = HashMap::new();
    let mut locals = HashMap::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&function_query, root, source);
    while let Some(found) = matches.next() {
        let function = found.captures[0].node;
        let Some((simple, qualified)) = function_name(&cx, function) else {
            continue;
        };
        let symbol = symbol_id(id.repo, path, &qualified);
        functions_by_start.insert(function.start_byte(), symbol.clone());
        if qualified == simple {
            locals.insert(simple.clone(), symbol.clone());
        }
        out.nodes.push(Node {
            id: symbol.clone(),
            label: "Symbol".into(),
            props: serde_json::json!({
                "name": qualified,
                "simple_name": simple,
                "package_dir": package_dir(path),
                "kind": "function",
                "language": "go",
                "prov": cx.prov(&function, &format!("Symbol {symbol}")),
            }),
        });
        out.edges.push(Edge {
            src: symbol.clone(),
            dst: file_id(id.repo, path),
            label: "DEFINED_IN".into(),
            props: serde_json::json!({
                "prov": cx.prov(&function, &format!("DEFINED_IN {symbol}")),
            }),
        });
    }

    let call_query = Query::new(&language, "(call_expression) @call").expect("static query");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&call_query, root, source);
    while let Some(found) = matches.next() {
        let call = found.captures[0].node;
        let Some(function) = call.child_by_field_name("function") else {
            continue;
        };
        let callee = cx.text(&function);
        if let Some((method, route, handler, framework)) =
            endpoint_registration(&cx, call, callee, &bindings, &receivers, &locals)
        {
            let endpoint = format!("ep:{}@{method}:{route}", id.repo);
            let mut endpoint_node = Node {
                id: endpoint.clone(),
                label: "Endpoint".into(),
                props: serde_json::json!({
                    "method": method,
                    "path": route,
                    "framework": framework,
                    "language": "go",
                    "prov": cx.prov(&call, &format!("Endpoint {endpoint}")),
                }),
            };
            let mut handles = Edge {
                src: endpoint.clone(),
                dst: String::new(),
                label: "HANDLES".into(),
                props: serde_json::json!({
                    "prov": cx.prov(&call, &format!("HANDLES {endpoint}")),
                }),
            };
            match handler {
                HandlerTarget::Resolved(handler) => {
                    endpoint_node.props["handler_sym"] = handler.clone().into();
                    handles.dst = handler;
                    out.nodes.push(endpoint_node);
                    out.edges.push(handles);
                }
                HandlerTarget::Package {
                    package_dir,
                    exported,
                } => out.pending_endpoints.push(PendingEndpoint {
                    node: endpoint_node,
                    edge: handles,
                    package_dir,
                    exported,
                }),
                HandlerTarget::Gap { expression } => {
                    let gap_id =
                        format!("gap:go-handler:{}@{}@{}", id.repo, path, call.start_byte());
                    endpoint_node.props["handler_sym"] = gap_id.clone().into();
                    let gap = Node {
                        id: gap_id.clone(),
                        label: "Gap".into(),
                        props: serde_json::json!({
                            "handler_expression": expression,
                            "reason": "unresolved Go route handler expression",
                            "attempted_tiers": ["T0"],
                            "prov": cx.prov_with_confidence(
                                &call,
                                ConfidenceTier::Gap,
                                &format!("Gap {gap_id}"),
                            ),
                        }),
                    };
                    handles.dst = gap_id;
                    handles.props = serde_json::json!({
                        "attempted_resolution": "go-handler-expression",
                        "prov": cx.prov_with_confidence(
                            &call,
                            ConfidenceTier::Gap,
                            &format!("HANDLES {} -> {}", endpoint, handles.dst),
                        ),
                    });
                    out.nodes.push(endpoint_node);
                    out.nodes.push(gap);
                    out.edges.push(handles);
                }
            }
        }

        let Some(src) = enclosing_function(call, &functions_by_start) else {
            continue;
        };
        if !callee.contains('.') {
            if let Some(dst) = locals.get(callee)
                && src != *dst
            {
                out.edges.push(Edge {
                    src,
                    dst: dst.clone(),
                    label: "CALLS".into(),
                    props: serde_json::json!({
                        "prov": cx.prov(&call, &format!("CALLS {callee}")),
                    }),
                });
            }
            continue;
        }
        let Some((base, exported)) = callee.split_once('.') else {
            continue;
        };
        let Some(package_dir) = bindings
            .get(base)
            .and_then(|binding| binding.package_dir.clone())
        else {
            continue;
        };
        out.pending_calls.push(PendingCall {
            src,
            package_dir,
            exported: exported.to_string(),
            props: serde_json::json!({
                "resolution": "go-module-directory-proven",
                "prov": cx.prov(&call, &format!("CALLS {callee}")),
            }),
        });
    }
    Ok(out)
}

/// Recover deterministic facts from one Go source file.
pub fn extract_source(
    source: &[u8],
    path: &str,
    id: &SourceId<'_>,
) -> Result<Extraction, ExtractError> {
    extract_source_with_module(source, path, id, None)
}

fn collect_go_files(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), ExtractError> {
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
                    "vendor" | "node_modules" | "dist" | "build" | "bin"
                )
            {
                continue;
            }
            collect_go_files(root, &path, out)?;
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("go")
            && !name.ends_with("_test.go")
            && !has_platform_suffix(&name)
            && !has_explicit_build_constraint(&std::fs::read(&path)?)
        {
            let relative = path.strip_prefix(root).expect("walk stays beneath root");
            out.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

fn has_explicit_build_constraint(source: &[u8]) -> bool {
    let text = String::from_utf8_lossy(source);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("//go:build") || line.starts_with("// +build") {
            return true;
        }
        if line.starts_with("package ") || (!line.is_empty() && !line.starts_with("//")) {
            break;
        }
    }
    false
}

fn has_platform_suffix(name: &str) -> bool {
    const GOOS: &[&str] = &[
        "aix",
        "android",
        "darwin",
        "dragonfly",
        "freebsd",
        "illumos",
        "ios",
        "js",
        "linux",
        "netbsd",
        "openbsd",
        "plan9",
        "solaris",
        "wasip1",
        "windows",
    ];
    const GOARCH: &[&str] = &[
        "386", "amd64", "arm", "arm64", "loong64", "mips", "mips64", "mips64le", "mipsle", "ppc64",
        "ppc64le", "riscv64", "s390x", "wasm",
    ];
    let stem = name.strip_suffix(".go").unwrap_or(name);
    let mut parts = stem.rsplit('_');
    let last = parts.next().unwrap_or("");
    GOOS.contains(&last) || GOARCH.contains(&last)
}

fn module_path(root: &Path) -> Result<Option<String>, ExtractError> {
    let path = root.join("go.mod");
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(raw.lines().find_map(|line| {
        line.trim()
            .strip_prefix("module ")
            .map(str::trim)
            .filter(|module| !module.is_empty())
            .map(str::to_string)
    }))
}

/// Recover a Go directory with content-addressed per-file parse reuse.
pub fn extract_dir_incremental(
    root: &Path,
    id: &SourceId<'_>,
    cache: &mut IncrementalCache,
) -> Result<(Extraction, IncrementalStats), ExtractError> {
    let module_path = module_path(root)?;
    let mut files = Vec::new();
    collect_go_files(root, root, &mut files)?;
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
        let source = std::fs::read(root.join(&path))?;
        let source_hash = core_prov::content_hash(&source);
        let context_hash = core_prov::content_hash(
            format!("{source_hash}\0{}", module_path.as_deref().unwrap_or("")).as_bytes(),
        );
        let extraction = if let Some(cached) = cache
            .files
            .get(&path)
            .filter(|cached| cached.source_hash == context_hash)
        {
            stats.reused_files += 1;
            let mut extraction = cached.extraction.clone();
            retarget_commit(&mut extraction, id.commit);
            extraction
        } else {
            stats.recomputed_files += 1;
            extract_source_with_module(&source, &path, id, module_path.as_deref())?
        };
        cache.files.insert(
            path,
            CachedFile {
                source_hash: context_hash,
                extraction: extraction.clone(),
            },
        );
        out.nodes.extend(extraction.nodes);
        out.edges.extend(extraction.edges);
        out.pending_calls.extend(extraction.pending_calls);
        out.pending_endpoints.extend(extraction.pending_endpoints);
    }
    let symbol_index = out
        .nodes
        .iter()
        .filter(|node| node.label == "Symbol")
        .filter_map(|node| {
            Some((
                (
                    node.props.get("package_dir")?.as_str()?.to_string(),
                    node.props.get("simple_name")?.as_str()?.to_string(),
                ),
                node.id.clone(),
            ))
        })
        .collect::<BTreeMap<_, _>>();
    for pending in std::mem::take(&mut out.pending_calls) {
        if let Some(dst) = symbol_index.get(&(pending.package_dir, pending.exported)) {
            out.edges.push(Edge {
                src: pending.src,
                dst: dst.clone(),
                label: "CALLS".into(),
                props: pending.props,
            });
        }
    }
    for mut pending in std::mem::take(&mut out.pending_endpoints) {
        if let Some(dst) = symbol_index.get(&(pending.package_dir, pending.exported)) {
            pending.node.props["handler_sym"] = dst.clone().into();
            pending.edge.dst = dst.clone();
            out.nodes.push(pending.node);
            out.edges.push(pending.edge);
        }
    }
    close_over(&mut out);
    Ok((out, stats))
}

/// Recover a Go directory without retaining an incremental cache.
pub fn extract_dir(root: &Path, id: &SourceId<'_>) -> Result<Extraction, ExtractError> {
    extract_dir_incremental(root, id, &mut IncrementalCache::default())
        .map(|(extraction, _)| extraction)
}

#[cfg(test)]
mod tests;
