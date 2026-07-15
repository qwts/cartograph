//! IndexedDB data model (US-0016/AC-0073): deterministic T0 `DataEntity`
//! nodes and `READS`/`WRITES` relations from explicit schema/store
//! declarations and repository operations.
//!
//! Entities come from `createObjectStore(<name>)` declarations; operations
//! come from method calls on store handles — either chained directly
//! (`tx.objectStore(X).put(…)`) or through a same-file `const store =
//! tx.objectStore(X)` binding. Store identities resolve through string
//! literals or a **binding-proven** const-string map (`DataStore.History`
//! declared in the same file or import-proven to the repo file exporting
//! it) — the same rule as chrome messaging (#149 review); a store identity
//! that stays unproven or runtime-computed becomes an explicit Gap
//! (R-INT-4), never a same-name coincidence elsewhere in the repo.

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::BTreeMap;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node as TsNode, Parser, Query, QueryCursor};

use crate::chrome_messaging::unwrap_assertions;
use crate::const_resolution::{ConstIndex, collect_const_objects, collect_imports};
use crate::{ExtractError, Extraction, FileCx, SourceId, enclosing_symbol, literal_string};

const EXTRACTOR_ID: &str = "t0.webextension";

/// Store-handle methods that read from an object store.
const READ_METHODS: &[&str] = &[
    "get",
    "getAll",
    "getAllKeys",
    "getKey",
    "count",
    "openCursor",
    "openKeyCursor",
];
/// Store-handle methods that write to an object store.
const WRITE_METHODS: &[&str] = &["put", "add", "delete", "clear"];

/// A store name before repo-wide const resolution.
#[derive(Debug, Clone, PartialEq)]
enum StoreName {
    Literal(String),
    Member(String),
    Computed(String),
}

struct PendingOp {
    label: &'static str, // READS | WRITES
    store: StoreName,
    symbol: Option<String>,
    path: String,
    byte_start: u64,
    byte_end: u64,
}

struct PendingDecl {
    store: StoreName,
    path: String,
    byte_start: u64,
    byte_end: u64,
}

#[derive(Default)]
struct RepoIndex {
    consts: ConstIndex,
    decls: Vec<PendingDecl>,
    ops: Vec<PendingOp>,
}

fn classify_store(cx: &FileCx, arg: TsNode) -> StoreName {
    let arg = unwrap_assertions(arg);
    if let Some(lit) = literal_string(cx, arg) {
        return StoreName::Literal(lit);
    }
    if arg.kind() == "member_expression" {
        return StoreName::Member(cx.text(&arg).to_string());
    }
    StoreName::Computed(cx.text(&arg).to_string())
}

/// Resolve a store name as seen from `file` — same-file or import-proven
/// const members only (#149 review); anything else is the Gap's raw text.
fn resolve(consts: &ConstIndex, file: &str, name: &StoreName) -> Result<String, String> {
    match name {
        StoreName::Literal(value) => Ok(value.clone()),
        StoreName::Member(member) => consts
            .resolve_member(file, member)
            .ok_or_else(|| member.clone()),
        StoreName::Computed(raw) => Err(raw.clone()),
    }
}

