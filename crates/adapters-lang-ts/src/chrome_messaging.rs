//! Chrome runtime messaging (US-0016/AC-0072): deterministic T0
//! producer/consumer sites for `chrome.runtime`/`chrome.tabs` messaging.
//!
//! Producers are `chrome.runtime.sendMessage(msg)` / `chrome.tabs.sendMessage(tab, msg)`
//! call sites — the receiver is the platform global, so the guard is the
//! exact callee text plus a shadow check, mirroring the `fetch` rule.
//! Message identity is the `type` property of the message object, resolved
//! deterministically through (a) string literals, (b) member access into a
//! repo-wide const object of string literals (`MessageType.Ping`), or (c) a
//! one-hop call to a creator function whose returned object literal carries
//! a resolvable `type`. Anything else stays `Computed` and the events crate
//! records an explicit Gap (AC-0012, R-INT-4) — never a guess.
//!
//! Consumers are explicit handler registrations: object-literal dispatch
//! tables whose computed keys (`[MessageType.LoadAlbums]: handler`) resolve
//! through the same const index, gated on the repo actually registering a
//! `chrome.runtime.onMessage.addListener`. A repo whose only consumer
//! evidence is the listener itself gets a single `Computed` site — the
//! dynamic dispatch surface is recorded, not invented.

use adapters_fw::events::{ChannelRole, EventSite, IdentityExpr};
use std::collections::BTreeMap;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node as TsNode, Parser, Query, QueryCursor};

use crate::{ExtractError, FileCx, SourceId, enclosing_symbol, literal_string, object_entries};

/// Channel kind for the events registry (`chan:chrome-message:<type>`).
pub const CHANNEL_KIND: &str = "chrome-message";

/// A message identity before repo-wide resolution.
#[derive(Debug, Clone)]
enum Pending {
    /// A string literal — already resolved.
    Literal(String),
    /// `Object.Member` — resolve against the repo-wide const-string index.
    Member(String),
    /// `createX(…)` — resolve through the creator's returned `type`.
    Creator(String),
    /// Not statically visible; carries the raw source text.
    Computed(String),
}

struct PendingSite {
    role: ChannelRole,
    identity: Pending,
    symbol: Option<String>,
    path: String,
    byte_start: u64,
    byte_end: u64,
}

#[derive(Default)]
struct RepoIndex {
    /// `Object.Member` → literal; `None` marks a cross-file conflict, which
    /// deterministically refuses to resolve rather than picking a winner.
    const_members: BTreeMap<String, Option<String>>,
    /// Creator function name → the `type` its returned object declares.
    creators: BTreeMap<String, Option<Pending>>,
    sites: Vec<PendingSite>,
    /// Dispatch-table keys awaiting the listener gate + const resolution.
    dispatch_keys: Vec<PendingSite>,
    listeners: Vec<PendingSite>,
}

fn insert_unique<T: Clone + PartialEq>(
    map: &mut BTreeMap<String, Option<T>>,
    key: String,
    value: T,
) {
    match map.get(&key) {
        None => {
            map.insert(key, Some(value));
        }
        Some(Some(existing)) if *existing == value => {}
        _ => {
            map.insert(key, None); // conflict: refuse to resolve
        }
    }
}

impl PartialEq for Pending {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Pending::Literal(a), Pending::Literal(b))
            | (Pending::Member(a), Pending::Member(b)) => a == b,
            _ => false,
        }
    }
}

/// Unwrap `as const` / `satisfies` wrappers down to the inner expression.
fn unwrap_assertions<'t>(node: TsNode<'t>) -> TsNode<'t> {
    let mut current = node;
    while matches!(current.kind(), "as_expression" | "satisfies_expression") {
        let Some(inner) = current.named_child(0) else {
            return current;
        };
        current = inner;
    }
    current
}

/// The `type` property's identity inside a message object literal.
fn type_of_object(cx: &FileCx, object: TsNode) -> Pending {
    for (key, value) in object_entries(cx, object) {
        if key != "type" {
            continue;
        }
        if let Some(lit) = literal_string(cx, value) {
            return Pending::Literal(lit);
        }
        if value.kind() == "member_expression" {
            return Pending::Member(cx.text(&value).to_string());
        }
        return Pending::Computed(cx.text(&value).to_string());
    }
    Pending::Computed(cx.text(&object).to_string())
}

