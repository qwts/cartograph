//! TypeScript language adapter — deterministic (T0) extraction via
//! tree-sitter (SPEC-00 §3.3, US-0002).
//!
//! Extracts, per file: the `File` node, `Symbol` nodes (functions, arrow
//! consts, anonymous route handlers), `IMPORTS` edges, intra- and cross-file
//! `CALLS` edges (cross-file only where an import binds the name — still
//! deterministic), and Express `Endpoint` nodes with `HANDLES` edges.
//! Endpoint receivers are tracked from framework factory calls
//! (`express()` / `express.Router()`), never guessed from variable names.
//!
//! Every fact carries [`core_prov::Provenance`] (tier `Deterministic`,
//! confidence `Confirmed`, evidence span, content hash) in its `props.prov`
//! (AC-0004, AC-0006). This tier never calls an LLM.

use adapters_fw::EXPRESS;
use adapters_fw::client::{FetchSite, NEXT_PAGES_DIR, REACT_ROUTER};
use adapters_fw::events::{
    ChannelRole, EVENT_SDKS, EventSite, IdentityArg, IdentityExpr, SdkPattern,
};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node as TsNode, Parser, Query, QueryCursor};

/// Extraction errors.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    /// tree-sitter grammar/version mismatch.
    #[error("language: {0}")]
    Language(#[from] tree_sitter::LanguageError),
    /// The parser returned no tree (timeout/cancellation — not expected).
    #[error("parse produced no tree for {0}")]
    NoTree(String),
    /// Filesystem failure while walking a directory.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Facts extracted from one file or one directory walk.
#[derive(Debug, Default)]
pub struct Extraction {
    /// Graph nodes (props carry provenance under `prov`).
    pub nodes: Vec<Node>,
    /// Graph edges (props carry provenance under `prov`).
    pub edges: Vec<Edge>,
    /// Producer/consumer call sites (US-0004) — not yet graph facts; the
    /// `events` crate resolves identities and stitches them into channels.
    pub event_sites: Vec<EventSite>,
    /// Data-fetch call sites (US-0005) — the `events` crate resolves their
    /// URLs against recovered endpoints into FETCHES edges.
    pub fetch_sites: Vec<FetchSite>,
    /// Default-exported symbol per file (`path` → sym id) — Next.js pages
    /// resolve their screen component through this.
    pub default_exports: HashMap<String, String>,
}

impl Extraction {
    /// Ensure every edge endpoint exists as a node; unresolved targets become
    /// placeholder nodes so referential integrity holds in the store.
    /// Placeholders are labeled by their id scheme (`file:`, `sym:`, `mod:`).
    pub fn close_over_endpoints(&mut self) {
        let mut known: std::collections::HashSet<String> =
            self.nodes.iter().map(|n| n.id.clone()).collect();
        let mut placeholders = Vec::new();
        for edge in &self.edges {
            for id in [&edge.src, &edge.dst] {
                if known.contains(id.as_str()) {
                    continue;
                }
                let label = match id.split(':').next() {
                    Some("file") => "File",
                    Some("sym") => "Symbol",
                    Some("mod") => "Module",
                    Some("ep") => "Endpoint",
                    Some("res") => "Resource",
                    Some("chan") => "Channel",
                    Some("gap") => "Gap",
                    Some("screen") => "Screen",
                    _ => "Unknown",
                };
                placeholders.push(Node {
                    id: id.clone(),
                    label: label.into(),
                    props: serde_json::json!({ "placeholder": true }),
                });
                known.insert(id.clone());
            }
        }
        self.nodes.extend(placeholders);
    }
}

/// Identity of the source being extracted; lands in every `EvidenceRef`.
pub struct SourceId<'a> {
    /// Repository identifier (e.g. `owner/name`, or `local` for a bare dir).
    pub repo: &'a str,
    /// Commit SHA, or `workdir` when extracting an unversioned tree.
    pub commit: &'a str,
}

const EXTRACTOR_ID: &str = "t0.adapter-ts";

struct FileCx<'a> {
    source: &'a [u8],
    path: &'a str,
    id: &'a SourceId<'a>,
}

impl FileCx<'_> {
    fn prov(&self, node: &TsNode, fact: &str) -> serde_json::Value {
        let p = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
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
        .expect("Deterministic/Confirmed is always within ceiling");
        serde_json::to_value(p).expect("provenance serializes")
    }

    fn text(&self, node: &TsNode) -> &str {
        node.utf8_text(self.source).unwrap_or("")
    }
}

/// Node ids are repo-namespaced (`{kind}:{repo}@{rest}`, US-0001 slice 2):
/// the same relative path or route in two repos must never collide.
/// Channels (`chan:`) and npm modules (`mod:`) stay global — they are the
/// cross-repo stitch points.
fn file_id(repo: &str, path: &str) -> String {
    format!("file:{repo}@{path}")
}

fn sym_id(repo: &str, path: &str, name: &str) -> String {
    format!("sym:{repo}@{path}#{name}")
}

/// Resolve a relative import specifier against the importing file's path.
/// Returns `None` for bare (package) specifiers.
fn resolve_relative(from: &str, spec: &str) -> Option<String> {
    if !spec.starts_with('.') {
        return None;
    }
    let dir = Path::new(from).parent().unwrap_or(Path::new(""));
    let mut out = PathBuf::new();
    for comp in dir.join(spec).components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    let mut s = out.to_string_lossy().replace('\\', "/");
    if !s.ends_with(".ts") && !s.ends_with(".tsx") {
        s.push_str(".ts");
    }
    Some(s)
}

