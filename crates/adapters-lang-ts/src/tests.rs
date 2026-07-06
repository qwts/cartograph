use super::*;

const APP_TS: &str = r#"
import express from 'express';
import { listUsers } from './users';

const app = express();

app.get('/users', listUsers);
app.post('/users', (req, res) => {
  createUser();
});

function createUser() {
  validate();
}

function validate() {}

app.listen(3000);
"#;

const USERS_TS: &str = r#"
export function listUsers() {}
"#;

fn id() -> SourceId<'static> {
    SourceId {
        repo: "qwtm/example",
        commit: "abc123",
    }
}

fn fixture_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("app.ts"), APP_TS).unwrap();
    std::fs::write(src.join("users.ts"), USERS_TS).unwrap();
    // Noise that must be skipped:
    std::fs::create_dir_all(dir.path().join("node_modules/junk")).unwrap();
    std::fs::write(
        dir.path().join("node_modules/junk/index.ts"),
        "export const x = 1;",
    )
    .unwrap();
    std::fs::write(src.join("types.d.ts"), "declare const y: number;").unwrap();
    dir
}

fn edge_pairs<'a>(ex: &'a Extraction, label: &str) -> Vec<(&'a str, &'a str)> {
    ex.edges
        .iter()
        .filter(|e| e.label == label)
        .map(|e| (e.src.as_str(), e.dst.as_str()))
        .collect()
}

#[test]
fn extracts_express_endpoints_not_arbitrary_calls() {
    // AC-0004: endpoints via the framework adapter, marked Confirmed.
    let ex = extract_source(APP_TS.as_bytes(), "src/app.ts", &id()).unwrap();
    let endpoints: Vec<_> = ex.nodes.iter().filter(|n| n.label == "Endpoint").collect();
    let ids: Vec<_> = endpoints.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, vec!["ep:GET:/users", "ep:POST:/users"]);
    // `app.listen(3000)` and non-router receivers must not become endpoints.
    assert!(!ids.iter().any(|i| i.contains("listen")));
}

#[test]
fn endpoint_receiver_must_come_from_framework_factory() {
    // A `get` call on an untracked receiver is NOT a T0 endpoint.
    let src = r#"
const map = new Map();
map.get('/users');
"#;
    let ex = extract_source(src.as_bytes(), "src/other.ts", &id()).unwrap();
    assert!(ex.nodes.iter().all(|n| n.label != "Endpoint"));
}

#[test]
fn handles_edges_bind_named_and_anonymous_handlers() {
    let ex = extract_source(APP_TS.as_bytes(), "src/app.ts", &id()).unwrap();
    let handles = edge_pairs(&ex, "HANDLES");
    // Named handler resolves through the import — cross-file, deterministically.
    assert!(handles.contains(&("ep:GET:/users", "sym:src/users.ts#listUsers")));
    // Anonymous handler gets a stable offset-keyed symbol in this file.
    let (_, anon) = handles
        .iter()
        .find(|(src, _)| *src == "ep:POST:/users")
        .expect("POST handler edge");
    assert!(anon.starts_with("sym:src/app.ts#anon@"));
}

#[test]
fn call_edges_are_symbol_to_symbol() {
    // AC-0005: intra-procedural call edges.
    let ex = extract_source(APP_TS.as_bytes(), "src/app.ts", &id()).unwrap();
    let calls = edge_pairs(&ex, "CALLS");
    // Anonymous POST handler calls createUser; createUser calls validate.
    assert!(
        calls
            .iter()
            .any(|(src, dst)| src.starts_with("sym:src/app.ts#anon@")
                && *dst == "sym:src/app.ts#createUser")
    );
    assert!(calls.contains(&("sym:src/app.ts#createUser", "sym:src/app.ts#validate")));
}