/// Classify one sendMessage message argument.
fn classify_message(cx: &FileCx, arg: TsNode) -> Pending {
    let arg = unwrap_assertions(arg);
    match arg.kind() {
        "object" => type_of_object(cx, arg),
        "call_expression" => {
            let callee = arg.child_by_field_name("function");
            match callee {
                Some(callee) if callee.kind() == "identifier" => {
                    Pending::Creator(cx.text(&callee).to_string())
                }
                _ => Pending::Computed(cx.text(&arg).to_string()),
            }
        }
        _ => Pending::Computed(cx.text(&arg).to_string()),
    }
}

fn resolve(index: &RepoIndex, pending: &Pending) -> IdentityExpr {
    match pending {
        Pending::Literal(value) => IdentityExpr::Literal(value.clone()),
        Pending::Member(member) => match index.const_members.get(member) {
            Some(Some(value)) => IdentityExpr::Literal(value.clone()),
            _ => IdentityExpr::Computed(member.clone()),
        },
        Pending::Creator(name) => match index.creators.get(name) {
            Some(Some(inner @ (Pending::Literal(_) | Pending::Member(_)))) => {
                match resolve(index, inner) {
                    IdentityExpr::Literal(value) => IdentityExpr::Literal(value),
                    _ => IdentityExpr::Computed(format!("{name}(…)")),
                }
            }
            _ => IdentityExpr::Computed(format!("{name}(…)")),
        },
        Pending::Computed(raw) => IdentityExpr::Computed(raw.clone()),
    }
}