/// Walk ancestors to the enclosing named function (or anonymous handler)
/// and return its symbol id, if any.
fn enclosing_symbol(cx: &FileCx, mut node: TsNode) -> Option<String> {
    while let Some(parent) = node.parent() {
        match parent.kind() {
            "function_declaration" | "method_definition" => {
                let name = parent.child_by_field_name("name")?;
                return Some(sym_id(cx.id.repo, cx.path, cx.text(&name)));
            }
            "arrow_function" | "function_expression" => {
                // Named via `const f = () => {}`?
                if let Some(decl) = parent
                    .parent()
                    .filter(|p| p.kind() == "variable_declarator")
                    && let Some(name) = decl.child_by_field_name("name")
                {
                    return Some(sym_id(cx.id.repo, cx.path, cx.text(&name)));
                }
                // Anonymous (e.g. inline route handler): stable offset-keyed id,
                // shared with the endpoint extractor.
                return Some(sym_id(
                    cx.id.repo,
                    cx.path,
                    &format!("anon@{}", parent.start_byte()),
                ));
            }
            _ => node = parent,
        }
    }
    None
}

/// Walk ancestors and return the *nearest* enclosing capitalized function
/// — the React component a site belongs to (SPEC-00 §3.5:
/// `FETCHES(component → Endpoint)`). A fetch inside an event-handler
/// closure (`const submit = () => fetch(…)`) anchors at the component,
/// not the closure — but a fetch inside a *nested* component belongs to
/// that nested component, never the outer one (which may not render it).
fn enclosing_component(cx: &FileCx, node: TsNode) -> Option<String> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        let name = match parent.kind() {
            "function_declaration" | "method_definition" => parent
                .child_by_field_name("name")
                .map(|n| cx.text(&n).to_string()),
            "arrow_function" | "function_expression" => parent
                .parent()
                .filter(|p| p.kind() == "variable_declarator")
                .and_then(|d| d.child_by_field_name("name"))
                .map(|n| cx.text(&n).to_string()),
            _ => None,
        };
        if let Some(name) = name
            && name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        {
            return Some(sym_id(cx.id.repo, cx.path, &name));
        }
        current = parent;
    }
    None
}

/// Bare module specifiers compare with the `node:` scheme stripped
/// (`node:events` and `events` are the same module).
fn norm_module(spec: &str) -> String {
    spec.strip_prefix("node:").unwrap_or(spec).to_string()
}

/// Module a constructor came from, proven via the import map: for
/// `AWS.SQS` the base `AWS` must be import-bound; for `EventEmitter` the
/// name itself. `None` when the constructor is not import-proven.
fn ctor_module(ctor: &str, import_modules: &HashMap<String, String>) -> Option<String> {
    let base = ctor.split('.').next().unwrap_or(ctor);
    import_modules.get(base).map(|m| norm_module(m))
}

/// Classify a channel-identity expression at T0 (US-0004): literal,
/// env-file-resolvable, or runtime-computed. Local `const X = 'lit'`
/// bindings count as literals — same-file resolution is deterministic.
fn classify_identity(
    cx: &FileCx,
    expr: &TsNode,
    const_strings: &HashMap<String, String>,
) -> IdentityExpr {
    match expr.kind() {
        "string" => {
            let mut w = expr.walk();
            let frag = expr
                .children(&mut w)
                .find(|c| c.kind() == "string_fragment")
                .map(|f| cx.text(&f).to_string())
                .unwrap_or_default();
            IdentityExpr::Literal(frag)
        }
        "template_string" => {
            let mut w = expr.walk();
            if expr
                .children(&mut w)
                .any(|c| c.kind() == "template_substitution")
            {
                IdentityExpr::Computed(cx.text(expr).to_string())
            } else {
                IdentityExpr::Literal(cx.text(expr).trim_matches('`').to_string())
            }
        }
        "identifier" => match const_strings.get(cx.text(expr)) {
            Some(lit) => IdentityExpr::Literal(lit.clone()),
            None => IdentityExpr::Computed(cx.text(expr).to_string()),
        },
        "member_expression" => {
            let text = cx.text(expr);
            match text.strip_prefix("process.env.") {
                Some(key) if !key.is_empty() && !key.contains('.') => {
                    IdentityExpr::EnvRef(key.to_string())
                }
                _ => IdentityExpr::Computed(text.to_string()),
            }
        }
        // Bracket form: `process.env['KEY']` — same deterministic env ref.
        "subscript_expression" => {
            let object = expr.child_by_field_name("object");
            let index = expr.child_by_field_name("index");
            match (object, index) {
                (Some(o), Some(i)) if cx.text(&o) == "process.env" && i.kind() == "string" => {
                    match classify_identity(cx, &i, const_strings) {
                        IdentityExpr::Literal(key) if !key.is_empty() => IdentityExpr::EnvRef(key),
                        _ => IdentityExpr::Computed(cx.text(expr).to_string()),
                    }
                }
                _ => IdentityExpr::Computed(cx.text(expr).to_string()),
            }
        }
        _ => IdentityExpr::Computed(cx.text(expr).to_string()),
    }
}