#[test]
fn imports_resolve_relative_files_and_modules() {
    let ex = extract_source(APP_TS.as_bytes(), "src/app.ts", &id()).unwrap();
    let imports = edge_pairs(&ex, "IMPORTS");
    assert!(imports.contains(&("file:src/app.ts", "file:src/users.ts")));
    assert!(imports.contains(&("file:src/app.ts", "mod:express")));
}

#[test]
fn every_fact_carries_confirmed_t0_provenance() {
    // AC-0006: provenance {tier, confidence, evidence span, extractor} on all facts.
    let ex = extract_source(APP_TS.as_bytes(), "src/app.ts", &id()).unwrap();
    let all_props = ex
        .nodes
        .iter()
        .map(|n| &n.props)
        .chain(ex.edges.iter().map(|e| &e.props));
    for props in all_props {
        let prov = props.get("prov").expect("prov present");
        assert_eq!(prov["tier"], "Deterministic");
        assert_eq!(prov["confidence_tier"], "Confirmed");
        assert_eq!(prov["extractor_id"], "t0.adapter-ts");
        let ev = &prov["evidence"][0];
        assert_eq!(ev["repo"], "qwtm/example");
        assert_eq!(ev["path"], "src/app.ts");
        assert_eq!(ev["commit_sha"], "abc123");
        assert!(ev["byte_end"].as_u64().unwrap() > ev["byte_start"].as_u64().unwrap());
        assert!(prov["content_hash"].as_str().unwrap().len() == 64);
    }
}

#[test]
fn evidence_spans_point_at_the_actual_source() {
    let ex = extract_source(APP_TS.as_bytes(), "src/app.ts", &id()).unwrap();
    let ep = ex.nodes.iter().find(|n| n.id == "ep:GET:/users").unwrap();
    let ev = &ep.props["prov"]["evidence"][0];
    let span = &APP_TS.as_bytes()
        [ev["byte_start"].as_u64().unwrap() as usize..ev["byte_end"].as_u64().unwrap() as usize];
    // Jump-to-source lands on the registration call (M1 exit-gate groundwork).
    assert_eq!(
        std::str::from_utf8(span).unwrap(),
        "app.get('/users', listUsers)"
    );
}

#[test]
fn dir_walk_skips_noise_and_is_deterministic() {
    let dir = fixture_dir();
    let a = extract_dir(dir.path(), &id()).unwrap();
    let b = extract_dir(dir.path(), &id()).unwrap();
    // node_modules and .d.ts excluded: only our two files produce File nodes
    // (placeholders are labeled but flagged).
    let files: Vec<_> = a
        .nodes
        .iter()
        .filter(|n| n.label == "File" && n.props.get("placeholder").is_none())
        .map(|n| n.id.as_str())
        .collect();
    assert_eq!(files, vec!["file:src/app.ts", "file:src/users.ts"]);
    // US-0014 groundwork: identical input -> identical facts (incl. hashes).
    assert_eq!(
        serde_json::to_string(&a.nodes).unwrap(),
        serde_json::to_string(&b.nodes).unwrap()
    );
    assert_eq!(
        serde_json::to_string(&a.edges).unwrap(),
        serde_json::to_string(&b.edges).unwrap()
    );
}

#[test]
fn close_over_creates_placeholders_for_unresolved_targets() {
    let src = "import { helper } from './missing';\nexport function run() { helper(); }";
    let mut ex = extract_source(src.as_bytes(), "src/run.ts", &id()).unwrap();
    ex.close_over_endpoints();
    // The missing file and its symbol exist as flagged placeholders.
    let placeholder = ex
        .nodes
        .iter()
        .find(|n| n.id == "file:src/missing.ts")
        .expect("placeholder file node");
    assert_eq!(placeholder.props["placeholder"], true);
}

// --- Event-site detection (US-0004; stitching tested in `events`) ----------

use adapters_fw::events::{ChannelRole, IdentityExpr};