fn extract_file(
    source: &[u8],
    path: &str,
    id: &SourceId,
    index: &mut RepoIndex,
) -> Result<(), ExtractError> {
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

    // A locally bound `chrome` is application code, not the platform API —
    // same fail-closed rule as the shadowed-`fetch` guard.
    let q_decls = Query::new(
        &language,
        r#"
        (variable_declarator name: (identifier) @name value: (_) @value)
        (import_specifier name: (identifier) @import)
        (import_clause (identifier) @import)
        "#,
    )
    .expect("static query");
    let mut chrome_shadowed = false;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q_decls, root, source);
    while let Some(m) = matches.next() {
        let (mut name, mut value) = (None, None);
        for c in m.captures {
            match q_decls.capture_names()[c.index as usize] {
                "name" => name = Some(c.node),
                "value" => value = Some(c.node),
                "import" if cx.text(&c.node) == "chrome" => chrome_shadowed = true,
                _ => {}
            }
        }
        let (Some(name), Some(value)) = (name, value) else {
            continue;
        };
        let name_text = cx.text(&name).to_string();
        if name_text == "chrome" {
            chrome_shadowed = true;
        }
        // Const-string object maps feed the repo-wide member index.
        let value = unwrap_assertions(value);
        if value.kind() == "object" {
            for (key, entry) in object_entries(&cx, value) {
                if let Some(lit) = literal_string(&cx, entry) {
                    insert_unique(&mut index.const_members, format!("{name_text}.{key}"), lit);
                }
            }
        }
        // Arrow-const creators: `const createPing = () => ({ type: … })`.
        if value.kind() == "arrow_function"
            && let Some(body) = value.child_by_field_name("body")
        {
            let body = if body.kind() == "parenthesized_expression" {
                body.named_child(0).unwrap_or(body)
            } else {
                body
            };
            record_creator(&cx, index, &name_text, body);
        }
    }

    // Function-declaration creators: `function createPing() { return { type } }`.
    let q_funcs = Query::new(
        &language,
        r#"(function_declaration name: (identifier) @name body: (statement_block) @body)"#,
    )
    .expect("static query");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q_funcs, root, source);
    while let Some(m) = matches.next() {
        let (mut name, mut body) = (None, None);
        for c in m.captures {
            match q_funcs.capture_names()[c.index as usize] {
                "name" => name = Some(cx.text(&c.node).to_string()),
                "body" => body = Some(c.node),
                _ => {}
            }
        }
        let (Some(name), Some(body)) = (name, body) else {
            continue;
        };
        let mut stack = vec![body];
        while let Some(node) = stack.pop() {
            if node.kind() == "return_statement" {
                if let Some(value) = node.named_child(0) {
                    let value = unwrap_assertions(value);
                    if value.kind() == "object" {
                        record_creator(&cx, index, &name, value);
                    }
                }
                continue;
            }
            // Nested functions declare their own returns, not this one's.
            if matches!(node.kind(), "arrow_function" | "function_expression") {
                continue;
            }
            let mut walk = node.walk();
            stack.extend(node.named_children(&mut walk));
        }
    }

    // Producer, listener, and dispatch-table sites.
    let q_calls = Query::new(
        &language,
        r#"(call_expression function: (member_expression) @callee arguments: (arguments) @args) @call"#,
    )
    .expect("static query");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q_calls, root, source);
    while let Some(m) = matches.next() {
        let (mut callee, mut args, mut call) = (None, None, None);
        for c in m.captures {
            match q_calls.capture_names()[c.index as usize] {
                "callee" => callee = Some(c.node),
                "args" => args = Some(c.node),
                "call" => call = Some(c.node),
                _ => {}
            }
        }
        let (Some(callee), Some(args), Some(call)) = (callee, args, call) else {
            continue;
        };
        if chrome_shadowed {
            continue;
        }
        let callee_text = cx.text(&callee);
        let message_arg_index = match callee_text {
            "chrome.runtime.sendMessage" => Some(0),
            "chrome.tabs.sendMessage" => Some(1),
            _ => None,
        };
        if let Some(arg_index) = message_arg_index {
            let mut walk = args.walk();
            let arg = args.named_children(&mut walk).nth(arg_index);
            let identity = match arg {
                Some(arg) => classify_message(&cx, arg),
                None => Pending::Computed("<no message argument>".into()),
            };
            index.sites.push(PendingSite {
                role: ChannelRole::Produces,
                identity,
                symbol: enclosing_symbol(&cx, call),
                path: path.into(),
                byte_start: call.start_byte() as u64,
                byte_end: call.end_byte() as u64,
            });
        } else if callee_text == "chrome.runtime.onMessage.addListener" {
            index.listeners.push(PendingSite {
                role: ChannelRole::Consumes,
                identity: Pending::Computed("chrome.runtime.onMessage.addListener".into()),
                symbol: enclosing_symbol(&cx, call),
                path: path.into(),
                byte_start: call.start_byte() as u64,
                byte_end: call.end_byte() as u64,
            });
        }
    }

    // Dispatch tables: `[MessageType.X]: handler` — an explicit handler
    // registration keyed by a message-type constant.
    let q_keys = Query::new(
        &language,
        r#"
        (pair
            key: (computed_property_name (member_expression) @member)
            value: (_) @handler) @pair
        "#,
    )
    .expect("static query");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q_keys, root, source);
    while let Some(m) = matches.next() {
        let (mut member, mut handler, mut pair) = (None, None, None);
        for c in m.captures {
            match q_keys.capture_names()[c.index as usize] {
                "member" => member = Some(c.node),
                "handler" => handler = Some(c.node),
                "pair" => pair = Some(c.node),
                _ => {}
            }
        }
        let (Some(member), Some(handler), Some(pair)) = (member, handler, pair) else {
            continue;
        };
        // A handler is code, not data: functions, references to them, or a
        // definition-wrapper call (`defineMessage({...})`).
        if !matches!(
            handler.kind(),
            "arrow_function" | "function_expression" | "identifier" | "call_expression"
        ) {
            continue;
        }
        index.dispatch_keys.push(PendingSite {
            role: ChannelRole::Consumes,
            identity: Pending::Member(cx.text(&member).to_string()),
            symbol: enclosing_symbol(&cx, pair),
            path: path.into(),
            byte_start: pair.start_byte() as u64,
            byte_end: pair.end_byte() as u64,
        });
    }
    Ok(())
}

fn record_creator(cx: &FileCx, index: &mut RepoIndex, name: &str, object: TsNode) {
    let pending = type_of_object(cx, object);
    if matches!(pending, Pending::Literal(_) | Pending::Member(_)) {
        insert_unique(&mut index.creators, name.to_string(), pending);
    }
}