/// Locate the channel identity inside a call's arguments per the registry
/// entry and classify it. A missing argument/key is `Computed` — the site
/// is real but its identity is not statically visible (escalates, AC-0012).
fn identity_in_args(
    cx: &FileCx,
    args: &TsNode,
    spec: IdentityArg,
    classify: &impl Fn(&TsNode) -> IdentityExpr,
) -> IdentityExpr {
    let mut w = args.walk();
    let Some(first) = args.children(&mut w).find(|c| c.is_named()) else {
        return IdentityExpr::Computed("<no argument>".into());
    };
    match spec {
        IdentityArg::First => classify(&first),
        IdentityArg::Key(key) => {
            // DFS for a `pair` with the registry key anywhere in the first
            // argument (handles nesting: `{ Entries: [{ DetailType: … }] }`).
            let mut stack = vec![first];
            while let Some(node) = stack.pop() {
                if node.kind() == "pair"
                    && let Some(k) = node.child_by_field_name("key")
                    && cx.text(&k).trim_matches(['"', '\'']) == key
                    && let Some(v) = node.child_by_field_name("value")
                {
                    return classify(&v);
                }
                let mut w = node.walk();
                let children: Vec<_> = node.children(&mut w).collect();
                // Reverse so the stack pops in document order — the first
                // matching pair wins deterministically.
                stack.extend(children.into_iter().rev());
            }
            IdentityExpr::Computed(cx.text(&first).to_string())
        }
    }
}