fn sites_for(source: &str) -> Vec<adapters_fw::events::EventSite> {
    extract_source(source.as_bytes(), "app.ts", &id())
        .unwrap()
        .event_sites
}

// AC-0010 groundwork: receivers must be proven from the SDK, mirroring the
// endpoint extractor — a look-alike `emit` on an unproven object is not a
// producer site.
#[test]
fn event_receiver_must_come_from_sdk_constructor() {
    let sites = sites_for(
        r#"
import { EventEmitter } from 'events';
const bus = new EventEmitter();
const impostor = getBus();
function a() { bus.emit('real.event'); }
function b() { impostor.emit('fake.event'); }
function c() { bus.emit('also.real'); }
"#,
    );
    let ids: Vec<_> = sites
        .iter()
        .map(|s| match &s.identity {
            IdentityExpr::Literal(l) => l.as_str(),
            _ => "?",
        })
        .collect();
    assert_eq!(ids, ["real.event", "also.real"]);
}

// A constructor imported from the wrong module is not an SDK command.
#[test]
fn command_constructor_must_be_imported_from_sdk_module() {
    let sites = sites_for(
        r#"
import { SendMessageCommand } from './local-fake';
new SendMessageCommand({ QueueUrl: 'https://q' });
"#,
    );
    assert!(sites.is_empty(), "look-alike ctor from a relative module");
}

// AWS SDK v2 method style: receiver proven from `new AWS.SQS()`.
#[test]
fn aws_v2_send_message_is_detected_with_key_identity() {
    let sites = sites_for(
        r#"
import AWS from 'aws-sdk';
const sqs = new AWS.SQS();
export function push() {
  sqs.sendMessage({ QueueUrl: 'https://sqs.example/q', MessageBody: 'x' });
}
"#,
    );
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].kind, "sqs-queue");
    assert_eq!(sites[0].role, ChannelRole::Produces);
    assert_eq!(
        sites[0].identity,
        IdentityExpr::Literal("https://sqs.example/q".into())
    );
    assert_eq!(sites[0].symbol.as_deref(), Some("sym:app.ts#push"));
}

// EventBridge: the identity key is found nested inside Entries[].
#[test]
fn eventbridge_detail_type_is_found_nested() {
    let sites = sites_for(
        r#"
import { PutEventsCommand } from '@aws-sdk/client-eventbridge';
new PutEventsCommand({
  Entries: [{ Source: 'app', DetailType: 'order.placed', Detail: '{}' }],
});
"#,
    );
    assert_eq!(sites.len(), 1);
    assert_eq!(
        sites[0].identity,
        IdentityExpr::Literal("order.placed".into())
    );
    assert_eq!(sites[0].kind, "eventbridge-detail-type");
}

// A local `const` bound to a string literal resolves at T0 (same file,
// deterministic); an unbound identifier stays computed (escalates).
#[test]
fn local_const_identity_is_literal_unknown_identifier_is_computed() {
    let sites = sites_for(
        r#"
import { EventEmitter } from 'events';
const bus = new EventEmitter();
const TOPIC = 'orders.created';
function a(dynamic: string) {
  bus.emit(TOPIC);
  bus.emit(dynamic);
}
"#,
    );
    assert_eq!(sites.len(), 2);
    assert_eq!(
        sites[0].identity,
        IdentityExpr::Literal("orders.created".into())
    );
    assert_eq!(sites[1].identity, IdentityExpr::Computed("dynamic".into()));
}

// A registry key that is not statically present is a computed identity —
// the site is kept (it escalates), never silently dropped (AC-0012).
#[test]
fn missing_identity_key_yields_computed_not_dropped() {
    let sites = sites_for(
        r#"
import { SendMessageCommand } from '@aws-sdk/client-sqs';
declare const params: any;
new SendMessageCommand(params);
"#,
    );
    assert_eq!(sites.len(), 1);
    assert!(matches!(sites[0].identity, IdentityExpr::Computed(_)));
}