fn prov_value(
    id: &SourceId,
    path: &str,
    span: (u64, u64),
    confidence: ConfidenceTier,
    fact: &str,
) -> serde_json::Value {
    let provenance = Provenance::new(
        Tier::Deterministic,
        confidence,
        vec![EvidenceRef {
            repo: id.repo.into(),
            path: path.into(),
            byte_start: span.0,
            byte_end: span.1,
            commit_sha: id.commit.into(),
        }],
        EXTRACTOR_ID,
        fact.as_bytes(),
    )
    .expect("Deterministic within ceiling");
    serde_json::to_value(provenance).expect("provenance serializes")
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

    // Imports and const-string maps go through the shared binding-proven
    // index (#149 review) — same rule as chrome messaging.
    collect_imports(&cx, root, &language, &mut index.consts);
    collect_const_objects(&cx, root, &language, &mut index.consts);

    // `const store = tx.objectStore(X)` binds a handle for later ops. The
    // binding is scoped to its enclosing function (#150 review): two
    // sibling functions each using a local `store` for different object
    // stores must never share one file-wide binding.
    let q_decls = Query::new(
        &language,
        r#"(variable_declarator name: (identifier) @name value: (_) @value)"#,
    )
    .expect("static query");
    let mut store_vars: BTreeMap<(Option<String>, String), StoreName> = BTreeMap::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&q_decls, root, source);
    while let Some(m) = matches.next() {
        let (mut name, mut value) = (None, None);
        for c in m.captures {
            match q_decls.capture_names()[c.index as usize] {
                "name" => name = Some(c.node),
                "value" => value = Some(c.node),
                _ => {}
            }
        }
        let (Some(name), Some(value)) = (name, value) else {
            continue;
        };
        let value = unwrap_assertions(value);
        if let Some(store) = object_store_arg(&cx, value, "objectStore") {
            let scope = enclosing_symbol(&cx, name);
            store_vars.insert((scope, cx.text(&name).to_string()), store);
        }
    }

    // Calls: declarations, chained ops, and ops on bound handles.
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
        let Some(property) = callee.child_by_field_name("property") else {
            continue;
        };
        let method = cx.text(&property).to_string();

        // Declarations: `db.createObjectStore('history', …)`.
        if method == "createObjectStore" {
            if let Some(arg) = first_named(&args) {
                index.decls.push(PendingDecl {
                    store: classify_store(&cx, arg),
                    path: path.into(),
                    byte_start: call.start_byte() as u64,
                    byte_end: call.end_byte() as u64,
                });
            }
            continue;
        }

        let op_label = if READ_METHODS.contains(&method.as_str()) {
            "READS"
        } else if WRITE_METHODS.contains(&method.as_str()) {
            "WRITES"
        } else {
            continue;
        };
        let Some(object) = callee.child_by_field_name("object") else {
            continue;
        };
        let symbol = enclosing_symbol(&cx, call);
        // Chained: `tx.objectStore(X).put(…)` (also through `.index(…)`).
        let store = object_store_arg(&cx, object, "objectStore").or_else(|| {
            // Bound handle: `store.put(…)` / `store.index(…).getAll(…)` —
            // resolved in the op's own function scope first, then the
            // module scope for top-level handles (#150 review).
            let base = cx.text(&object);
            let base = base.split('.').next().unwrap_or(base);
            let handle = object
                .kind()
                .eq("identifier")
                .then(|| cx.text(&object))
                .or_else(|| (object.kind() == "call_expression").then_some(base))?;
            let handle = handle.split('(').next().unwrap_or(handle).to_string();
            store_vars
                .get(&(symbol.clone(), handle.clone()))
                .or_else(|| store_vars.get(&(None, handle)))
                .cloned()
        });
        if let Some(store) = store {
            index.ops.push(PendingOp {
                label: op_label,
                store,
                symbol,
                path: path.into(),
                byte_start: call.start_byte() as u64,
                byte_end: call.end_byte() as u64,
            });
        }
    }
    Ok(())
}

/// If `node` is (or chains from) an `<recv>.objectStore(<arg>)` call, the
/// classified store name.
fn object_store_arg(cx: &FileCx, node: TsNode, method: &str) -> Option<StoreName> {
    let mut current = unwrap_assertions(node);
    loop {
        if current.kind() != "call_expression" {
            return None;
        }
        let callee = current.child_by_field_name("function")?;
        if callee.kind() != "member_expression" {
            return None;
        }
        let property = callee.child_by_field_name("property")?;
        if cx.text(&property) == method {
            let args = current.child_by_field_name("arguments")?;
            return first_named(&args).map(|arg| classify_store(cx, arg));
        }
        // Walk through intermediate chain links such as `.index(…)`.
        current = callee.child_by_field_name("object")?;
        current = unwrap_assertions(current);
    }
}

fn first_named<'t>(args: &TsNode<'t>) -> Option<TsNode<'t>> {
    let mut walk = args.walk();
    args.named_children(&mut walk).next()
}

