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

fn file_id(path: &str) -> String {
    format!("file:{path}")
}

fn sym_id(path: &str, name: &str) -> String {
    format!("sym:{path}#{name}")
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
                return Some(sym_id(cx.path, cx.text(&name)));
            }
            "arrow_function" | "function_expression" => {
                // Named via `const f = () => {}`?
                if let Some(decl) = parent
                    .parent()
                    .filter(|p| p.kind() == "variable_declarator")
                    && let Some(name) = decl.child_by_field_name("name")
                {
                    return Some(sym_id(cx.path, cx.text(&name)));
                }
                // Anonymous (e.g. inline route handler): stable offset-keyed id,
                // shared with the endpoint extractor.
                return Some(sym_id(cx.path, &format!("anon@{}", parent.start_byte())));
            }
            _ => node = parent,
        }
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
    let language: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
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
        id: file_id(path),
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
        let sid = sym_id(path, &name);
        out.nodes.push(Node {
            id: sid.clone(),
            label: "Symbol".into(),
            props: serde_json::json!({
                "name": name,
                "kind": "Function",
                "prov": cx.prov(&def_node, &format!("Symbol {sid}")),
            }),
        });
        out.edges.push(Edge {
            src: sid.clone(),
            dst: file_id(path),
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
            Some(f) => file_id(f),
            None => format!("mod:{spec}"),
        };
        out.edges.push(Edge {
            src: file_id(path),
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
                                imported
                                    .insert(cx.text(&local).to_string(), sym_id(f, cx.text(&name)));
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
        let ep_id = format!("ep:{verb}:{route}");
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
                    let sid = sym_id(path, &format!("anon@{}", h.start_byte()));
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
                        dst: file_id(path),
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
    // at T0 (same-file, deterministic).
    let q_consts = Query::new(
        &language,
        r#"
        (variable_declarator
            name: (identifier) @name
            value: (string (string_fragment) @lit))
        "#,
    )
    .expect("static query");
    let mut const_strings: HashMap<String, String> = HashMap::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q_consts, root, source);
    while let Some(m) = matches.next() {
        let (mut name, mut lit) = (None, None);
        for c in m.captures {
            match q_consts.capture_names()[c.index as usize] {
                "name" => name = Some(cx.text(&c.node).to_string()),
                "lit" => lit = Some(cx.text(&c.node).to_string()),
                _ => {}
            }
        }
        if let (Some(name), Some(lit)) = (name, lit) {
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
    }
    out.close_over_endpoints();
    Ok(out)
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