/// Extract facts from one TypeScript source file.
pub fn extract_source(
    source: &[u8],
    path: &str,
    id: &SourceId,
) -> Result<Extraction, ExtractError> {
    // TSX needs its own grammar (JSX node kinds); plain TS keeps the
    // TypeScript grammar. The queries below are valid against both.
    let is_tsx = path.ends_with(".tsx");
    let language: tree_sitter::Language = if is_tsx {
        tree_sitter_typescript::LANGUAGE_TSX.into()
    } else {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    };
    let mut parser = Parser::new();
    parser.set_language(&language)?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| ExtractError::NoTree(path.into()))?;
    let root = tree.root_node();
    let cx = FileCx { source, path, id };
    let mut out = Extraction::default();

    // File node spans the whole file.
    out.nodes.push(Node {
        id: file_id(id.repo, path),
        label: "File".into(),
        props: serde_json::json!({ "path": path, "prov": cx.prov(&root, &format!("File {path}")) }),
    });

    // --- Symbols: function declarations and arrow/function consts -----------
    let q_funcs = Query::new(
        &language,
        r#"
        (function_declaration name: (identifier) @name) @def
        (variable_declarator
            name: (identifier) @name
            value: [(arrow_function) (function_expression)]) @def
        "#,
    )
    .expect("static query");
    let mut locals: HashMap<String, String> = HashMap::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q_funcs, root, source);
    while let Some(m) = matches.next() {
        let name_node = m.nodes_for_capture_index(0).next().or_else(|| {
            m.captures
                .iter()
                .find(|c| q_funcs.capture_names()[c.index as usize] == "name")
                .map(|c| c.node)
        });
        let def_node = m
            .captures
            .iter()
            .find(|c| q_funcs.capture_names()[c.index as usize] == "def")
            .map(|c| c.node);
        let (Some(name_node), Some(def_node)) = (name_node, def_node) else {
            continue;
        };
        let name = cx.text(&name_node).to_string();
        let sid = sym_id(id.repo, path, &name);
        // A capitalized function in a .tsx file is a React component
        // (SPEC-00 §3.5) — same node id, so call edges keep working.
        let is_component = is_tsx && name.chars().next().is_some_and(|c| c.is_ascii_uppercase());
        out.nodes.push(Node {
            id: sid.clone(),
            label: if is_component { "Component" } else { "Symbol" }.into(),
            props: serde_json::json!({
                "name": name,
                "kind": if is_component { "Component" } else { "Function" },
                "prov": cx.prov(&def_node, &format!("Symbol {sid}")),
            }),
        });
        out.edges.push(Edge {
            src: sid.clone(),
            dst: file_id(id.repo, path),
            label: "DEFINED_IN".into(),
            props: serde_json::json!({ "prov": cx.prov(&def_node, &format!("DEFINED_IN {sid}")) }),
        });
        locals.insert(name, sid);
    }

    // --- Imports: IMPORTS edges + imported-name -> foreign symbol map -------
    let q_imports = Query::new(
        &language,
        r#"
        (import_statement
            (import_clause)? @clause
            source: (string (string_fragment) @source)) @stmt
        "#,
    )
    .expect("static query");
    let mut imported: HashMap<String, String> = HashMap::new(); // local name -> foreign sym id
    let mut import_modules: HashMap<String, String> = HashMap::new(); // local name -> module spec
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q_imports, root, source);
    while let Some(m) = matches.next() {
        let mut spec = None;
        let mut stmt = None;
        let mut clause = None;
        for c in m.captures {
            match q_imports.capture_names()[c.index as usize] {
                "source" => spec = Some(cx.text(&c.node).to_string()),
                "stmt" => stmt = Some(c.node),
                "clause" => clause = Some(c.node),
                _ => {}
            }
        }
        let (Some(spec), Some(stmt)) = (spec, stmt) else {
            continue;
        };
        let target_file = resolve_relative(path, &spec);
        let dst = match &target_file {
            Some(f) => file_id(id.repo, f),
            None => format!("mod:{spec}"),
        };
        out.edges.push(Edge {
            src: file_id(id.repo, path),
            dst: dst.clone(),
            label: "IMPORTS".into(),
            props: serde_json::json!({
                "specifier": spec,
                "prov": cx.prov(&stmt, &format!("IMPORTS {path} -> {spec}")),
            }),
        });
        // Bind imported names for deterministic cross-file call/handler edges.
        if let Some(clause) = clause {
            let mut walker = clause.walk();
            for child in clause.children(&mut walker) {
                match child.kind() {
                    // `import express from 'express'` — default import.
                    "identifier" => {
                        import_modules.insert(cx.text(&child).to_string(), spec.clone());
                    }
                    "named_imports" => {
                        let mut w2 = child.walk();
                        for s in child.children(&mut w2) {
                            if s.kind() != "import_specifier" {
                                continue;
                            }
                            let Some(name) = s.child_by_field_name("name") else {
                                continue;
                            };
                            // `import { a as b }` binds `b` locally.
                            let local = s.child_by_field_name("alias").unwrap_or(name);
                            if let Some(f) = &target_file {
                                imported.insert(
                                    cx.text(&local).to_string(),
                                    sym_id(id.repo, f, cx.text(&name)),
                                );
                            } else {
                                import_modules.insert(cx.text(&local).to_string(), spec.clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // --- Framework receivers: vars bound to express() / express.Router() ----
    let q_factories = Query::new(
        &language,
        r#"
        (variable_declarator
            name: (identifier) @var
            value: (call_expression function: [(identifier) (member_expression)] @callee))
        "#,
    )
    .expect("static query");
    let mut routers: HashMap<String, ()> = HashMap::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q_factories, root, source);
    while let Some(m) = matches.next() {
        let mut var = None;
        let mut callee = None;
        for c in m.captures {
            match q_factories.capture_names()[c.index as usize] {
                "var" => var = Some(cx.text(&c.node).to_string()),
                "callee" => callee = Some(cx.text(&c.node).to_string()),
                _ => {}
            }
        }
        let (Some(var), Some(callee)) = (var, callee) else {
            continue;
        };
        // Only when the factory name is imported from the framework module.
        let base = callee.split('.').next().unwrap_or(&callee);
        let from_framework =
            import_modules.get(base).map(String::as_str) == Some(EXPRESS.module_name);
        if from_framework && EXPRESS.is_factory(&callee) {
            routers.insert(var, ());
        }
    }

    // --- Endpoints: router.<verb>('/route', handler) -------------------------
    let q_endpoints = Query::new(
        &language,
        r#"
        (call_expression
            function: (member_expression
                object: (identifier) @recv
                property: (property_identifier) @method)
            arguments: (arguments . (string (string_fragment) @route))) @call
        "#,
    )
    .expect("static query");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q_endpoints, root, source);
    while let Some(m) = matches.next() {
        let mut recv = None;
        let mut method = None;
        let mut route = None;
        let mut call = None;
        for c in m.captures {
            match q_endpoints.capture_names()[c.index as usize] {
                "recv" => recv = Some(cx.text(&c.node).to_string()),
                "method" => method = Some(cx.text(&c.node).to_string()),
                "route" => route = Some(cx.text(&c.node).to_string()),
                "call" => call = Some(c.node),
                _ => {}
            }
        }
        let (Some(recv), Some(method), Some(route), Some(call)) = (recv, method, route, call)
        else {
            continue;
        };
        if !routers.contains_key(&recv) {
            continue; // receiver not proven to be a framework object -> not T0
        }
        let Some(verb) = EXPRESS.http_method(&method) else {
            continue;
        };
        let ep_id = format!("ep:{}@{verb}:{route}", id.repo);
        out.nodes.push(Node {
            id: ep_id.clone(),
            label: "Endpoint".into(),
            props: serde_json::json!({
                "method": verb, "path": route,
                "prov": cx.prov(&call, &format!("Endpoint {verb} {route}")),
            }),
        });
        // Handler: last argument — identifier (local or imported) or inline fn.
        let handler_sym = call
            .child_by_field_name("arguments")
            .and_then(|args| {
                let mut w = args.walk();
                let children: Vec<_> =
                    args.children(&mut w).filter(|c| c.is_named()).collect();
                children.last().copied()
            })
            .and_then(|h| match h.kind() {
                "identifier" => {
                    let name = cx.text(&h);
                    locals.get(name).cloned().or_else(|| imported.get(name).cloned())
                }
                "arrow_function" | "function_expression" => {
                    let sid = sym_id(id.repo, path, &format!("anon@{}", h.start_byte()));
                    out.nodes.push(Node {
                        id: sid.clone(),
                        label: "Symbol".into(),
                        props: serde_json::json!({
                            "name": format!("<handler {verb} {route}>"),
                            "kind": "Function",
                            "prov": cx.prov(&h, &format!("Symbol {sid}")),
                        }),
                    });
                    out.edges.push(Edge {
                        src: sid.clone(),
                        dst: file_id(id.repo, path),
                        label: "DEFINED_IN".into(),
                        props: serde_json::json!({ "prov": cx.prov(&h, &format!("DEFINED_IN {sid}")) }),
                    });
                    Some(sid)
                }
                _ => None,
            });
        if let Some(handler) = handler_sym {
            out.edges.push(Edge {
                src: ep_id,
                dst: handler,
                label: "HANDLES".into(),
                props: serde_json::json!({ "prov": cx.prov(&call, &format!("HANDLES {verb} {route}")) }),
            });
        }
    }

    // --- Event sites: SDK producer/consumer calls (US-0004) ------------------
    // Receiver proof mirrors the endpoint extractor: a site only matches when
    // its constructor/receiver provably comes from the SDK module — never
    // guessed from variable names.

    // Local `const X = 'literal'` bindings resolve identifiers to literals
    // at T0 (same-file, deterministic). Only `const` counts: `let`/`var`
    // bindings are reassignable, so promoting them would stamp Confirmed on
    // a runtime value — those stay computed and escalate (AC-0012).
    let q_consts = Query::new(
        &language,
        r#"
        (variable_declarator
            name: (identifier) @name
            value: (string (string_fragment) @lit)) @decl
        "#,
    )
    .expect("static query");
    let mut const_strings: HashMap<String, String> = HashMap::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q_consts, root, source);
    while let Some(m) = matches.next() {
        let (mut name, mut lit, mut decl) = (None, None, None);
        for c in m.captures {
            match q_consts.capture_names()[c.index as usize] {
                "name" => name = Some(cx.text(&c.node).to_string()),
                "lit" => lit = Some(cx.text(&c.node).to_string()),
                "decl" => decl = Some(c.node),
                _ => {}
            }
        }
        let is_const = decl
            .and_then(|d| d.parent())
            .filter(|p| p.kind() == "lexical_declaration")
            .and_then(|p| p.child(0))
            .is_some_and(|kw| kw.kind() == "const");
        if let (Some(name), Some(lit), true) = (name, lit, is_const) {
            const_strings.insert(name, lit);
        }
    }

    // Vars bound to `new Ctor(...)` — receiver proof for Method patterns.
    let q_news = Query::new(
        &language,
        r#"
        (variable_declarator
            name: (identifier) @var
            value: (new_expression
                constructor: [(identifier) (member_expression)] @ctor))
        "#,
    )
    .expect("static query");
    let mut constructed: HashMap<String, String> = HashMap::new(); // var -> ctor text
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q_news, root, source);
    while let Some(m) = matches.next() {
        let (mut var, mut ctor) = (None, None);
        for c in m.captures {
            match q_news.capture_names()[c.index as usize] {
                "var" => var = Some(cx.text(&c.node).to_string()),
                "ctor" => ctor = Some(cx.text(&c.node).to_string()),
                _ => {}
            }
        }
        if let (Some(var), Some(ctor)) = (var, ctor) {
            constructed.insert(var, ctor);
        }
    }

    // Vars bound to `obj.factory()` where `obj` was constructed from the SDK
    // module — receiver proof for FactoryMethod patterns (kafkajs).
    let q_factory_recv = Query::new(
        &language,
        r#"
        (variable_declarator
            name: (identifier) @var
            value: (call_expression
                function: (member_expression
                    object: (identifier) @obj
                    property: (property_identifier) @factory)))
        "#,
    )
    .expect("static query");
    // var -> (module of obj's constructor, factory name)
    let mut factory_receivers: HashMap<String, (String, String)> = HashMap::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q_factory_recv, root, source);
    while let Some(m) = matches.next() {
        let (mut var, mut obj, mut factory) = (None, None, None);
        for c in m.captures {
            match q_factory_recv.capture_names()[c.index as usize] {
                "var" => var = Some(cx.text(&c.node).to_string()),
                "obj" => obj = Some(cx.text(&c.node).to_string()),
                "factory" => factory = Some(cx.text(&c.node).to_string()),
                _ => {}
            }
        }
        let (Some(var), Some(obj), Some(factory)) = (var, obj, factory) else {
            continue;
        };
        // Chain of proof: obj <- new Ctor(...), Ctor imported from module.
        if let Some(ctor) = constructed.get(&obj)
            && let Some(module) = ctor_module(ctor, &import_modules)
        {
            factory_receivers.insert(var, (module, factory));
        }
    }

    let classify = |expr: &TsNode| -> IdentityExpr { classify_identity(&cx, expr, &const_strings) };
    let push_site = |out: &mut Extraction,
                     kind: &str,
                     role: ChannelRole,
                     identity: IdentityExpr,
                     site: &TsNode| {
        out.event_sites.push(EventSite {
            kind: kind.into(),
            role,
            identity,
            symbol: enclosing_symbol(&cx, *site),
            path: path.into(),
            byte_start: site.start_byte() as u64,
            byte_end: site.end_byte() as u64,
        });
    };

    // Constructor pattern: `new SendMessageCommand({ QueueUrl: … })`.
    let q_cmd = Query::new(
        &language,
        r#"
        (new_expression
            constructor: (identifier) @ctor
            arguments: (arguments) @args) @new
        "#,
    )
    .expect("static query");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q_cmd, root, source);
    while let Some(m) = matches.next() {
        let (mut ctor, mut args, mut site) = (None, None, None);
        for c in m.captures {
            match q_cmd.capture_names()[c.index as usize] {
                "ctor" => ctor = Some(cx.text(&c.node).to_string()),
                "args" => args = Some(c.node),
                "new" => site = Some(c.node),
                _ => {}
            }
        }
        let (Some(ctor), Some(args), Some(site)) = (ctor, args, site) else {
            continue;
        };
        let from = import_modules.get(&ctor).map(|m| norm_module(m));
        for sdk in EVENT_SDKS {
            let SdkPattern::Constructor { module, ctor: c } = sdk.pattern else {
                continue;
            };
            if ctor != c || from.as_deref() != Some(module) {
                continue;
            }
            let identity = identity_in_args(&cx, &args, sdk.identity, &classify);
            push_site(&mut out, sdk.kind, sdk.role, identity, &site);
        }
    }

    // Method + FactoryMethod patterns: `recv.method(…)` with proven receiver.
    let q_member_calls = Query::new(
        &language,
        r#"
        (call_expression
            function: (member_expression
                object: (identifier) @recv
                property: (property_identifier) @method)
            arguments: (arguments) @args) @call
        "#,
    )
    .expect("static query");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q_member_calls, root, source);
    while let Some(m) = matches.next() {
        let (mut recv, mut method, mut args, mut site) = (None, None, None, None);
        for c in m.captures {
            match q_member_calls.capture_names()[c.index as usize] {
                "recv" => recv = Some(cx.text(&c.node).to_string()),
                "method" => method = Some(cx.text(&c.node).to_string()),
                "args" => args = Some(c.node),
                "call" => site = Some(c.node),
                _ => {}
            }
        }
        let (Some(recv), Some(method), Some(args), Some(site)) = (recv, method, args, site) else {
            continue;
        };
        for sdk in EVENT_SDKS {
            let matched = match sdk.pattern {
                SdkPattern::Method {
                    module,
                    ctor,
                    method: m,
                } => {
                    method == m
                        && constructed.get(&recv).map(String::as_str) == Some(ctor)
                        && ctor_module(ctor, &import_modules).as_deref() == Some(module)
                }
                SdkPattern::FactoryMethod {
                    module,
                    factory,
                    method: m,
                } => {
                    method == m
                        && factory_receivers
                            .get(&recv)
                            .map(|(mo, f)| (mo.as_str(), f.as_str()))
                            == Some((module, factory))
                }
                SdkPattern::Constructor { .. } => false,
            };
            if matched {
                let identity = identity_in_args(&cx, &args, sdk.identity, &classify);
                push_site(&mut out, sdk.kind, sdk.role, identity, &site);
            }
        }
    }

    // --- Client side (US-0005, .tsx only): screens, renders, fetch sites ----
    if is_tsx {
        // JSX usage: <Comp/> inside a component body -> RENDERS edge. Only
        // capitalized names resolved through locals/imports count — plain
        // HTML tags and unproven names are skipped.
        let q_jsx = Query::new(
            &language,
            r#"
            [
                (jsx_self_closing_element name: (identifier) @tag) @el
                (jsx_opening_element name: (identifier) @tag) @el
            ]
            "#,
        )
        .expect("static query");
        let mut renders: Vec<(String, String, TsNode)> = Vec::new();
        let mut route_elements: Vec<TsNode> = Vec::new();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&q_jsx, root, source);
        while let Some(m) = matches.next() {
            let (mut tag, mut el) = (None, None);
            for c in m.captures {
                match q_jsx.capture_names()[c.index as usize] {
                    "tag" => tag = Some(c.node),
                    "el" => el = Some(c.node),
                    _ => {}
                }
            }
            let (Some(tag), Some(el)) = (tag, el) else {
                continue;
            };
            let name = cx.text(&tag);
            if !name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                continue;
            }
            // Router registration is not a render relationship.
            if name == REACT_ROUTER.route_component
                && import_modules
                    .get(name)
                    .is_some_and(|m| REACT_ROUTER.modules.contains(&m.as_str()))
            {
                route_elements.push(el);
                continue;
            }
            let Some(dst) = locals
                .get(name)
                .cloned()
                .or_else(|| imported.get(name).cloned())
            else {
                continue;
            };
            if let Some(src) = enclosing_symbol(&cx, el)
                && src != dst
            {
                renders.push((src, dst, el));
            }
        }
        let mut seen_renders = std::collections::HashSet::new();
        for (src, dst, el) in renders {
            if seen_renders.insert((src.clone(), dst.clone())) {
                out.edges.push(Edge {
                    src: src.clone(),
                    dst: dst.clone(),
                    label: "RENDERS".into(),
                    props: serde_json::json!({
                        "prov": cx.prov(&el, &format!("RENDERS {src} -> {dst}")),
                    }),
                });
            }
        }

        // React Router screens: <Route path="/x" element={<Comp/>}/> with
        // Route import-proven. The screen renders the routed component.
        for el in route_elements {
            let mut route_path = None;
            let mut element_comp = None;
            let mut w = el.walk();
            for attr in el.children(&mut w).filter(|c| c.kind() == "jsx_attribute") {
                let Some(attr_name) = attr.child(0) else {
                    continue;
                };
                match cx.text(&attr_name) {
                    "path" => {
                        // path="/x" — a string attribute value.
                        if let Some(v) = attr.child(2)
                            && v.kind() == "string"
                        {
                            route_path = Some(cx.text(&v).trim_matches(['"', '\'']).to_string());
                        }
                    }
                    "element" => {
                        // element={<Comp/>} — find the JSX name inside.
                        let mut stack = vec![attr];
                        while let Some(n) = stack.pop() {
                            if matches!(
                                n.kind(),
                                "jsx_self_closing_element" | "jsx_opening_element"
                            ) && let Some(name) = n.child_by_field_name("name")
                            {
                                element_comp = locals
                                    .get(cx.text(&name))
                                    .cloned()
                                    .or_else(|| imported.get(cx.text(&name)).cloned());
                                break;
                            }
                            let mut w2 = n.walk();
                            let children: Vec<_> = n.children(&mut w2).collect();
                            stack.extend(children.into_iter().rev());
                        }
                    }
                    _ => {}
                }
            }
            let Some(route) = route_path else {
                continue;
            };
            let screen_id = format!("screen:{}@{route}", id.repo);
            out.nodes.push(Node {
                id: screen_id.clone(),
                label: "Screen".into(),
                props: serde_json::json!({
                    "route": route,
                    "router": "react-router",
                    "prov": cx.prov(&el, &format!("Screen {route}")),
                }),
            });
            if let Some(comp) = element_comp {
                out.edges.push(Edge {
                    src: screen_id.clone(),
                    dst: comp,
                    label: "RENDERS".into(),
                    props: serde_json::json!({
                        "prov": cx.prov(&el, &format!("RENDERS {screen_id}")),
                    }),
                });
            }
        }

        // Fetch sites: fetch(url, {method}) and import-proven axios calls.
        // The URL classifies exactly like a channel identity (AC-0014).
        // Only a *direct* property of the options object is the HTTP method —
        // a nested `headers: { method: … }` or `data: { method: … }` is not.
        let top_level_key = |obj: &TsNode, key: &str| -> Option<IdentityExpr> {
            if obj.kind() != "object" {
                return None;
            }
            let mut w = obj.walk();
            for pair in obj.children(&mut w).filter(|c| c.kind() == "pair") {
                if let Some(k) = pair.child_by_field_name("key")
                    && cx.text(&k).trim_matches(['"', '\'']) == key
                    && let Some(v) = pair.child_by_field_name("value")
                {
                    return Some(classify_identity(&cx, &v, &const_strings));
                }
            }
            None
        };
        let method_in_args = |args: &TsNode, position: usize| -> Option<String> {
            let mut w = args.walk();
            let arg = args
                .children(&mut w)
                .filter(|c| c.is_named())
                .nth(position)?;
            match top_level_key(&arg, "method")? {
                IdentityExpr::Literal(m) => Some(m.to_ascii_uppercase()),
                _ => Some("?".into()),
            }
        };
        let first_arg_classified = |args: &TsNode| -> Option<IdentityExpr> {
            let mut w = args.walk();
            args.children(&mut w)
                .find(|c| c.is_named())
                .map(|a| classify_identity(&cx, &a, &const_strings))
        };
        let push_fetch =
            |out: &mut Extraction, method: String, url: IdentityExpr, site: &TsNode| {
                out.fetch_sites.push(FetchSite {
                    method,
                    url,
                    symbol: enclosing_component(&cx, *site)
                        .or_else(|| enclosing_symbol(&cx, *site)),
                    path: path.into(),
                    byte_start: site.start_byte() as u64,
                    byte_end: site.end_byte() as u64,
                });
            };

        // fetch(url, opts?) — the browser global.
        let q_fetch = Query::new(
            &language,
            r#"(call_expression function: (identifier) @fn arguments: (arguments) @args) @call"#,
        )
        .expect("static query");
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&q_fetch, root, source);
        while let Some(m) = matches.next() {
            let (mut fn_name, mut args, mut call) = (None, None, None);
            for c in m.captures {
                match q_fetch.capture_names()[c.index as usize] {
                    "fn" => fn_name = Some(cx.text(&c.node).to_string()),
                    "args" => args = Some(c.node),
                    "call" => call = Some(c.node),
                    _ => {}
                }
            }
            let (Some(fn_name), Some(args), Some(call)) = (fn_name, args, call) else {
                continue;
            };
            // A locally defined or imported `fetch` is application code, not
            // the browser API — confirming it against an endpoint would
            // corrupt the graph.
            let fetch_shadowed = locals.contains_key("fetch")
                || imported.contains_key("fetch")
                || import_modules.contains_key("fetch");
            if fn_name == "fetch" && !fetch_shadowed {
                let Some(url) = first_arg_classified(&args) else {
                    continue;
                };
                let method = method_in_args(&args, 1).unwrap_or_else(|| "GET".into());
                push_fetch(&mut out, method, url, &call);
            } else if fn_name == "axios"
                && import_modules.get("axios").map(String::as_str) == Some("axios")
            {
                // axios({ url, method }) object form — top-level keys only.
                let mut w = args.walk();
                let Some(first) = args.children(&mut w).find(|c| c.is_named()) else {
                    continue;
                };
                let Some(url) = top_level_key(&first, "url") else {
                    continue;
                };
                let method = method_in_args(&args, 0).unwrap_or_else(|| "GET".into());
                push_fetch(&mut out, method, url, &call);
            }
        }

        // axios.get/post/… (url) — member form, import-proven.
        let q_axios = Query::new(
            &language,
            r#"
            (call_expression
                function: (member_expression
                    object: (identifier) @obj
                    property: (property_identifier) @method)
                arguments: (arguments) @args) @call
            "#,
        )
        .expect("static query");
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&q_axios, root, source);
        while let Some(m) = matches.next() {
            let (mut obj, mut method, mut args, mut call) = (None, None, None, None);
            for c in m.captures {
                match q_axios.capture_names()[c.index as usize] {
                    "obj" => obj = Some(cx.text(&c.node).to_string()),
                    "method" => method = Some(cx.text(&c.node).to_string()),
                    "args" => args = Some(c.node),
                    "call" => call = Some(c.node),
                    _ => {}
                }
            }
            let (Some(obj), Some(method), Some(args), Some(call)) = (obj, method, args, call)
            else {
                continue;
            };
            if obj != "axios" || import_modules.get("axios").map(String::as_str) != Some("axios") {
                continue;
            }
            if !["get", "post", "put", "delete", "patch", "head"].contains(&method.as_str()) {
                continue;
            }
            let Some(url) = first_arg_classified(&args) else {
                continue;
            };
            push_fetch(&mut out, method.to_ascii_uppercase(), url, &call);
        }

        // Default export (Next.js pages resolve their screen through it).
        let mut w = root.walk();
        for stmt in root
            .children(&mut w)
            .filter(|c| c.kind() == "export_statement")
        {
            let mut has_default = false;
            let mut w2 = stmt.walk();
            for child in stmt.children(&mut w2) {
                if child.kind() == "default" {
                    has_default = true;
                }
            }
            if !has_default {
                continue;
            }
            let target = stmt
                .child_by_field_name("declaration")
                .and_then(|d| d.child_by_field_name("name"))
                .map(|n| sym_id(id.repo, path, cx.text(&n)))
                .or_else(|| {
                    stmt.child_by_field_name("value")
                        .filter(|v| v.kind() == "identifier")
                        .and_then(|v| {
                            locals
                                .get(cx.text(&v))
                                .cloned()
                                .or_else(|| imported.get(cx.text(&v)).cloned())
                        })
                });
            if let Some(target) = target {
                out.default_exports.insert(path.to_string(), target);
            }
        }
    }

    // --- Calls: caller symbol -> callee symbol (local or import-bound) ------
    let q_calls = Query::new(
        &language,
        r#"(call_expression function: (identifier) @callee) @call"#,
    )
    .expect("static query");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q_calls, root, source);
    while let Some(m) = matches.next() {
        let mut callee_node = None;
        let mut call = None;
        for c in m.captures {
            match q_calls.capture_names()[c.index as usize] {
                "callee" => callee_node = Some(c.node),
                "call" => call = Some(c.node),
                _ => {}
            }
        }
        let (Some(callee_node), Some(call)) = (callee_node, call) else {
            continue;
        };
        let callee_name = cx.text(&callee_node);
        let Some(dst) = locals
            .get(callee_name)
            .cloned()
            .or_else(|| imported.get(callee_name).cloned())
        else {
            continue; // unknown callee (global, builtin) — not resolvable at T0
        };
        let Some(src_sym) = enclosing_symbol(&cx, call) else {
            continue; // top-level statement, not a symbol-to-symbol call
        };
        if src_sym == dst {
            continue; // direct recursion adds no path information at M1
        }
        out.edges.push(Edge {
            src: src_sym,
            dst,
            label: "CALLS".into(),
            props: serde_json::json!({
                "prov": cx.prov(&call, &format!("CALLS {} at {}", callee_name, call.start_byte())),
            }),
        });
    }

    Ok(out)
}