/// Extract the IndexedDB data model for the whole tree.
pub fn extract_dir(root: &Path, id: &SourceId) -> Result<Extraction, ExtractError> {
    let mut files = Vec::new();
    crate::collect_ts_files(root, root, &mut files)?;
    files.sort(); // deterministic order (US-0014)
    let mut index = RepoIndex::default();
    for rel in &files {
        let source = std::fs::read(root.join(rel))?;
        extract_file(&source, rel, id, &mut index)?;
    }

    let mut out = Extraction::default();
    let mut entities: BTreeMap<String, Node> = BTreeMap::new();
    let entity =
        |store: &str, path: &str, span: (u64, u64), entities: &mut BTreeMap<String, Node>| {
            let entity_id = format!("data:{}@idb:{store}", id.repo);
            entities.entry(entity_id.clone()).or_insert_with(|| Node {
                id: entity_id.clone(),
                label: "DataEntity".into(),
                props: serde_json::json!({
                    "store": store,
                    "storage": "indexeddb",
                    "prov": prov_value(
                        id,
                        path,
                        span,
                        ConfidenceTier::Confirmed,
                        &format!("DataEntity {entity_id}"),
                    ),
                }),
            });
            entity_id
        };

    // Declarations first: the schema is the entity's defining evidence.
    for decl in &index.decls {
        match resolve(&index.consts, &decl.path, &decl.store) {
            Ok(store) => {
                entity(
                    &store,
                    &decl.path,
                    (decl.byte_start, decl.byte_end),
                    &mut entities,
                );
            }
            Err(raw) => {
                let gap_id = format!("gap:idb:{}@{}@{}", id.repo, decl.path, decl.byte_start);
                out.nodes.push(Node {
                    id: gap_id.clone(),
                    label: "Gap".into(),
                    props: serde_json::json!({
                        "reason": "runtime-computed object-store identity",
                        "raw": raw,
                        "attempted_tiers": ["T0"],
                        "prov": prov_value(
                            id,
                            &decl.path,
                            (decl.byte_start, decl.byte_end),
                            ConfidenceTier::Gap,
                            &format!("Gap {gap_id}"),
                        ),
                    }),
                });
            }
        }
    }

    for op in &index.ops {
        match resolve(&index.consts, &op.path, &op.store) {
            Ok(store) => {
                let entity_id = entity(
                    &store,
                    &op.path,
                    (op.byte_start, op.byte_end),
                    &mut entities,
                );
                let src = op
                    .symbol
                    .clone()
                    .unwrap_or_else(|| format!("file:{}@{}", id.repo, op.path));
                out.edges.push(Edge {
                    src: src.clone(),
                    dst: entity_id.clone(),
                    label: op.label.into(),
                    props: serde_json::json!({
                        "prov": prov_value(
                            id,
                            &op.path,
                            (op.byte_start, op.byte_end),
                            ConfidenceTier::Confirmed,
                            &format!("{} {src} -> {entity_id}", op.label),
                        ),
                    }),
                });
            }
            Err(raw) => {
                let gap_id = format!("gap:idb:{}@{}@{}", id.repo, op.path, op.byte_start);
                let src = op
                    .symbol
                    .clone()
                    .unwrap_or_else(|| format!("file:{}@{}", id.repo, op.path));
                out.nodes.push(Node {
                    id: gap_id.clone(),
                    label: "Gap".into(),
                    props: serde_json::json!({
                        "reason": "runtime-computed object-store identity",
                        "raw": raw,
                        "attempted_tiers": ["T0"],
                        "prov": prov_value(
                            id,
                            &op.path,
                            (op.byte_start, op.byte_end),
                            ConfidenceTier::Gap,
                            &format!("Gap {gap_id}"),
                        ),
                    }),
                });
                out.edges.push(Edge {
                    src,
                    dst: gap_id.clone(),
                    label: op.label.into(),
                    props: serde_json::json!({
                        "prov": prov_value(
                            id,
                            &op.path,
                            (op.byte_start, op.byte_end),
                            ConfidenceTier::Gap,
                            &format!("{} -> {gap_id}", op.label),
                        ),
                    }),
                });
            }
        }
    }

    out.nodes.extend(entities.into_values());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(files: &[(&str, &str)]) -> Extraction {
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
    fn schema_declarations_and_repository_ops_build_a_cited_data_model() {
        let out = extract(&[
            (
                "src/schema.ts",
                "export const DataStore = { History: 'history', Blobs: 'blobs' } as const;\n",
            ),
            (
                "src/migrations.ts",
                "import { DataStore } from './schema.js';\n\
                 export function migrate(db: IDBDatabase) {\n\
                 \x20 db.createObjectStore(DataStore.History, { keyPath: 'uuid' });\n\
                 \x20 db.createObjectStore('settings');\n\
                 }\n",
            ),
            (
                "src/repo.ts",
                "import { DataStore } from './schema.js';\n\
                 export function saveRecord(tx: IDBTransaction, record: unknown) {\n\
                 \x20 tx.objectStore(DataStore.History).put(record);\n\
                 }\n\
                 export function listRecords(tx: IDBTransaction) {\n\
                 \x20 const store = tx.objectStore(DataStore.History);\n\
                 \x20 return store.getAll();\n\
                 }\n",
            ),
        ]);
        // Entities: declared stores plus the literal one.
        let ids: Vec<&str> = out
            .nodes
            .iter()
            .filter(|node| node.label == "DataEntity")
            .map(|node| node.id.as_str())
            .collect();
        assert!(ids.contains(&"data:local/ext@idb:history"), "ids: {ids:?}");
        assert!(ids.contains(&"data:local/ext@idb:settings"));

        // Chained write and bound-handle read both attribute to symbols.
        assert!(out.edges.iter().any(|edge| {
            edge.label == "WRITES"
                && edge.src == "sym:local/ext@src/repo.ts#saveRecord"
                && edge.dst == "data:local/ext@idb:history"
        }));
        assert!(out.edges.iter().any(|edge| {
            edge.label == "READS"
                && edge.src == "sym:local/ext@src/repo.ts#listRecords"
                && edge.dst == "data:local/ext@idb:history"
        }));
        // Every fact is Confirmed T0 with evidence into the actual call.
        let entity = out
            .nodes
            .iter()
            .find(|node| node.id == "data:local/ext@idb:history")
            .unwrap();
        let prov: Provenance = serde_json::from_value(entity.props["prov"].clone()).unwrap();
        assert_eq!(prov.confidence_tier, ConfidenceTier::Confirmed);
        assert_eq!(prov.extractor_id, EXTRACTOR_ID);
        assert!(prov.evidence[0].byte_end > prov.evidence[0].byte_start);
    }

    #[test]
    fn same_named_handles_stay_in_their_own_function_scope() {
        // #150 review: two sibling functions each bind a local `store` to a
        // different object store — ops must attribute to their own store,
        // never to a file-wide winner. A module-level handle still serves
        // functions that use it.
        let out = extract(&[(
            "src/repo.ts",
            "const shared = db.transaction('settings').objectStore('settings');\n\
             export function saveHistory(tx: IDBTransaction, record: unknown) {\n\
             \x20 const store = tx.objectStore('history');\n\
             \x20 store.put(record);\n\
             }\n\
             export function listBlobs(tx: IDBTransaction) {\n\
             \x20 const store = tx.objectStore('blobs');\n\
             \x20 return store.getAll();\n\
             }\n\
             export function readSetting() {\n\
             \x20 return shared.get('theme');\n\
             }\n",
        )]);
        let edge = |label: &str, src: &str| {
            out.edges
                .iter()
                .find(|edge| edge.label == label && edge.src.ends_with(src))
                .unwrap_or_else(|| panic!("{label} from {src}"))
                .dst
                .clone()
        };
        assert_eq!(edge("WRITES", "#saveHistory"), "data:local/ext@idb:history");
        assert_eq!(edge("READS", "#listBlobs"), "data:local/ext@idb:blobs");
        // Module-level binding is visible from the using function.
        assert_eq!(edge("READS", "#readSetting"), "data:local/ext@idb:settings");
    }

    #[test]
    fn unproven_store_bindings_fail_closed() {
        // #149 review parity: a bare-package import cannot be proven, even
        // when a same-named exported map exists elsewhere in the repo.
        let out = extract(&[
            (
                "other/schema.ts",
                "export const DataStore = { History: 'other-history' };\n",
            ),
            (
                "src/repo.ts",
                "import { DataStore } from 'some-lib';\n\
                 export function save(tx: IDBTransaction, record: unknown) {\n\
                 \x20 tx.objectStore(DataStore.History).put(record);\n\
                 }\n",
            ),
        ]);
        assert!(out.nodes.iter().all(|node| node.label != "DataEntity"));
        assert!(
            out.nodes
                .iter()
                .any(|node| { node.label == "Gap" && node.props["raw"] == "DataStore.History" })
        );
    }

    #[test]
    fn computed_store_identities_are_explicit_gaps() {
        let out = extract(&[(
            "src/dynamic.ts",
            "export function wipe(db: IDBDatabase, name: string) {\n\
             \x20 db.createObjectStore(`${name}-cache`);\n\
             }\n\
             export function drain(tx: IDBTransaction, name: string) {\n\
             \x20 tx.objectStore(name).clear();\n\
             }\n",
        )]);
        let gaps: Vec<&Node> = out.nodes.iter().filter(|n| n.label == "Gap").collect();
        assert_eq!(gaps.len(), 2, "one per unresolved site");
        assert!(
            gaps.iter()
                .all(|gap| { gap.props["reason"] == "runtime-computed object-store identity" })
        );
        // The op edge points at the gap, keeping the relation explicit.
        assert!(out.edges.iter().any(|edge| {
            edge.label == "WRITES"
                && edge.src == "sym:local/ext@src/dynamic.ts#drain"
                && edge.dst.starts_with("gap:idb:")
        }));
        assert!(out.nodes.iter().all(|node| node.label != "DataEntity"));
    }
}
