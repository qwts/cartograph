//! Python deterministic (T0) language adapter (SPEC-00 §3.3, M10).
//!
//! Import-proven FastAPI and Flask decorators become Endpoint/HANDLES facts;
//! functions, imports, and direct calls become the server graph. Directory
//! joins prove local imported call targets after every incremental pass. This
//! tier never calls an LLM and every emitted fact carries exact source-span
//! provenance.

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node as TsNode, Parser, Query, QueryCursor};

const EXTRACTOR_ID: &str = "t0.adapter-python";

/// Python extraction errors.
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

/// Python graph facts from one file or directory.
#[derive(Debug, Clone, Default)]
pub struct Extraction {
    /// Recovered nodes.
    pub nodes: Vec<Node>,
    /// Recovered edges.
    pub edges: Vec<Edge>,
    pending_calls: Vec<PendingCall>,
}

#[derive(Debug, Clone)]
struct PendingCall {
    resolved: Edge,
    gap: Option<(Node, Edge)>,
}

#[derive(Debug, Clone)]
struct CachedFile {
    source_hash: String,
    extraction: Extraction,
}

/// Reusable per-file Python parse cache (AC-0053).
#[derive(Debug, Default)]
pub struct IncrementalCache {
    files: BTreeMap<String, CachedFile>,
}

/// Physical Python source work performed by an incremental extraction.
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
        retarget_props_commit(&mut pending.resolved.props, commit);
        if let Some((node, edge)) = &mut pending.gap {
            retarget_props_commit(&mut node.props, commit);
            retarget_props_commit(&mut edge.props, commit);
        }
    }
}

fn normalize_path(path: PathBuf) -> Option<String> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(normalized.to_string_lossy().replace('\\', "/"))
}

fn module_file(path: &str, module: &str) -> Option<String> {
    let dots = module.chars().take_while(|ch| *ch == '.').count();
    let tail = module[dots..].trim();
    let mut base = if dots == 0 {
        PathBuf::new()
    } else {
        let mut parent = Path::new(path).parent()?.to_path_buf();
        for _ in 1..dots {
            parent.pop();
        }
        parent
    };
    if !tail.is_empty() {
        for part in tail.split('.') {
            base.push(part);
        }
    }
    if base.as_os_str().is_empty() {
        return None;
    }
    base.set_extension("py");
    normalize_path(base)
}

#[derive(Debug, Clone)]
enum ImportBinding {
    Module {
        module: String,
    },
    Symbol {
        module: String,
        exported: String,
        target_file: Option<String>,
        relative: bool,
    },
}

fn clean_import_piece(piece: &str) -> &str {
    piece.trim().trim_matches(|ch| ch == '(' || ch == ')')
}

fn parse_imports(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    language: &tree_sitter::Language,
    out: &mut Extraction,
) -> HashMap<String, ImportBinding> {
    let query = Query::new(
        language,
        "[(import_statement) (import_from_statement)] @import",
    )
    .expect("static query");
    let mut bindings = HashMap::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, cx.source);
    while let Some(found) = matches.next() {
        let statement = found.captures[0].node;
        let raw = cx.text(&statement).replace(['\n', '\\'], " ");
        let mut modules = BTreeSet::new();
        if let Some(rest) = raw.trim().strip_prefix("import ") {
            for piece in rest.split(',').map(clean_import_piece) {
                let mut alias = piece.split_whitespace();
                let Some(module) = alias.next() else { continue };
                let local = match (alias.next(), alias.next()) {
                    (Some("as"), Some(local)) => local,
                    _ => module.split('.').next().unwrap_or(module),
                };
                modules.insert(module.to_string());
                bindings.insert(
                    local.to_string(),
                    ImportBinding::Module {
                        module: module.to_string(),
                    },
                );
            }
        } else if let Some(rest) = raw.trim().strip_prefix("from ")
            && let Some((module, names)) = rest.split_once(" import ")
        {
            let module = module.trim();
            let relative = module.starts_with('.');
            modules.insert(module.to_string());
            for piece in names.split(',').map(clean_import_piece) {
                let mut alias = piece.split_whitespace();
                let Some(exported) = alias.next() else {
                    continue;
                };
                if exported == "*" {
                    continue;
                }
                let local = match (alias.next(), alias.next()) {
                    (Some("as"), Some(local)) => local,
                    _ => exported,
                };
                bindings.insert(
                    local.to_string(),
                    ImportBinding::Symbol {
                        module: module.to_string(),
                        exported: exported.to_string(),
                        target_file: module_file(cx.path, module),
                        relative,
                    },
                );
            }
        }
        for module in modules {
            let destination = if module.starts_with('.') {
                module_file(cx.path, &module)
                    .map(|path| file_id(cx.id.repo, &path))
                    .unwrap_or_else(|| format!("mod:{module}"))
            } else {
                format!("mod:{module}")
            };
            out.edges.push(Edge {
                src: file_id(cx.id.repo, cx.path),
                dst: destination,
                label: "IMPORTS".into(),
                props: serde_json::json!({
                    "specifier": module,
                    "prov": cx.prov(&statement, &format!("IMPORTS {}", cx.text(&statement))),
                }),
            });
        }
    }
    bindings
}