/// Extract facts from every `.ts`/`.tsx` file under `root` (skipping
/// `node_modules`, `dist`, hidden dirs, and `.d.ts` declarations), with edge
/// endpoints closed over placeholders.
pub fn extract_dir(root: &Path, id: &SourceId) -> Result<Extraction, ExtractError> {
    let mut files = Vec::new();
    collect_ts_files(root, root, &mut files)?;
    files.sort(); // deterministic order (US-0014)
    let mut out = Extraction::default();
    for rel in &files {
        let source = std::fs::read(root.join(rel))?;
        let ex = extract_source(&source, rel, id)?;
        out.nodes.extend(ex.nodes);
        out.edges.extend(ex.edges);
        out.event_sites.extend(ex.event_sites);
        out.fetch_sites.extend(ex.fetch_sites);
        out.default_exports.extend(ex.default_exports);
    }
    next_pages_screens(&mut out, id);
    out.close_over_endpoints();
    Ok(out)
}

/// Next.js pages-router convention (SPEC-00 §3.5): a `.tsx` file under a
/// `pages/` directory *is* a screen — `pages/users/[id].tsx` → route
/// `/users/[id]`, rendering the file's default export. File-structural,
/// so it runs over the whole walk rather than per file.
fn next_pages_screens(out: &mut Extraction, id: &SourceId) {
    let mut screens = Vec::new();
    for node in &out.nodes {
        if node.label != "File" {
            continue;
        }
        let Some(path) = node.props["path"].as_str() else {
            continue;
        };
        let Some(idx) = path
            .strip_prefix(&format!("{NEXT_PAGES_DIR}/"))
            .map(|r| (0, r))
            .or_else(|| {
                path.find(&format!("/{NEXT_PAGES_DIR}/"))
                    .map(|i| (i + 1, &path[i + 1 + NEXT_PAGES_DIR.len() + 1..]))
            })
        else {
            continue;
        };
        let (_, rel) = idx;
        if !path.ends_with(".tsx") || rel.starts_with('_') {
            continue; // _app.tsx/_document.tsx are chrome, not screens.
        }
        let mut route = format!("/{}", rel.trim_end_matches(".tsx"));
        if route.ends_with("/index") || route == "/index" {
            route = route
                .trim_end_matches("index")
                .trim_end_matches('/')
                .to_string();
            if route.is_empty() {
                route = "/".into();
            }
        }
        let screen_id = format!("screen:{}@{route}", id.repo);
        let prov = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![EvidenceRef {
                repo: id.repo.into(),
                path: path.into(),
                byte_start: 0,
                byte_end: 0,
                commit_sha: id.commit.into(),
            }],
            EXTRACTOR_ID,
            format!("Screen {route}").as_bytes(),
        )
        .expect("Deterministic/Confirmed is always within ceiling");
        screens.push((screen_id, route, path.to_string(), prov));
    }
    for (screen_id, route, path, prov) in screens {
        out.nodes.push(Node {
            id: screen_id.clone(),
            label: "Screen".into(),
            props: serde_json::json!({
                "route": route,
                "router": "next-pages",
                "prov": serde_json::to_value(prov).expect("provenance serializes"),
            }),
        });
        if let Some(comp) = out.default_exports.get(&path).cloned() {
            let edge_prov = Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
                vec![EvidenceRef {
                    repo: id.repo.into(),
                    path: path.clone(),
                    byte_start: 0,
                    byte_end: 0,
                    commit_sha: id.commit.into(),
                }],
                EXTRACTOR_ID,
                format!("RENDERS {screen_id} -> {comp}").as_bytes(),
            )
            .expect("within ceiling");
            out.edges.push(Edge {
                src: screen_id,
                dst: comp,
                label: "RENDERS".into(),
                props: serde_json::json!({
                    "prov": serde_json::to_value(edge_prov).expect("serializes"),
                }),
            });
        }
    }
}

fn collect_ts_files(root: &Path, dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name == "node_modules" || name == "dist" || name.starts_with('.') {
                continue;
            }
            collect_ts_files(root, &path, out)?;
        } else if (name.ends_with(".ts") || name.ends_with(".tsx")) && !name.ends_with(".d.ts") {
            let rel = path
                .strip_prefix(root)
                .expect("entry under root")
                .to_string_lossy()
                .replace('\\', "/");
            out.push(rel);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