fn site(pending: PendingSite, identity: IdentityExpr) -> EventSite {
    EventSite {
        kind: CHANNEL_KIND.into(),
        role: pending.role,
        identity,
        symbol: pending.symbol,
        path: pending.path,
        byte_start: pending.byte_start,
        byte_end: pending.byte_end,
    }
}

/// Extract chrome-messaging event sites for the whole tree. The result
/// feeds `events::stitch` exactly like SDK event sites: literal identities
/// become Confirmed channels, computed ones explicit Gaps.
pub fn extract_dir(root: &Path, id: &SourceId) -> Result<Vec<EventSite>, ExtractError> {
    let mut files = Vec::new();
    crate::collect_ts_files(root, root, &mut files)?;
    files.sort(); // deterministic order (US-0014)
    let mut index = RepoIndex::default();
    for rel in &files {
        let source = std::fs::read(root.join(rel))?;
        extract_file(&source, rel, id, &mut index)?;
    }

    let mut out = Vec::new();
    let sites = std::mem::take(&mut index.sites);
    for pending in sites {
        let identity = resolve(&index, &pending.identity);
        out.push(site(pending, identity));
    }
    // Consumer facts need a real runtime listener to exist — dispatch
    // tables alone are data until something registers them.
    if !index.listeners.is_empty() {
        let keys = std::mem::take(&mut index.dispatch_keys);
        let mut resolved_any = false;
        for pending in keys {
            let identity = resolve(&index, &pending.identity);
            if let IdentityExpr::Literal(_) = identity {
                resolved_any = true;
                out.push(site(pending, identity));
            }
            // Unresolved computed keys are not messaging evidence — a
            // dispatch table only counts through the const-map proof.
        }
        if !resolved_any {
            // The listener is the only consumer evidence: record the
            // dynamic dispatch surface explicitly (Gap at T0).
            let listeners = std::mem::take(&mut index.listeners);
            for pending in listeners {
                let identity = resolve(&index, &pending.identity);
                out.push(site(pending, identity));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(files: &[(&str, &str)]) -> Vec<EventSite> {
        let dir = tempfile::tempdir().unwrap();
        for (path, source) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, source).unwrap();
        }
        let id = SourceId {
            repo: "local/ext",
            commit: "abc123",
        };
        extract_dir(dir.path(), &id).unwrap()
    }

    #[test]
    fn literal_and_const_map_identities_resolve_across_files() {
        let sites = extract(&[
            (
                "src/protocol.ts",
                "export const MessageType = { Ping: 'ext.ping', Capture: 'ext.capture' } as const;\n",
            ),
            (
                "src/content.ts",
                concat!(
                    "import { MessageType } from './protocol.js';\n",
                    "export function ping() {\n",
                    "  void chrome.runtime.sendMessage({ type: MessageType.Ping });\n",
                    "  void chrome.runtime.sendMessage({ type: 'ext.literal' });\n",
                    "}\n",
                ),
            ),
        ]);
        let produced: Vec<(&str, &IdentityExpr)> = sites
            .iter()
            .filter(|s| s.role == ChannelRole::Produces)
            .map(|s| (s.kind.as_str(), &s.identity))
            .collect();
        assert_eq!(produced.len(), 2, "sites: {produced:?}");
        assert!(produced.iter().all(|(kind, _)| *kind == CHANNEL_KIND));
        assert!(
            produced
                .iter()
                .any(|(_, i)| **i == IdentityExpr::Literal("ext.ping".into()))
        );
        assert!(
            produced
                .iter()
                .any(|(_, i)| **i == IdentityExpr::Literal("ext.literal".into()))
        );
        // Producer sites carry their enclosing symbol for edge endpoints.
        assert!(
            sites
                .iter()
                .all(|s| s.symbol == Some("sym:local/ext@src/content.ts#ping".into()))
        );
    }

    #[test]
    fn creator_calls_resolve_one_hop_and_dynamic_stays_computed() {
        let sites = extract(&[
            (
                "src/messages.ts",
                concat!(
                    "export const MessageType = { Toggle: 'ext.toggle' } as const;\n",
                    "export function createToggleMessage() {\n",
                    "  return { type: MessageType.Toggle };\n",
                    "}\n",
                ),
            ),
            (
                "src/worker.ts",
                concat!(
                    "import { createToggleMessage } from './messages.js';\n",
                    "async function toggle(tabId: number, kind: string) {\n",
                    "  await chrome.tabs.sendMessage(tabId, createToggleMessage());\n",
                    "  await chrome.runtime.sendMessage({ type: `ext.${kind}` });\n",
                    "}\n",
                ),
            ),
        ]);
        let identities: Vec<&IdentityExpr> = sites.iter().map(|s| &s.identity).collect();
        assert!(
            identities.contains(&&IdentityExpr::Literal("ext.toggle".into())),
            "creator return resolves: {identities:?}"
        );
        // Template-string identity is runtime-computed — explicit, not guessed.
        assert!(
            identities
                .iter()
                .any(|i| matches!(i, IdentityExpr::Computed(raw) if raw.contains("ext.${kind}"))),
        );
    }

    #[test]
    fn dispatch_tables_subscribe_only_behind_a_real_listener() {
        let table = concat!(
            "import { MessageType } from './protocol.js';\n",
            "export const handlers = {\n",
            "  [MessageType.Ping]: () => 'pong',\n",
            "};\n",
        );
        let protocol = "export const MessageType = { Ping: 'ext.ping' } as const;\n";

        // Without a listener the table is just data — no consumer facts.
        let unregistered = extract(&[("src/protocol.ts", protocol), ("src/table.ts", table)]);
        assert!(unregistered.iter().all(|s| s.role != ChannelRole::Consumes));

        // With a listener the resolvable keys are explicit registrations.
        let registered = extract(&[
            ("src/protocol.ts", protocol),
            ("src/table.ts", table),
            (
                "src/worker.ts",
                "chrome.runtime.onMessage.addListener(() => true);\n",
            ),
        ]);
        let consumed: Vec<&EventSite> = registered
            .iter()
            .filter(|s| s.role == ChannelRole::Consumes)
            .collect();
        assert_eq!(consumed.len(), 1);
        assert_eq!(
            consumed[0].identity,
            IdentityExpr::Literal("ext.ping".into())
        );
        assert_eq!(consumed[0].path, "src/table.ts");

        // A listener with no resolvable table records the dynamic dispatch
        // surface as one explicit computed site (Gap at T0).
        let dynamic_only = extract(&[(
            "src/worker.ts",
            "chrome.runtime.onMessage.addListener(() => true);\n",
        )]);
        let consumed: Vec<&EventSite> = dynamic_only
            .iter()
            .filter(|s| s.role == ChannelRole::Consumes)
            .collect();
        assert_eq!(consumed.len(), 1);
        assert!(matches!(&consumed[0].identity, IdentityExpr::Computed(_)));
    }

    #[test]
    fn shadowed_chrome_and_conflicting_const_maps_fail_closed() {
        // A local `chrome` binding is application code, not the platform.
        let shadowed = extract(&[(
            "src/fake.ts",
            concat!(
                "const chrome = { runtime: { sendMessage: (m: unknown) => m } };\n",
                "chrome.runtime.sendMessage({ type: 'ext.fake' });\n",
            ),
        )]);
        assert!(shadowed.is_empty(), "shadowed chrome must not match");

        // Two exported maps with the same name and different values: the
        // member refuses to resolve rather than picking a winner.
        let conflicted = extract(&[
            (
                "a/protocol.ts",
                "export const MessageType = { Ping: 'a.ping' };\n",
            ),
            (
                "b/protocol.ts",
                "export const MessageType = { Ping: 'b.ping' };\n",
            ),
            (
                "src/send.ts",
                concat!(
                    "import { MessageType } from '../a/protocol.js';\n",
                    "chrome.runtime.sendMessage({ type: MessageType.Ping });\n",
                ),
            ),
        ]);
        assert_eq!(conflicted.len(), 1);
        assert!(matches!(
            &conflicted[0].identity,
            IdentityExpr::Computed(raw) if raw == "MessageType.Ping"
        ));
    }
}
