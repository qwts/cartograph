//! Chrome runtime messaging (US-0016/AC-0072): deterministic T0
//! producer/consumer sites for `chrome.runtime`/`chrome.tabs` messaging.
//!
//! Producers are `chrome.runtime.sendMessage(msg)` / `chrome.tabs.sendMessage(tab, msg)`
//! call sites. The receiver must be the platform global: any local binding
//! named `chrome` — variable, import (named/default/namespace), parameter,
//! or function declaration — shadows the API and fails the whole file
//! closed, mirroring the `fetch` rule (#149 review).
//!
//! Message identity is the `type` property of the message object, resolved
//! deterministically through (a) string literals, (b) member access into a
//! const object of string literals (`MessageType.Ping`) whose binding is
//! **proven** — declared in the same file or imported through a relative
//! specifier resolving to the repo file that exports it — or (c) a one-hop
//! call to a binding-proven creator function whose returned object literal
//! carries a resolvable `type`. Unproven bindings (bare-package imports,
//! parameters, same-name coincidences elsewhere in the repo) stay
//! `Computed`, and the events crate records an explicit Gap (AC-0012,
//! R-INT-4) — never a guess.
//!
//! Consumers are explicit handler registrations: object-literal dispatch
//! tables whose computed keys (`[MessageType.LoadAlbums]: handler`) resolve
//! through the same proof, gated on the repo actually registering a
//! `chrome.runtime.onMessage.addListener`. A repo whose only consumer
//! evidence is the listener itself gets a single `Computed` site — the
//! dynamic dispatch surface is recorded, not invented.

use adapters_fw::events::{ChannelRole, EventSite, IdentityExpr};
use std::collections::BTreeMap;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node as TsNode, Parser, Query, QueryCursor};

use crate::const_resolution::{ConstIndex, collect_const_objects, collect_imports};
use crate::{ExtractError, FileCx, SourceId, enclosing_symbol, literal_string, object_entries};

/// Channel kind for the events registry (`chan:chrome-message:<type>`).
pub const CHANNEL_KIND: &str = "chrome-message";

/// A message identity before binding-proven resolution.
#[derive(Debug, Clone)]
enum Pending {
    /// A string literal — already resolved.
    Literal(String),
    /// `Object.Member` — resolve through the proven const index.
    Member(String),
    /// `createX(…)` — resolve through a proven creator's returned `type`.
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
    consts: ConstIndex,
    /// (file, creator name) → the `type` its returned object declares.
    creator_defs: BTreeMap<(String, String), Pending>,
    /// (file, creator name) — the creator is exported from that file.
    exported_creators: BTreeMap<(String, String), ()>,
    sites: Vec<PendingSite>,
    /// Dispatch-table keys awaiting the listener gate + proof.
    dispatch_keys: Vec<PendingSite>,
    listeners: Vec<PendingSite>,
}

/// Unwrap `as const` / `satisfies` wrappers down to the inner expression.
pub(crate) fn unwrap_assertions<'t>(node: TsNode<'t>) -> TsNode<'t> {
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

impl RepoIndex {
    /// Resolve a pending identity as seen from `file`, binding-proven.
    fn resolve(&self, file: &str, pending: &Pending) -> IdentityExpr {
        match pending {
            Pending::Literal(value) => IdentityExpr::Literal(value.clone()),
            Pending::Member(member) => match self.consts.resolve_member(file, member) {
                Some(value) => IdentityExpr::Literal(value),
                None => IdentityExpr::Computed(member.clone()),
            },
            Pending::Creator(name) => {
                // Same-file creator, else an import-proven one.
                let key = (file.to_string(), name.clone());
                let (def_file, def) = if let Some(def) = self.creator_defs.get(&key) {
                    (file.to_string(), def)
                } else if let Some((target, original)) =
                    self.consts.prove_import(file, name, |candidate, original| {
                        self.exported_creators
                            .contains_key(&(candidate.to_string(), original.to_string()))
                    })
                {
                    match self.creator_defs.get(&(target.clone(), original)) {
                        Some(def) => (target, def),
                        None => return IdentityExpr::Computed(format!("{name}(…)")),
                    }
                } else {
                    return IdentityExpr::Computed(format!("{name}(…)"));
                };
                // The creator's own `type` resolves in *its* file context.
                match self.resolve(&def_file, &def.clone()) {
                    IdentityExpr::Literal(value) => IdentityExpr::Literal(value),
                    _ => IdentityExpr::Computed(format!("{name}(…)")),
                }
            }
            Pending::Computed(raw) => IdentityExpr::Computed(raw.clone()),
        }
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

    let mut chrome_shadowed = collect_imports(&cx, root, &language, &mut index.consts);
    collect_const_objects(&cx, root, &language, &mut index.consts);

    // Any other local binding named `chrome` — variable, parameter, or
    // function — shadows the platform API too (#149 review, fail closed).
    let q_bindings = Query::new(
        &language,
        r#"
        (variable_declarator name: (identifier) @binding)
        (required_parameter pattern: (identifier) @binding)
        (optional_parameter pattern: (identifier) @binding)
        (arrow_function parameter: (identifier) @binding)
        (function_declaration name: (identifier) @binding)
        "#,
    )
    .expect("static query");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q_bindings, root, source);
    while let Some(m) = matches.next() {
        for c in m.captures() {
            if cx.text(&c.node) == "chrome" {
                chrome_shadowed = true;
            }
        }
    }