fn literal_string(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let text = cx.text(&node).trim();
    let quote_at = text.find(['\'', '"'])?;
    if text[..quote_at]
        .chars()
        .any(|prefix| matches!(prefix, 'f' | 'F'))
    {
        return None;
    }
    Some(text[quote_at..].trim_matches(['\'', '"']).to_string())
}

fn call_name(cx: &FileCx<'_>, call: TsNode<'_>) -> Option<String> {
    call.child_by_field_name("function")
        .map(|function| cx.text(&function).to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Framework {
    FastApi,
    Flask,
}

fn framework_factory(callee: &str, bindings: &HashMap<String, ImportBinding>) -> Option<Framework> {
    let mut parts = callee.split('.');
    let base = parts.next()?;
    let member = parts.next();
    match (bindings.get(base), member) {
        (
            Some(ImportBinding::Symbol {
                module, exported, ..
            }),
            None,
        ) if module == "fastapi" && exported == "FastAPI" => Some(Framework::FastApi),
        (
            Some(ImportBinding::Symbol {
                module, exported, ..
            }),
            None,
        ) if module == "flask" && exported == "Flask" => Some(Framework::Flask),
        (Some(ImportBinding::Module { module }), Some("FastAPI")) if module == "fastapi" => {
            Some(Framework::FastApi)
        }
        (Some(ImportBinding::Module { module }), Some("Flask")) if module == "flask" => {
            Some(Framework::Flask)
        }
        _ => None,
    }
}

fn assignment_receivers(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    language: &tree_sitter::Language,
    bindings: &HashMap<String, ImportBinding>,
) -> HashMap<String, Framework> {
    let query = Query::new(
        language,
        r#"(assignment left: (identifier) @variable right: (call) @call)"#,
    )
    .expect("static query");
    let mut receivers = HashMap::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, cx.source);
    while let Some(found) = matches.next() {
        let mut variable = None;
        let mut call = None;
        for capture in found.captures {
            match query.capture_names()[capture.index as usize] {
                "variable" => variable = Some(cx.text(&capture.node).to_string()),
                "call" => call = Some(capture.node),
                _ => {}
            }
        }
        let (Some(variable), Some(callee)) = (variable, call.and_then(|call| call_name(cx, call)))
        else {
            continue;
        };
        if let Some(framework) = framework_factory(&callee, bindings) {
            receivers.insert(variable, framework);
        }
    }
    receivers
}

fn qualified_function_name(cx: &FileCx<'_>, function: TsNode<'_>) -> Option<String> {
    let name = function.child_by_field_name("name")?;
    let mut names = vec![cx.text(&name).to_string()];
    let mut parent = function.parent();
    while let Some(node) = parent {
        if matches!(node.kind(), "function_definition" | "class_definition")
            && let Some(name) = node.child_by_field_name("name")
        {
            names.push(cx.text(&name).to_string());
        }
        parent = node.parent();
    }
    names.reverse();
    Some(names.join("."))
}

fn named_arguments(node: TsNode<'_>) -> Vec<TsNode<'_>> {
    let Some(arguments) = node.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut walk = arguments.walk();
    arguments.named_children(&mut walk).collect()
}

fn decorator_methods(
    cx: &FileCx<'_>,
    decorator: TsNode<'_>,
    receivers: &HashMap<String, Framework>,
) -> Vec<(String, String, Framework)> {
    let mut walk = decorator.walk();
    let Some(call) = decorator
        .named_children(&mut walk)
        .find(|child| child.kind() == "call")
    else {
        return Vec::new();
    };
    let Some(function) = call.child_by_field_name("function") else {
        return Vec::new();
    };
    if function.kind() != "attribute" {
        return Vec::new();
    }
    let (Some(object), Some(attribute)) = (
        function.child_by_field_name("object"),
        function.child_by_field_name("attribute"),
    ) else {
        return Vec::new();
    };
    let Some(framework) = receivers.get(cx.text(&object)).copied() else {
        return Vec::new();
    };
    let method = cx.text(&attribute);
    let arguments = named_arguments(call);
    let Some(path) = arguments
        .first()
        .copied()
        .and_then(|argument| literal_string(cx, argument))
    else {
        return Vec::new();
    };
    const DIRECT_METHODS: &[&str] = &["get", "post", "put", "delete", "patch", "options", "head"];
    if DIRECT_METHODS.contains(&method) {
        return vec![(method.to_ascii_uppercase(), path, framework)];
    }
    if framework != Framework::Flask || method != "route" {
        return Vec::new();
    }
    let mut methods = Vec::new();
    for argument in arguments.iter().skip(1) {
        if argument.kind() != "keyword_argument" {
            continue;
        }
        let (Some(name), Some(value)) = (
            argument.child_by_field_name("name"),
            argument.child_by_field_name("value"),
        ) else {
            continue;
        };
        if cx.text(&name) != "methods" || value.kind() != "list" {
            continue;
        }
        let mut walk = value.walk();
        methods.extend(
            value
                .named_children(&mut walk)
                .filter_map(|item| literal_string(cx, item))
                .map(|method| method.to_ascii_uppercase()),
        );
    }
    if methods.is_empty() {
        methods.push("GET".into());
    }
    methods
        .into_iter()
        .map(|method| (method, path.clone(), framework))
        .collect()
}

fn enclosing_function(mut node: TsNode<'_>, functions: &HashMap<usize, String>) -> Option<String> {
    while let Some(parent) = node.parent() {
        if parent.kind() == "function_definition"
            && let Some(id) = functions.get(&parent.start_byte())
        {
            return Some(id.clone());
        }
        node = parent;
    }
    None
}

fn close_over_endpoints(extraction: &mut Extraction) {
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

/// Recover deterministic facts from one Python source file.
pub fn extract_source(
    source: &[u8],
    path: &str,
    id: &SourceId<'_>,
) -> Result<Extraction, ExtractError> {
    let language: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
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
            "language": "python",
            "prov": cx.prov(&root, &format!("File {path}")),
        }),
    });

    let bindings = parse_imports(&cx, root, &language, &mut out);
    let receivers = assignment_receivers(&cx, root, &language, &bindings);
    let function_query = Query::new(
        &language,
        "(function_definition name: (identifier) @name) @function",
    )
    .expect("static query");
    let mut functions_by_start = HashMap::new();
    let mut locals = HashMap::new();
    let mut functions = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&function_query, root, source);
    while let Some(found) = matches.next() {
        let mut function = None;
        let mut simple_name = None;
        for capture in found.captures {
            match function_query.capture_names()[capture.index as usize] {
                "function" => function = Some(capture.node),
                "name" => simple_name = Some(cx.text(&capture.node).to_string()),
                _ => {}
            }
        }
        let (Some(function), Some(simple_name), Some(qualified_name)) = (
            function,
            simple_name,
            function.and_then(|function| qualified_function_name(&cx, function)),
        ) else {
            continue;
        };
        let symbol = symbol_id(id.repo, path, &qualified_name);
        functions_by_start.insert(function.start_byte(), symbol.clone());
        if qualified_name == simple_name {
            locals.insert(simple_name.clone(), symbol.clone());
        }
        out.nodes.push(Node {
            id: symbol.clone(),
            label: "Symbol".into(),
            props: serde_json::json!({
                "name": qualified_name,
                "kind": "function",
                "language": "python",
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
        functions.push((function, symbol));
    }

    for (function, handler) in &functions {
        let Some(parent) = function
            .parent()
            .filter(|parent| parent.kind() == "decorated_definition")
        else {
            continue;
        };
        let mut walk = parent.walk();
        for decorator in parent
            .named_children(&mut walk)
            .filter(|child| child.kind() == "decorator")
        {
            for (method, route, framework) in decorator_methods(&cx, decorator, &receivers) {
                let endpoint = format!("ep:{}@{method}:{route}", id.repo);
                let framework_name = match framework {
                    Framework::FastApi => "fastapi",
                    Framework::Flask => "flask",
                };
                out.nodes.push(Node {
                    id: endpoint.clone(),
                    label: "Endpoint".into(),
                    props: serde_json::json!({
                        "method": method,
                        "path": route,
                        "handler_sym": handler,
                        "framework": framework_name,
                        "language": "python",
                        "prov": cx.prov(&decorator, &format!("Endpoint {endpoint}")),
                    }),
                });
                out.edges.push(Edge {
                    src: endpoint.clone(),
                    dst: handler.clone(),
                    label: "HANDLES".into(),
                    props: serde_json::json!({
                        "prov": cx.prov(&decorator, &format!("HANDLES {endpoint} -> {handler}")),
                    }),
                });
            }
        }
    }

    let call_query = Query::new(
        &language,
        "(call function: [(identifier) (attribute)] @callee) @call",
    )
    .expect("static query");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&call_query, root, source);
    while let Some(found) = matches.next() {
        let mut call = None;
        let mut callee = None;
        for capture in found.captures {
            match call_query.capture_names()[capture.index as usize] {
                "call" => call = Some(capture.node),
                "callee" => callee = Some(cx.text(&capture.node).to_string()),
                _ => {}
            }
        }
        let (Some(call), Some(callee), Some(src)) = (
            call,
            callee,
            call.and_then(|call| enclosing_function(call, &functions_by_start)),
        ) else {
            continue;
        };
        let direct = callee
            .split_once('.')
            .is_none()
            .then(|| locals.get(&callee).cloned())
            .flatten();
        if let Some(dst) = direct {
            if src != dst {
                out.edges.push(Edge {
                    src,
                    dst,
                    label: "CALLS".into(),
                    props: serde_json::json!({
                        "prov": cx.prov(&call, &format!("CALLS {callee}")),
                    }),
                });
            }
            continue;
        }

        let imported = if let Some((base, member)) = callee.split_once('.') {
            match bindings.get(base) {
                Some(ImportBinding::Module { module }) => module_file(path, module)
                    .map(|target| (target, member.to_string(), module.starts_with('.'))),
                _ => None,
            }
        } else {
            match bindings.get(&callee) {
                Some(ImportBinding::Symbol {
                    target_file,
                    exported,
                    relative,
                    ..
                }) => target_file
                    .clone()
                    .map(|target| (target, exported.clone(), *relative)),
                _ => None,
            }
        };
        let Some((target_file, exported, relative)) = imported else {
            continue;
        };
        let dst = symbol_id(id.repo, &target_file, &exported);
        let resolved = Edge {
            src: src.clone(),
            dst: dst.clone(),
            label: "CALLS".into(),
            props: serde_json::json!({
                "resolution": "directory-proven",
                "prov": cx.prov(&call, &format!("CALLS {src} -> {dst}")),
            }),
        };
        let gap = relative.then(|| {
            let gap_id = format!("gap:call:{}@{}@{}", id.repo, path, call.start_byte());
            let node = Node {
                id: gap_id.clone(),
                label: "Gap".into(),
                props: serde_json::json!({
                    "callee": callee,
                    "reason": "unresolved Python import target",
                    "attempted_tiers": ["T0"],
                    "prov": cx.prov_with_confidence(
                        &call,
                        ConfidenceTier::Gap,
                        &format!("Gap {gap_id}"),
                    ),
                }),
            };
            let edge = Edge {
                src,
                dst: gap_id.clone(),
                label: "CALLS".into(),
                props: serde_json::json!({
                    "attempted_resolution": "directory-import",
                    "prov": cx.prov_with_confidence(
                        &call,
                        ConfidenceTier::Gap,
                        &format!("CALLS -> {gap_id}"),
                    ),
                }),
            };
            (node, edge)
        });
        out.pending_calls.push(PendingCall { resolved, gap });
    }

    Ok(out)
}

fn collect_python_files(
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
                    "__pycache__" | "venv" | "site-packages" | "node_modules" | "dist" | "build"
                )
            {
                continue;
            }
            collect_python_files(root, &path, out)?;
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("py") {
            let relative = path.strip_prefix(root).expect("walk stays beneath root");
            out.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// Recover a Python directory with content-addressed per-file parse reuse.
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
    collect_python_files(root, root, &mut files)?;
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
    }
    let known = out
        .nodes
        .iter()
        .filter(|node| node.label == "Symbol")
        .map(|node| node.id.clone())
        .collect::<HashSet<_>>();
    for pending in std::mem::take(&mut out.pending_calls) {
        if known.contains(&pending.resolved.dst) {
            out.edges.push(pending.resolved);
        } else if let Some((node, edge)) = pending.gap {
            out.nodes.push(node);
            out.edges.push(edge);
        }
    }
    close_over_endpoints(&mut out);
    Ok((out, stats))
}

/// Recover a Python directory without retaining an incremental cache.
pub fn extract_dir(root: &Path, id: &SourceId<'_>) -> Result<Extraction, ExtractError> {
    extract_dir_incremental(root, id, &mut IncrementalCache::default())
        .map(|(extraction, _)| extraction)
}

#[cfg(test)]
mod tests;