    // Arrow-const creators: `const createPing = () => ({ type: … })`.
    let q_arrow_creators = Query::new(
        &language,
        r#"(variable_declarator name: (identifier) @name value: (arrow_function) @arrow) @decl"#,
    )
    .expect("static query");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q_arrow_creators, root, source);
    while let Some(m) = matches.next() {
        let (mut name, mut arrow, mut decl) = (None, None, None);
        for c in m.captures() {
            match q_arrow_creators.capture_names()[c.index as usize] {
                "name" => name = Some(cx.text(&c.node).to_string()),
                "arrow" => arrow = Some(c.node),
                "decl" => decl = Some(c.node),
                _ => {}
            }
        }
        let (Some(name), Some(arrow), Some(decl)) = (name, arrow, decl) else {
            continue;
        };
        if let Some(body) = arrow.child_by_field_name("body") {
            let body = if body.kind() == "parenthesized_expression" {
                body.named_child(0).unwrap_or(body)
            } else {
                body
            };
            record_creator(
                &cx,
                index,
                &name,
                body,
                crate::const_resolution::is_exported(decl),
            );
        }
    }

    // Function-declaration creators: `function createPing() { return { type } }`.
    let q_funcs = Query::new(
        &language,
        r#"(function_declaration name: (identifier) @name body: (statement_block) @body) @decl"#,
    )
    .expect("static query");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q_funcs, root, source);
    while let Some(m) = matches.next() {
        let (mut name, mut body, mut decl) = (None, None, None);
        for c in m.captures() {
            match q_funcs.capture_names()[c.index as usize] {
                "name" => name = Some(cx.text(&c.node).to_string()),
                "body" => body = Some(c.node),
                "decl" => decl = Some(c.node),
                _ => {}
            }
        }
        let (Some(name), Some(body), Some(decl)) = (name, body, decl) else {
            continue;
        };
        let exported = crate::const_resolution::is_exported(decl);
        let mut stack = vec![body];
        while let Some(node) = stack.pop() {
            if node.kind() == "return_statement" {
                if let Some(value) = node.named_child(0) {
                    let value = unwrap_assertions(value);
                    if value.kind() == "object" {
                        record_creator(&cx, index, &name, value, exported);
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

    // Producer and listener sites.
    let q_calls = Query::new(
        &language,
        r#"(call_expression function: (member_expression) @callee arguments: (arguments) @args) @call"#,
    )
    .expect("static query");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q_calls, root, source);
    while let Some(m) = matches.next() {
        let (mut callee, mut args, mut call) = (None, None, None);
        for c in m.captures() {
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
        for c in m.captures() {
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

fn record_creator(cx: &FileCx, index: &mut RepoIndex, name: &str, object: TsNode, exported: bool) {
    let pending = type_of_object(cx, object);
    if matches!(pending, Pending::Literal(_) | Pending::Member(_)) {
        let key = (cx.path.to_string(), name.to_string());
        if exported {
            index.exported_creators.insert(key.clone(), ());
        }
        index.creator_defs.insert(key, pending);
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
    crate::collect_ts_files(root, &mut files)?;
    files.sort(); // deterministic order (US-0014)
    let mut index = RepoIndex::default();
    for rel in &files {
        let source = std::fs::read(root.join(rel))?;
        extract_file(&source, rel, id, &mut index)?;
    }

    let mut out = Vec::new();
    let sites = std::mem::take(&mut index.sites);
    for pending in sites {
        let identity = index.resolve(&pending.path.clone(), &pending.identity);
        out.push(site(pending, identity));
    }
    // Consumer facts need a real runtime listener to exist — dispatch
    // tables alone are data until something registers them.
    if !index.listeners.is_empty() {
        let keys = std::mem::take(&mut index.dispatch_keys);
        let mut resolved_any = false;
        for pending in keys {
            let identity = index.resolve(&pending.path.clone(), &pending.identity);
            if let IdentityExpr::Literal(_) = identity {
                resolved_any = true;
                out.push(site(pending, identity));
            }
            // Unresolved computed keys are not messaging evidence — a
            // dispatch table only counts through the binding proof.
        }
        if !resolved_any {
            // The listener is the only consumer evidence: record the
            // dynamic dispatch surface explicitly (Gap at T0).
            let listeners = std::mem::take(&mut index.listeners);
            for pending in listeners {
                let identity = index.resolve(&pending.path.clone(), &pending.identity);
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
    fn every_local_chrome_binding_shadows_the_platform_api() {
        // #149 review: variables, parameters, function declarations, and
        // namespace imports named `chrome` all fail the file closed.
        for (name, source) in [
            (
                "variable",
                "const chrome = { runtime: { sendMessage: (m: unknown) => m } };\n\
                 chrome.runtime.sendMessage({ type: 'ext.fake' });\n",
            ),
            (
                "parameter",
                "export function send(chrome: { runtime: { sendMessage(m: unknown): void } }) {\n\
                 \x20 chrome.runtime.sendMessage({ type: 'ext.fake' });\n\
                 }\n",
            ),
            (
                "namespace import",
                "import * as chrome from './shim.js';\n\
                 chrome.runtime.sendMessage({ type: 'ext.fake' });\n",
            ),
            (
                "arrow parameter",
                "export const send = (chrome: { runtime: { sendMessage(m: unknown): void } }) =>\n\
                 \x20 chrome.runtime.sendMessage({ type: 'ext.fake' });\n",
            ),
        ] {
            let sites = extract(&[("src/fake.ts", source)]);
            assert!(sites.is_empty(), "{name} shadow must not match: {sites:?}");
        }
    }

    #[test]
    fn unproven_bindings_stay_computed_and_proof_picks_the_right_map() {
        // Two same-named exported maps: the import proof selects the one the
        // file actually imports — never a repo-wide name coincidence.
        let sites = extract(&[
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
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].identity, IdentityExpr::Literal("a.ping".into()));

        // A bare-package import cannot be proven inside the repo: Computed,
        // even though an unrelated same-named map exists elsewhere.
        let unproven = extract(&[
            (
                "other/protocol.ts",
                "export const MessageType = { Ping: 'other.ping' };\n",
            ),
            (
                "src/send.ts",
                concat!(
                    "import { MessageType } from 'some-lib';\n",
                    "chrome.runtime.sendMessage({ type: MessageType.Ping });\n",
                ),
            ),
        ]);
        assert_eq!(unproven.len(), 1);
        assert!(matches!(
            &unproven[0].identity,
            IdentityExpr::Computed(raw) if raw == "MessageType.Ping"
        ));

        // Aliased named imports keep the proof through the original name.
        let aliased = extract(&[
            (
                "src/protocol.ts",
                "export const MessageType = { Ping: 'ext.ping' };\n",
            ),
            (
                "src/send.ts",
                concat!(
                    "import { MessageType as MT } from './protocol.js';\n",
                    "chrome.runtime.sendMessage({ type: MT.Ping });\n",
                ),
            ),
        ]);
        assert_eq!(aliased.len(), 1);
        assert_eq!(
            aliased[0].identity,
            IdentityExpr::Literal("ext.ping".into())
        );

        // An imported creator resolves only through its own export proof.
        let creator_unproven = extract(&[
            (
                "other/messages.ts",
                concat!(
                    "export const MessageType = { Toggle: 'other.toggle' };\n",
                    "export function createToggleMessage() {\n",
                    "  return { type: MessageType.Toggle };\n",
                    "}\n",
                ),
            ),
            (
                "src/worker.ts",
                concat!(
                    "import { createToggleMessage } from 'some-lib';\n",
                    "async function toggle(tabId: number) {\n",
                    "  await chrome.tabs.sendMessage(tabId, createToggleMessage());\n",
                    "}\n",
                ),
            ),
        ]);
        assert_eq!(creator_unproven.len(), 1);
        assert!(matches!(
            &creator_unproven[0].identity,
            IdentityExpr::Computed(raw) if raw == "createToggleMessage(…)"
        ));
    }
}
