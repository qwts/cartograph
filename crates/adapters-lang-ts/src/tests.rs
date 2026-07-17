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
    assert_eq!(
        ids,
        vec!["ep:qwtm/example@GET:/users", "ep:qwtm/example@POST:/users"]
    );
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
    assert!(handles.contains(&(
        "ep:qwtm/example@GET:/users",
        "sym:qwtm/example@src/users.ts#listUsers"
    )));
    // Anonymous handler gets a stable offset-keyed symbol in this file.
    let (_, anon) = handles
        .iter()
        .find(|(src, _)| *src == "ep:qwtm/example@POST:/users")
        .expect("POST handler edge");
    assert!(anon.starts_with("sym:qwtm/example@src/app.ts#anon@"));
}

#[test]
fn call_edges_are_symbol_to_symbol() {
    // AC-0005: intra-procedural call edges.
    let ex = extract_source(APP_TS.as_bytes(), "src/app.ts", &id()).unwrap();
    let calls = edge_pairs(&ex, "CALLS");
    // Anonymous POST handler calls createUser; createUser calls validate.
    assert!(calls.iter().any(
        |(src, dst)| src.starts_with("sym:qwtm/example@src/app.ts#anon@")
            && *dst == "sym:qwtm/example@src/app.ts#createUser"
    ));
    assert!(calls.contains(&(
        "sym:qwtm/example@src/app.ts#createUser",
        "sym:qwtm/example@src/app.ts#validate"
    )));
}

#[test]
fn typed_member_calls_resolve_local_and_imported_methods() {
    // AC-0005: explicit TS receiver types and constructor types make member
    // calls deterministic across method and file boundaries.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("service.ts"),
        r#"
export class UserService {
  list() {}
}
"#,
    )
    .unwrap();
    std::fs::write(
        src.join("controller.ts"),
        r#"
import { UserService } from './service';
import { MissingService as MS } from './missing';

export class UserController {
  constructor(private readonly users: UserService) {}
  index() { this.users.list(); }
}

export function run(users: UserService) {
  users.list();
}

export function inferred() {
  const users = new UserService();
  users.list();
}

export function unresolved(missing: MS) {
  missing.run();
}
"#,
    )
    .unwrap();

    let ex = extract_dir(dir.path(), &id()).unwrap();
    let calls = edge_pairs(&ex, "CALLS");
    let target = "sym:qwtm/example@src/service.ts#UserService.list";
    assert!(calls.contains(&(
        "sym:qwtm/example@src/controller.ts#UserController.index",
        target
    )));
    assert!(calls.contains(&("sym:qwtm/example@src/controller.ts#run", target)));
    assert!(calls.contains(&("sym:qwtm/example@src/controller.ts#inferred", target)));
    assert!(
        !calls
            .iter()
            .any(|(_, dst)| *dst == "sym:qwtm/example@src/missing.ts#MissingService.run")
    );
    let gap = ex
        .nodes
        .iter()
        .find(|node| node.label == "Gap" && node.props["callee"] == "MissingService.run")
        .expect("unresolved typed call remains an explicit semantic slot");
    assert!(calls.contains(&(
        "sym:qwtm/example@src/controller.ts#unresolved",
        gap.id.as_str()
    )));
    assert_eq!(gap.props["prov"]["confidence_tier"], "Gap");
    let target_node = ex.nodes.iter().find(|node| node.id == target).unwrap();
    assert_eq!(target_node.props["kind"], "Method");
}

#[test]
fn unresolved_relative_import_calls_emit_gaps_without_global_noise() {
    // AC-0021: a call with a relative-import target that directory-wide T0
    // cannot prove becomes a CALLS Gap. Package/global calls are not local
    // semantic candidates and must not flood the graph with false slots.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("caller.ts"),
        r#"
import { processOrder as po } from './missing';
import { randomUUID } from 'node:crypto';
export function run() {
  po();
  randomUUID();
  console.log('done');
}
"#,
    )
    .unwrap();
    let ex = extract_dir(dir.path(), &id()).unwrap();
    let gaps: Vec<_> = ex.nodes.iter().filter(|node| node.label == "Gap").collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].props["callee"], "processOrder");
    let calls = edge_pairs(&ex, "CALLS");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, gaps[0].id);
    assert_eq!(
        ex.edges
            .iter()
            .find(|edge| edge.label == "CALLS")
            .unwrap()
            .props["prov"]["confidence_tier"],
        "Gap"
    );
}

#[test]
fn fastify_and_nest_endpoints_are_import_proven() {
    // AC-0004: framework registry coverage includes Fastify factory routes and
    // Nest controller/method decorators, without name-based guessing.
    let fastify = extract_source(
        br#"
import Fastify from 'fastify';
const app = Fastify();
function listUsers() {}
app.get('/users', listUsers);
"#,
        "src/fastify.ts",
        &id(),
    )
    .unwrap();
    assert!(
        fastify
            .nodes
            .iter()
            .any(|node| node.id == "ep:qwtm/example@GET:/users")
    );

    let nest = extract_source(
        br#"
import { Controller as Api, Get, Post } from '@nestjs/common';

@Api('users')
export class UsersController {
  @Get()
  list() {}

  @Post(':id')
  update() {}
}
"#,
        "src/users.controller.ts",
        &id(),
    )
    .unwrap();
    let endpoints: Vec<_> = nest
        .nodes
        .iter()
        .filter(|node| node.label == "Endpoint")
        .map(|node| node.id.as_str())
        .collect();
    assert_eq!(
        endpoints,
        [
            "ep:qwtm/example@GET:/users",
            "ep:qwtm/example@POST:/users/:id"
        ]
    );
    let handles = edge_pairs(&nest, "HANDLES");
    assert!(handles.contains(&(
        "ep:qwtm/example@GET:/users",
        "sym:qwtm/example@src/users.controller.ts#UsersController.list"
    )));

    let lookalike = extract_source(
        br#"
function Controller(_: string) { return (_: unknown) => {}; }
function Get() { return (_: unknown) => {}; }
@Controller('fake')
class FakeController { @Get() list() {} }
"#,
        "src/fake.ts",
        &id(),
    )
    .unwrap();
    assert!(lookalike.nodes.iter().all(|node| node.label != "Endpoint"));
}

#[test]
fn imports_resolve_relative_files_and_modules() {
    let ex = extract_source(APP_TS.as_bytes(), "src/app.ts", &id()).unwrap();
    let imports = edge_pairs(&ex, "IMPORTS");
    assert!(imports.contains(&(
        "file:qwtm/example@src/app.ts",
        "file:qwtm/example@src/users.ts"
    )));
    assert!(imports.contains(&("file:qwtm/example@src/app.ts", "mod:express")));
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
    let ep = ex
        .nodes
        .iter()
        .find(|n| n.id == "ep:qwtm/example@GET:/users")
        .unwrap();
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
    assert_eq!(
        files,
        vec![
            "file:qwtm/example@src/app.ts",
            "file:qwtm/example@src/users.ts"
        ]
    );
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
fn delta_reingest_reuses_unchanged_files_and_recomputes_only_changes() {
    // AC-0040 (T-0040): source parsing is content-addressed per physical file.
    let dir = fixture_dir();
    let mut cache = IncrementalCache::default();
    let (_, first) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(first.recomputed_files, 2);
    assert_eq!(first.reused_files, 0);

    let (same, second) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(second.recomputed_files, 0);
    assert_eq!(second.reused_files, 2);

    std::fs::write(
        dir.path().join("src/users.ts"),
        "export function listUsers() { return []; }\n",
    )
    .unwrap();
    let (changed, third) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(third.recomputed_files, 1);
    assert_eq!(third.reused_files, 1);
    assert_ne!(
        serde_json::to_string(&same.nodes).unwrap(),
        serde_json::to_string(&changed.nodes).unwrap()
    );

    std::fs::remove_file(dir.path().join("src/users.ts")).unwrap();
    let (_, fourth) = extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
    assert_eq!(fourth.deleted_files, 1);
}

#[test]
fn progress_hook_fires_once_per_file_in_deterministic_order() {
    // AC-0094 (T-0094): the shell's live "what it's doing right now" ping
    // rides this callback, one call per file, in the same sorted order
    // extraction itself uses (US-0014's determinism).
    let dir = fixture_dir();
    let mut cache = IncrementalCache::default();
    let mut seen = Vec::new();
    let (_, stats) =
        extract_dir_incremental_with_progress(dir.path(), &id(), &mut cache, &mut |path| {
            seen.push(path.to_string())
        })
        .unwrap();
    assert_eq!(stats.recomputed_files, 2);
    assert_eq!(
        seen,
        vec!["src/app.ts".to_string(), "src/users.ts".to_string()]
    );

    // The plain entry point (used everywhere that doesn't care) stays silent.
    let mut cache = IncrementalCache::default();
    extract_dir_incremental(dir.path(), &id(), &mut cache).unwrap();
}

#[test]
fn plain_javascript_and_jsx_files_are_collected_and_parsed() {
    // AC-0095: no separate JS adapter exists — this crate's grammar parses
    // plain JS/JSX directly, so .js/.jsx/.mjs/.cjs are collected right
    // alongside .ts/.tsx.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("index.js"),
        "export function add(a, b) { return a + b; }\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("util.mjs"), "export const x = 1;\n").unwrap();
    std::fs::write(dir.path().join("legacy.cjs"), "module.exports = {};\n").unwrap();
    std::fs::write(
        dir.path().join("Widget.jsx"),
        "export function Widget() { return <div>hi</div>; }\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let files: std::collections::BTreeSet<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "File" && n.props.get("placeholder").is_none())
        .map(|n| n.id.as_str())
        .collect();
    assert_eq!(
        files,
        std::collections::BTreeSet::from([
            "file:qwtm/example@Widget.jsx",
            "file:qwtm/example@index.js",
            "file:qwtm/example@legacy.cjs",
            "file:qwtm/example@util.mjs",
        ])
    );
    // .jsx gets the same JSX-aware Component treatment as .tsx (capitalized,
    // parsed under the TSX grammar).
    let widget = out
        .nodes
        .iter()
        .find(|n| n.id == "sym:qwtm/example@Widget.jsx#Widget")
        .expect("Widget symbol");
    assert_eq!(widget.label, "Component");
}

#[test]
fn extensionless_import_reconciles_to_the_real_non_ts_extension() {
    // The real extension of an extensionless relative import isn't knowable
    // per file (`resolve_relative` guesses `.ts`); once the whole directory
    // is known, both the IMPORTS edge and the cross-file CALLS edge must
    // join the real `.jsx` file's nodes instead of a phantom `.ts`
    // placeholder next to an orphaned real File/Symbol.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("app.js"),
        "import { helper } from './helper';\nexport function run() { helper(); }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("helper.jsx"),
        "export function helper() { return null; }\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let real_file = "file:qwtm/example@helper.jsx";
    let real_symbol = "sym:qwtm/example@helper.jsx#helper";
    let guessed_file = "file:qwtm/example@helper.ts";
    let guessed_symbol = "sym:qwtm/example@helper.ts#helper";

    assert!(out.nodes.iter().any(|n| n.id == real_file));
    assert!(!out.nodes.iter().any(|n| n.id == guessed_file));
    assert!(edge_pairs(&out, "IMPORTS").contains(&("file:qwtm/example@app.js", real_file)));

    assert!(out.nodes.iter().any(|n| n.id == real_symbol));
    assert!(!out.nodes.iter().any(|n| n.id == guessed_symbol));
    assert!(edge_pairs(&out, "CALLS").contains(&("sym:qwtm/example@app.js#run", real_symbol)));
    // No Gap: the call resolved for real, not by falling through to one.
    assert!(!out.nodes.iter().any(|n| n.label == "Gap"));
}

#[test]
fn imported_jsx_component_reconciles_to_the_real_non_ts_extension() {
    // RENDERS edges for an imported component are built eagerly from the
    // same `imported` map as cross-file CALLS (both direct JSX usage and a
    // React Router `element={<Comp/>}`) — they need the same directory-wide
    // extension correction, not just IMPORTS/CALLS.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("App.jsx"),
        r#"
import { Header } from './Header';
export function App() {
  return <Header />;
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("Header.jsx"),
        "export function Header() { return null; }\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let real_symbol = "sym:qwtm/example@Header.jsx#Header";
    let guessed_symbol = "sym:qwtm/example@Header.ts#Header";
    assert!(!out.nodes.iter().any(|n| n.id == guessed_symbol));
    assert!(edge_pairs(&out, "RENDERS").contains(&("sym:qwtm/example@App.jsx#App", real_symbol)));
    assert!(!out.nodes.iter().any(|n| n.label == "Gap"));
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
        .find(|n| n.id == "file:qwtm/example@src/missing.ts")
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
    assert_eq!(
        sites[0].symbol.as_deref(),
        Some("sym:qwtm/example@app.ts#push")
    );
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

// Only immutable `const` bindings promote to literal identities: a `let`
// can be reassigned before the emit, so stamping Confirmed on it would
// fabricate a channel for a runtime value (AC-0012 boundary).
#[test]
fn let_bound_identity_stays_computed() {
    let sites = sites_for(
        r#"
import { EventEmitter } from 'events';
const bus = new EventEmitter();
let topic = 'orders';
function a() { bus.emit(topic); }
"#,
    );
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].identity, IdentityExpr::Computed("topic".into()));
}

// Bracketed env access is the same deterministic config ref as dotted
// access (AC-0011): `process.env['KEY']` resolves, computed keys do not.
#[test]
fn bracketed_env_access_is_an_env_ref() {
    let sites = sites_for(
        r#"
import { EventEmitter } from 'events';
const bus = new EventEmitter();
function a(k: string) {
  bus.emit(process.env['ORDERS_TOPIC']);
  bus.emit(process.env[k]);
}
"#,
    );
    assert_eq!(sites.len(), 2);
    assert_eq!(
        sites[0].identity,
        IdentityExpr::EnvRef("ORDERS_TOPIC".into())
    );
    assert!(matches!(sites[1].identity, IdentityExpr::Computed(_)));
}

// --- Client-side extraction (US-0005) --------------------------------------

use adapters_fw::client::FetchSite;

fn client_extract(path: &str, source: &str) -> Extraction {
    extract_source(source.as_bytes(), path, &id()).unwrap()
}

// AC-0013: a React Router repo yields Screen nodes, Component nodes, and
// RENDERS edges — routes import-proven, components capitalized-in-tsx.
// (T-0013)
#[test]
fn react_router_routes_become_screens_with_renders() {
    let out = client_extract(
        "app.tsx",
        r#"
import { Routes, Route } from 'react-router-dom';
import { Orders } from './orders';

export function App() {
  return (
    <Routes>
      <Route path="/orders" element={<Orders />} />
    </Routes>
  );
}
"#,
    );
    let screen = out
        .nodes
        .iter()
        .find(|n| n.label == "Screen")
        .expect("screen node");
    assert_eq!(screen.id, "screen:qwtm/example@/orders");
    assert_eq!(screen.props["route"], "/orders");
    // The screen renders the routed (imported) component.
    // Import binding resolves to .ts by default (extension-aware
    // resolution rides typed inter-proc work, #2); close-over placeholders
    // keep the edge valid either way.
    assert!(out.edges.iter().any(|e| e.label == "RENDERS"
        && e.src == "screen:qwtm/example@/orders"
        && e.dst == "sym:qwtm/example@orders.ts#Orders"));
    // App itself is a Component (capitalized, tsx).
    let app = out
        .nodes
        .iter()
        .find(|n| n.id == "sym:qwtm/example@app.tsx#App")
        .unwrap();
    assert_eq!(app.label, "Component");
}

// A Route look-alike without the react-router import is not a screen.
#[test]
fn route_component_must_be_import_proven() {
    let out = client_extract(
        "app.tsx",
        r#"
import { Route } from './my-own-router';
export function App() {
  return <Route path="/fake" element={<div />} />;
}
"#,
    );
    assert!(out.nodes.iter().all(|n| n.label != "Screen"));
}

// Component-to-component JSX usage becomes RENDERS; HTML tags do not.
#[test]
fn jsx_usage_becomes_renders_edges() {
    let out = client_extract(
        "page.tsx",
        r#"
import { Header } from './header';
function Body() { return <div>content</div>; }
export function Page() {
  return (
    <main>
      <Header />
      <Body />
    </main>
  );
}
"#,
    );
    let renders: Vec<(&str, &str)> = out
        .edges
        .iter()
        .filter(|e| e.label == "RENDERS")
        .map(|e| (e.src.as_str(), e.dst.as_str()))
        .collect();
    assert!(renders.contains(&(
        "sym:qwtm/example@page.tsx#Page",
        "sym:qwtm/example@header.ts#Header"
    )));
    assert!(renders.contains(&(
        "sym:qwtm/example@page.tsx#Page",
        "sym:qwtm/example@page.tsx#Body"
    )));
    // <main>/<div> never produce edges.
    assert_eq!(renders.len(), 2);
}

// AC-0013 (Next.js half): pages/ files are screens by convention; index
// collapses; _app is chrome. (T-0013)
#[test]
fn next_pages_convention_yields_screens() {
    let dir = tempfile::tempdir().unwrap();
    let pages = dir.path().join("pages");
    std::fs::create_dir_all(pages.join("users")).unwrap();
    std::fs::write(
        pages.join("index.tsx"),
        "export default function Home() { return <div/>; }\n",
    )
    .unwrap();
    std::fs::write(
        pages.join("users").join("[id].tsx"),
        "export default function UserDetail() { return <div/>; }\n",
    )
    .unwrap();
    std::fs::write(
        pages.join("_app.tsx"),
        "export default function MyApp() { return <div/>; }\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let screens: Vec<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Screen")
        .map(|n| n.id.as_str())
        .collect();
    assert!(screens.contains(&"screen:qwtm/example@/"));
    assert!(screens.contains(&"screen:qwtm/example@/users/[id]"));
    assert!(!screens.iter().any(|s| s.contains("_app")));
    // The screen renders the page's default export.
    assert!(out.edges.iter().any(|e| e.label == "RENDERS"
        && e.src == "screen:qwtm/example@/users/[id]"
        && e.dst == "sym:qwtm/example@pages/users/[id].tsx#UserDetail"));
}

#[test]
fn next_pages_convention_covers_plain_js_and_excludes_api_routes() {
    // Real-world Next.js apps commonly write JSX pages in plain .js without
    // renaming to .jsx (AC-0095) — a page under pages/ qualifies the same as
    // .tsx/.jsx. pages/api/* shares the extension but is never a screen.
    let dir = tempfile::tempdir().unwrap();
    let pages = dir.path().join("pages");
    std::fs::create_dir_all(pages.join("api")).unwrap();
    std::fs::write(
        pages.join("about.js"),
        "export default function About() { return <div/>; }\n",
    )
    .unwrap();
    std::fs::write(
        pages.join("api").join("hello.js"),
        "export default function handler(req, res) { res.status(200).end(); }\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let screens: Vec<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Screen")
        .map(|n| n.id.as_str())
        .collect();
    assert_eq!(screens, vec!["screen:qwtm/example@/about"]);
    assert!(out.edges.iter().any(|e| e.label == "RENDERS"
        && e.src == "screen:qwtm/example@/about"
        && e.dst == "sym:qwtm/example@pages/about.js#About"));
}

// Fetch sites classify their URL like channel identities: literal, env
// ref, or computed — and the method comes from the call shape. (T-0014
// groundwork; resolution against endpoints is tested in `events`.)
#[test]
fn fetch_and_axios_sites_are_detected_and_classified() {
    let out = client_extract(
        "api.tsx",
        r#"
import axios from 'axios';
export function Orders() {
  fetch('/api/orders');
  fetch('/api/orders', { method: 'POST', body: '{}' });
  fetch(process.env.API_BASE);
  axios.get('/api/users?page=2');
  axios({ url: '/api/ping', method: 'delete' });
  fetch(buildUrl());
  return <div/>;
}
function buildUrl(): string { return '/x'; }
"#,
    );
    let sites: Vec<(&str, &FetchSite)> = out
        .fetch_sites
        .iter()
        .map(|s| (s.method.as_str(), s))
        .collect();
    assert_eq!(sites.len(), 6);
    // Call-form pass first (fetch + axios object form, document order),
    // then the member-form pass.
    assert!(matches!(&sites[0].1.url, IdentityExpr::Literal(u) if u == "/api/orders"));
    assert_eq!(sites[0].0, "GET");
    assert_eq!(sites[1].0, "POST");
    assert!(matches!(&sites[2].1.url, IdentityExpr::EnvRef(k) if k == "API_BASE"));
    assert_eq!(sites[3].0, "DELETE");
    assert!(matches!(&sites[3].1.url, IdentityExpr::Literal(u) if u == "/api/ping"));
    assert!(matches!(&sites[4].1.url, IdentityExpr::Computed(_)));
    assert_eq!(sites[5].0, "GET");
    assert!(matches!(&sites[5].1.url, IdentityExpr::Literal(u) if u == "/api/users?page=2"));
    // All sites anchor at the enclosing component.
    assert!(
        out.fetch_sites
            .iter()
            .all(|s| s.symbol.as_deref() == Some("sym:qwtm/example@api.tsx#Orders"))
    );
}

// A shadowed `fetch` (imported or locally defined) is application code —
// treating it as the browser API would confirm FETCHES against the wrong
// target and corrupt the graph.
#[test]
fn shadowed_fetch_is_not_a_fetch_site() {
    let imported_shadow = client_extract(
        "a.tsx",
        r#"
import { fetch } from './cache';
export function A() { fetch('/orders'); return <div/>; }
"#,
    );
    assert!(imported_shadow.fetch_sites.is_empty());

    let local_shadow = client_extract(
        "b.tsx",
        r#"
function fetch(url: string) { return url; }
export function B() { fetch('/orders'); return <div/>; }
"#,
    );
    assert!(local_shadow.fetch_sites.is_empty());
}

// Only a direct property of the options object sets the HTTP method — a
// `method` key nested under headers/data does not.
#[test]
fn nested_method_key_does_not_set_the_http_method() {
    let out = client_extract(
        "c.tsx",
        r#"
export function C() {
  fetch('/orders', { headers: { method: 'POST' } });
  return <div/>;
}
"#,
    );
    assert_eq!(out.fetch_sites.len(), 1);
    assert_eq!(out.fetch_sites[0].method, "GET");
}

// A fetch inside an event-handler closure anchors at the component, not
// the closure (SPEC-00 §3.5: FETCHES(component → Endpoint)) — otherwise
// the screen's RENDERS chain can never reach it.
#[test]
fn fetch_in_handler_closure_anchors_at_the_component() {
    let out = client_extract(
        "checkout.tsx",
        r#"
export function Checkout() {
  const submit = () => fetch('/orders', { method: 'POST' });
  return <button onClick={submit}>Order</button>;
}
"#,
    );
    assert_eq!(out.fetch_sites.len(), 1);
    assert_eq!(
        out.fetch_sites[0].symbol.as_deref(),
        Some("sym:qwtm/example@checkout.tsx#Checkout")
    );
}

// A fetch inside a *nested* component belongs to that nested component —
// attributing it to the outer one would confirm FETCHES onto screens that
// never render the child.
#[test]
fn fetch_in_nested_component_anchors_at_the_nearest_component() {
    let out = client_extract(
        "checkout.tsx",
        r#"
export function Checkout() {
  const CouponLookup = () => {
    fetch('/coupons');
    return <input />;
  };
  return <div>checkout</div>;
}
"#,
    );
    assert_eq!(out.fetch_sites.len(), 1);
    assert_eq!(
        out.fetch_sites[0].symbol.as_deref(),
        Some("sym:qwtm/example@checkout.tsx#CouponLookup")
    );
}

#[test]
fn pulumi_aws_constructors_emit_resources_dependencies_and_capabilities() {
    // AC-0051/T-0051: Pulumi remains a T0 language-adapter parse, while AWS
    // relationship semantics come from the same registry as Terraform.
    let source = r#"
import * as aws from '@pulumi/aws';
import * as pulumi from '@pulumi/pulumi';
import { Bucket as LogsBucket } from '@pulumi/aws/s3';

const queue = new aws.sqs.Queue('orders', {});
const logs = new LogsBucket('logs', {});
new aws.s3.Bucket('audit', {});
const archive = new pulumi.asset.FileAsset('archive.zip');
const worker = new aws.lambda.Function('worker', {});
const mapping = new aws.lambda.EventSourceMapping('orders-worker', {
  eventSourceArn: queue.arn,
  functionName: worker.arn,
}, { parent: worker, dependsOn: [queue] });
"#;
    let ex = extract_source(source.as_bytes(), "infra.ts", &id()).unwrap();
    let resources = ex
        .nodes
        .iter()
        .filter(|node| node.label == "Resource")
        .collect::<Vec<_>>();
    assert_eq!(resources.len(), 5);
    assert!(resources.iter().all(|node| {
        node.props["source"] == "pulumi"
            && node.props["prov"]["tier"] == "Deterministic"
            && node.props["prov"]["confidence_tier"] == "Confirmed"
    }));
    let queue = "res:qwtm/example@pulumi:aws:sqs/queue:Queue:orders";
    let logs = "res:qwtm/example@pulumi:aws:s3/bucket:Bucket:logs";
    let audit = "res:qwtm/example@pulumi:aws:s3/bucket:Bucket:audit";
    let worker = "res:qwtm/example@pulumi:aws:lambda/function:Function:worker";
    let mapping =
        "res:qwtm/example@pulumi:aws:lambda/eventSourceMapping:EventSourceMapping:orders-worker";
    assert!(edge_pairs(&ex, "REFERENCES").contains(&(mapping, queue)));
    assert!(edge_pairs(&ex, "REFERENCES").contains(&(mapping, worker)));
    assert!(edge_pairs(&ex, "DEPENDS_ON").contains(&(mapping, queue)));
    assert!(edge_pairs(&ex, "DEPENDS_ON").contains(&(mapping, worker)));
    assert!(edge_pairs(&ex, "TRIGGERS").contains(&(queue, worker)));
    assert!(resources.iter().any(|node| node.id == logs));
    assert!(resources.iter().any(|node| node.id == audit));
    assert!(
        resources
            .iter()
            .all(|node| node.props["logical_id"] != "archive.zip")
    );
    let trigger = ex
        .edges
        .iter()
        .find(|edge| edge.label == "TRIGGERS")
        .unwrap();
    assert_eq!(trigger.props["via"], mapping);
    assert_eq!(trigger.props["registry"], iac::registry::REGISTRY_VERSION);
}

#[test]
fn pulumi_imported_resources_resolve_directory_relationships() {
    // AC-0051/T-0051: relative imports are proven only after the whole
    // directory is recovered, then REFERENCES and registry edges target the
    // imported resources rather than disappearing at the file boundary.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("queue.ts"),
        "import * as aws from '@pulumi/aws';\nexport const queue = new aws.sqs.Queue('orders', {});\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("worker.ts"),
        "import * as aws from '@pulumi/aws';\nexport const worker = new aws.lambda.Function('worker', {});\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("mapping.ts"),
        r#"
import * as aws from '@pulumi/aws';
import { queue } from './queue';
import { worker } from './worker';
new aws.lambda.EventSourceMapping('orders-worker', {
  eventSourceArn: queue.arn,
  functionName: worker.arn,
}, { dependsOn: [queue, worker] });
"#,
    )
    .unwrap();

    let ex = extract_dir(dir.path(), &id()).unwrap();
    let queue = "res:qwtm/example@pulumi:aws:sqs/queue:Queue:orders";
    let worker = "res:qwtm/example@pulumi:aws:lambda/function:Function:worker";
    let mapping =
        "res:qwtm/example@pulumi:aws:lambda/eventSourceMapping:EventSourceMapping:orders-worker";
    assert!(edge_pairs(&ex, "REFERENCES").contains(&(mapping, queue)));
    assert!(edge_pairs(&ex, "REFERENCES").contains(&(mapping, worker)));
    assert!(edge_pairs(&ex, "DEPENDS_ON").contains(&(mapping, queue)));
    assert!(edge_pairs(&ex, "DEPENDS_ON").contains(&(mapping, worker)));
    assert!(edge_pairs(&ex, "TRIGGERS").contains(&(queue, worker)));
}

#[test]
fn pulumi_lookalikes_without_import_proof_are_ignored() {
    // AC-0051/T-0051: constructor spelling is never enough to assert IaC.
    let source = r#"
const aws = makeTestDouble();
const queue = new aws.sqs.Queue('orders', {});
"#;
    let ex = extract_source(source.as_bytes(), "lookalike.ts", &id()).unwrap();
    assert!(ex.nodes.iter().all(|node| node.label != "Resource"));
}

// --- Literal eval()/new Function() extraction (#214, AC-0099) ---------------

// A string-literal eval() is compile-time-known: its Symbols and CALLS are
// Confirmed T0, cite the string's span at the eval site, carry `via: "eval"`,
// and the enclosing symbol CALLS the extracted code's entry symbol so the
// flow tracer walks through the eval boundary. (T-0099)
#[test]
fn literal_eval_extracts_facts_with_eval_site_provenance() {
    let src = r#"
export function boot() {
  eval("function setup() { registerLegacy(); } function registerLegacy() {} setup();");
}
"#;
    let ex = extract_source(src.as_bytes(), "src/boot.ts", &id()).unwrap();
    let calls = edge_pairs(&ex, "CALLS");
    let entry = calls
        .iter()
        .find(|(s, d)| {
            *s == "sym:qwtm/example@src/boot.ts#boot"
                && d.starts_with("sym:qwtm/example@src/boot.ts#eval@")
        })
        .expect("containment CALLS into the eval entry symbol")
        .1;
    // Eval-recovered symbols are namespaced under the entry (#217 review).
    let setup_id = format!("{entry}.setup");
    let register_id = format!("{entry}.registerLegacy");
    let setup = ex
        .nodes
        .iter()
        .find(|n| n.id == setup_id)
        .expect("eval-defined symbol");
    assert_eq!(setup.props["via"], "eval");
    assert_eq!(setup.props["prov"]["tier"], "Deterministic");
    assert_eq!(setup.props["prov"]["confidence_tier"], "Confirmed");
    // Evidence cites the string literal's span at the eval site — the
    // code's true source location.
    let ev = &setup.props["prov"]["evidence"][0];
    let span = &src
        [ev["byte_start"].as_u64().unwrap() as usize..ev["byte_end"].as_u64().unwrap() as usize];
    assert!(
        span.starts_with("\"function setup()"),
        "cites the string at the eval site: {span}"
    );

    assert!(calls.contains(&(entry, setup_id.as_str())));
    assert!(calls.contains(&(setup_id.as_str(), register_id.as_str())));
    let entry_node = ex.nodes.iter().find(|n| n.id == entry).expect("entry node");
    assert_eq!(entry_node.props["via"], "eval");
    assert!(
        entry_node.props["name"]
            .as_str()
            .unwrap()
            .starts_with("<eval@")
    );
    // The site is classified covered — preflight closes its finding.
    assert_eq!(
        ex.eval_sites,
        vec![EvalSite {
            path: "src/boot.ts".into(),
            line: 3,
            proof: EvalProof::Covered,
        }]
    );
}

// #217 review: an eval-defined symbol that shares its name with a real
// outer symbol is a different declaration — the store upserts nodes by id,
// so the eval-recovered fact must live under the site's namespace instead
// of merging with (and clobbering) the outer one. (T-0099)
#[test]
fn eval_defined_symbols_never_clobber_same_named_outer_symbols() {
    let src = r#"
function setup() {}
export function boot() {
  setup();
  eval("function setup() {} setup();");
}
"#;
    let ex = extract_source(src.as_bytes(), "src/app.ts", &id()).unwrap();
    let outer = ex
        .nodes
        .iter()
        .find(|n| n.id == "sym:qwtm/example@src/app.ts#setup")
        .expect("outer symbol keeps its id");
    assert!(outer.props.get("via").is_none(), "outer fact untouched");
    let calls = edge_pairs(&ex, "CALLS");
    let entry = calls
        .iter()
        .find(|(s, d)| {
            *s == "sym:qwtm/example@src/app.ts#boot"
                && d.starts_with("sym:qwtm/example@src/app.ts#eval@")
        })
        .expect("containment CALLS into the eval entry symbol")
        .1;
    let inner_id = format!("{entry}.setup");
    let inner = ex
        .nodes
        .iter()
        .find(|n| n.id == inner_id)
        .expect("eval-defined symbol is namespaced under its site");
    assert_eq!(inner.props["via"], "eval");
    // Each call targets its own declaration: boot's direct call the outer
    // symbol, the eval body's call the eval-scoped one.
    assert!(calls.contains(&(
        "sym:qwtm/example@src/app.ts#boot",
        "sym:qwtm/example@src/app.ts#setup"
    )));
    assert!(calls.contains(&(entry, inner_id.as_str())));
}

// A same-file `const CODE = '…'` and a const-object member proven through
// `const_resolution` are as compile-time-known as an inline literal. (T-0099)
#[test]
fn const_resolved_eval_extracts_through_binding_proof() {
    let src = r#"
const BOOT = "function legacyBoot() {}";
const Scripts = { report: "function legacyReport() {}" } as const;
export function run() {
  eval(BOOT);
  eval(Scripts.report);
}
"#;
    let ex = extract_source(src.as_bytes(), "src/legacy.ts", &id()).unwrap();
    for name in ["legacyBoot", "legacyReport"] {
        let node = ex
            .nodes
            .iter()
            .find(|n| {
                n.id.starts_with("sym:qwtm/example@src/legacy.ts#eval@")
                    && n.id.ends_with(&format!(".{name}"))
            })
            .unwrap_or_else(|| panic!("{name} extracted under its eval namespace"));
        assert_eq!(node.props["name"], *name);
        assert_eq!(node.props["via"], "eval");
        assert_eq!(node.props["prov"]["confidence_tier"], "Confirmed");
    }
    // Both sites prove to literals; `run` calls both entry symbols.
    let claims: Vec<(u64, EvalProof)> = ex.eval_sites.iter().map(|s| (s.line, s.proof)).collect();
    assert_eq!(
        claims,
        vec![(5, EvalProof::Covered), (6, EvalProof::Covered)]
    );
    let entries = edge_pairs(&ex, "CALLS")
        .into_iter()
        .filter(|(s, d)| {
            *s == "sym:qwtm/example@src/legacy.ts#run"
                && d.starts_with("sym:qwtm/example@src/legacy.ts#eval@")
        })
        .count();
    assert_eq!(entries, 2);
}

// Unprovable arguments emit no facts, ever — a const-shaped binding that
// cannot be proven stays an explicit Gap claim, and interpolated templates
// or computed expressions stay dynamic (Unsupported). (T-0099)
#[test]
fn unproven_and_interpolated_eval_yield_classification_but_no_facts() {
    let src = r#"
const code = build();
export function run(name: string) {
  eval(code);
  eval(`register('${name}')`);
  eval(name + '()');
}
"#;
    let ex = extract_source(src.as_bytes(), "src/dynamic.ts", &id()).unwrap();
    assert!(ex.nodes.iter().all(|n| n.props.get("via").is_none()));
    assert!(ex.edges.iter().all(|e| e.props.get("via").is_none()));
    let claims: Vec<(u64, EvalProof)> = ex.eval_sites.iter().map(|s| (s.line, s.proof)).collect();
    assert_eq!(
        claims,
        vec![
            (4, EvalProof::ConstUnproven),
            (5, EvalProof::Dynamic),
            (6, EvalProof::Dynamic),
        ]
    );
}

// A local binding named `eval` is NOT the global — no facts and no claim,
// following the `shadowed_fetch_is_not_a_fetch_site` precedent. (T-0099)
#[test]
fn shadowed_eval_is_not_the_global_and_yields_no_facts() {
    let local_shadow = extract_source(
        br#"
function eval(code) { return code; }
export function run() { eval("function fake() {}"); }
"#,
        "src/sandbox.js",
        &id(),
    )
    .unwrap();
    assert!(
        local_shadow
            .nodes
            .iter()
            .all(|n| n.id != "sym:qwtm/example@src/sandbox.js#fake")
    );
    assert!(local_shadow.eval_sites.is_empty());

    let imported_shadow = extract_source(
        br#"
import { eval } from './interpreter';
export function run() { eval("function fake() {}"); }
"#,
        "src/host.ts",
        &id(),
    )
    .unwrap();
    assert!(
        imported_shadow
            .nodes
            .iter()
            .all(|n| n.id != "sym:qwtm/example@src/host.ts#fake")
    );
    assert!(imported_shadow.eval_sites.is_empty());

    // #217 review: a NON-function local binding shadows too — a const bound
    // to anything, or a parameter — as does a `Function` binding for the
    // constructor form. None of these are the global evaluator.
    for (name, path, source) in [
        (
            "const-bound variable",
            "src/vm.js",
            "const eval = interpret;\nexport function run() { eval(\"function fake() {}\"); }\n",
        ),
        (
            "parameter",
            "src/exec.js",
            "export function run(eval) { eval(\"function fake() {}\"); }\n",
        ),
        (
            "Function constructor binding",
            "src/ctor.js",
            "const Function = makeCtor();\nexport function run() { return new Function(\"return 1;\"); }\n",
        ),
    ] {
        let shadowed = extract_source(source.as_bytes(), path, &id()).unwrap();
        assert!(
            shadowed.nodes.iter().all(|n| n.props.get("via").is_none()),
            "{name} shadow must not extract: {path}"
        );
        assert!(
            shadowed.eval_sites.is_empty(),
            "{name} shadow must not claim: {path}"
        );
    }
}

// new Function("a", "b", "body"): the LAST string argument is the body,
// earlier ones are parameter names; a computed body emits nothing. (T-0099)
#[test]
fn new_function_extracts_the_last_string_argument_as_body() {
    let src = r#"
export function make() {
  return new Function("a", "b", "function helper(x) { return x; } return helper(a) + helper(b);");
}
"#;
    let ex = extract_source(src.as_bytes(), "src/factory.ts", &id()).unwrap();
    let calls = edge_pairs(&ex, "CALLS");
    let entry = calls
        .iter()
        .find(|(s, d)| {
            *s == "sym:qwtm/example@src/factory.ts#make"
                && d.starts_with("sym:qwtm/example@src/factory.ts#eval@")
        })
        .expect("containment CALLS into the body entry symbol")
        .1;
    let helper_id = format!("{entry}.helper");
    let helper = ex
        .nodes
        .iter()
        .find(|n| n.id == helper_id)
        .expect("body-defined symbol");
    assert_eq!(helper.props["via"], "eval");
    // Evidence cites the BODY string (the last argument), not a parameter.
    let ev = &helper.props["prov"]["evidence"][0];
    let span = &src
        [ev["byte_start"].as_u64().unwrap() as usize..ev["byte_end"].as_u64().unwrap() as usize];
    assert!(span.starts_with("\"function helper"), "body span: {span}");
    assert!(calls.contains(&(entry, helper_id.as_str())));
    assert_eq!(
        ex.eval_sites,
        vec![EvalSite {
            path: "src/factory.ts".into(),
            line: 3,
            proof: EvalProof::Covered,
        }]
    );

    // Runtime-computed body: no facts, stays dynamic.
    let dynamic = extract_source(
        br#"
export function make() {
  return new Function(buildBody());
}
"#,
        "src/dyn.ts",
        &id(),
    )
    .unwrap();
    assert!(dynamic.nodes.iter().all(|n| n.props.get("via").is_none()));
    assert_eq!(
        dynamic.eval_sites,
        vec![EvalSite {
            path: "src/dyn.ts".into(),
            line: 3,
            proof: EvalProof::Dynamic,
        }]
    );
}

#[test]
fn directory_index_import_resolves_through_index_files() {
    // #213 (AC-0100): `./utils` meaning `./utils/index.ts` is the Node
    // directory-index convention; the extensionless guess must try the
    // index file before giving up, for IMPORTS and cross-file CALLS alike.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("app.ts"),
        "import { helper } from './utils';\nexport function run() { helper(); }\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("utils")).unwrap();
    std::fs::write(
        dir.path().join("utils/index.ts"),
        "export function helper() {}\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let index_file = "file:qwtm/example@utils/index.ts";
    assert!(edge_pairs(&out, "IMPORTS").contains(&("file:qwtm/example@app.ts", index_file)));
    assert!(
        !out.nodes
            .iter()
            .any(|n| n.id == "file:qwtm/example@utils.ts")
    );
    assert!(edge_pairs(&out, "CALLS").contains(&(
        "sym:qwtm/example@app.ts#run",
        "sym:qwtm/example@utils/index.ts#helper"
    )));
}

#[test]
fn nodenext_js_extension_import_resolves_to_the_ts_source() {
    // #213 (AC-0100): `moduleResolution: nodenext` requires imports to
    // spell the *emitted* `.js` extension — `import './foo.js'` written in
    // a tree that holds `foo.ts`. The spelled file doesn't exist; its TS
    // sibling does, and both the IMPORTS and CALLS edges must join it. A
    // real `.js` file spelled `.js` is left exactly as spelled.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("app.ts"),
        "import { fetchUsers } from './api.js';\nimport { legacy } from './old.js';\nexport function run() { fetchUsers(); legacy(); }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("api.ts"),
        "export function fetchUsers() {}\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("old.js"), "export function legacy() {}\n").unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let imports = edge_pairs(&out, "IMPORTS");
    assert!(imports.contains(&("file:qwtm/example@app.ts", "file:qwtm/example@api.ts")));
    assert!(imports.contains(&("file:qwtm/example@app.ts", "file:qwtm/example@old.js")));
    // No phantom api.js placeholder survives.
    assert!(!out.nodes.iter().any(|n| n.id == "file:qwtm/example@api.js"));
    let calls = edge_pairs(&out, "CALLS");
    assert!(calls.contains(&(
        "sym:qwtm/example@app.ts#run",
        "sym:qwtm/example@api.ts#fetchUsers"
    )));
    assert!(calls.contains(&(
        "sym:qwtm/example@app.ts#run",
        "sym:qwtm/example@old.js#legacy"
    )));
}

#[test]
fn tsconfig_paths_alias_resolves_to_a_real_file_with_the_config_cited() {
    // #213 (AC-0100): a `paths` alias is applied before the specifier is
    // classified as an opaque package, and the tsconfig that decided the
    // resolution is cited in the edge's provenance evidence. Unmatched
    // bare specifiers stay `mod:` nodes exactly as before.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("tsconfig.json"),
        r#"{
  // JSONC on purpose — real tsconfigs carry comments.
  "compilerOptions": {
    "baseUrl": ".",
    "paths": { "@/*": ["src/*"] },
  },
}"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src/components")).unwrap();
    std::fs::write(
        dir.path().join("src/components/Button.tsx"),
        "export function Button() { return null; }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/app.tsx"),
        "import { Button } from '@/components/Button';\nimport react from 'react';\nexport const a = 1;\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let imports: Vec<_> = out.edges.iter().filter(|e| e.label == "IMPORTS").collect();
    let resolved = imports
        .iter()
        .find(|e| e.dst == "file:qwtm/example@src/components/Button.tsx")
        .expect("alias resolves to the real file");
    assert_eq!(resolved.props["resolved_via"], "tsconfig-paths");
    let prov: Provenance = serde_json::from_value(resolved.props["prov"].clone()).unwrap();
    assert!(
        prov.evidence
            .iter()
            .any(|evidence| evidence.path == "tsconfig.json"),
        "the deciding config is citable evidence"
    );
    // External packages stay opaque — resolution never reaches node_modules.
    assert!(imports.iter().any(|e| e.dst == "mod:react"));
    assert!(!imports.iter().any(|e| e.dst == "mod:@/components/Button"));
}

#[test]
fn baseurl_bare_specifier_resolves_without_paths() {
    // #213: an explicit baseUrl alone lets bare specifiers name files
    // relative to it (classic pre-NodeNext TS).
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("tsconfig.json"),
        r#"{ "compilerOptions": { "baseUrl": "src" } }"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src/lib")).unwrap();
    std::fs::write(dir.path().join("src/lib/db.ts"), "export function q() {}\n").unwrap();
    std::fs::write(
        dir.path().join("src/main.ts"),
        "import { q } from 'lib/db';\nexport const a = 1;\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let imports = edge_pairs(&out, "IMPORTS");
    assert!(imports.contains(&(
        "file:qwtm/example@src/main.ts",
        "file:qwtm/example@src/lib/db.ts"
    )));
}

#[test]
fn workspace_package_bare_specifier_resolves_through_its_exports_map() {
    // #213 (AC-0100): a bare specifier naming a sibling workspace package
    // resolves through that package's exports map to its real source, with
    // the package.json cited; anything not in the tree stays `mod:`.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("packages/shared/src")).unwrap();
    std::fs::write(
        dir.path().join("packages/shared/package.json"),
        r#"{ "name": "@acme/shared", "exports": { ".": { "import": "./src/index.ts" } } }"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("packages/shared/src/index.ts"),
        "export function api() {}\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("packages/app")).unwrap();
    std::fs::write(
        dir.path().join("packages/app/main.ts"),
        "import { api } from '@acme/shared';\nimport lodash from 'lodash';\nexport const a = 1;\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let imports: Vec<_> = out.edges.iter().filter(|e| e.label == "IMPORTS").collect();
    let resolved = imports
        .iter()
        .find(|e| e.dst == "file:qwtm/example@packages/shared/src/index.ts")
        .expect("workspace package resolves to its entry source");
    assert_eq!(resolved.props["resolved_via"], "workspace-package");
    let prov: Provenance = serde_json::from_value(resolved.props["prov"].clone()).unwrap();
    assert!(
        prov.evidence
            .iter()
            .any(|evidence| evidence.path == "packages/shared/package.json")
    );
    assert!(imports.iter().any(|e| e.dst == "mod:lodash"));

    // Deterministic: a second walk produces identical edges (US-0014).
    let again = extract_dir(dir.path(), &id()).unwrap();
    assert_eq!(out.edges, again.edges);
}

#[test]
fn unproven_alias_and_missing_index_fail_closed() {
    // #213: resolution is a proof against real files — an alias mapping to
    // nothing and an import with no file and no index stay explicit
    // (`mod:` node / placeholder), never a guessed rewrite.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("tsconfig.json"),
        r#"{ "compilerOptions": { "baseUrl": ".", "paths": { "@/*": ["src/*"] } } }"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app.ts"),
        "import { x } from '@/nothing';\nimport { y } from './missing';\nexport const a = 1;\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let imports: Vec<_> = out.edges.iter().filter(|e| e.label == "IMPORTS").collect();
    assert!(imports.iter().any(|e| e.dst == "mod:@/nothing"));
    // The relative miss keeps the guessed placeholder id (made an explicit
    // placeholder node by close_over_endpoints) — no invented resolution.
    assert!(
        imports
            .iter()
            .any(|e| e.dst == "file:qwtm/example@missing.ts")
    );
    let placeholder = out
        .nodes
        .iter()
        .find(|n| n.id == "file:qwtm/example@missing.ts")
        .expect("placeholder node exists");
    assert_eq!(placeholder.props["placeholder"], true);
}

#[test]
fn nested_tsconfig_shadows_parent_aliases_even_when_empty() {
    // #220 review: the nearest tsconfig governs its files outright — a
    // nested package with its own (alias-free) tsconfig must NOT have the
    // root config's aliases reach into it; the import stays an explicit
    // `mod:` node. Files governed by the root config keep resolving.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("tsconfig.json"),
        r#"{ "compilerOptions": { "baseUrl": ".", "paths": { "@/*": ["src/*"] } } }"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/thing.ts"), "export function t() {}\n").unwrap();
    std::fs::write(
        dir.path().join("root.ts"),
        "import { t } from '@/thing';\nexport const a = 1;\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("packages/app")).unwrap();
    std::fs::write(dir.path().join("packages/app/tsconfig.json"), "{}").unwrap();
    std::fs::write(
        dir.path().join("packages/app/main.ts"),
        "import { t } from '@/thing';\nexport const b = 1;\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let imports = edge_pairs(&out, "IMPORTS");
    // Root-governed file resolves through the root alias…
    assert!(imports.contains(&(
        "file:qwtm/example@root.ts",
        "file:qwtm/example@src/thing.ts"
    )));
    // …but the nested package's identical import is shadowed by its own
    // config and stays unresolved rather than borrowing the root alias.
    assert!(imports.contains(&("file:qwtm/example@packages/app/main.ts", "mod:@/thing")));
}

#[test]
fn most_specific_paths_pattern_wins() {
    // #220 review: TypeScript picks the pattern with the longest prefix
    // before `*` — `@/foo/*` must beat `@/*` even when both targets exist.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("tsconfig.json"),
        r#"{ "compilerOptions": { "baseUrl": ".", "paths": { "@/*": ["src/*"], "@/foo/*": ["special/*"] } } }"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src/foo")).unwrap();
    std::fs::write(
        dir.path().join("src/foo/bar.ts"),
        "export const general = 1;\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("special")).unwrap();
    std::fs::write(
        dir.path().join("special/bar.ts"),
        "export const specific = 1;\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app.ts"),
        "import { specific } from '@/foo/bar';\nexport const a = 1;\n",
    )
    .unwrap();

    let out = extract_dir(dir.path(), &id()).unwrap();
    let imports = edge_pairs(&out, "IMPORTS");
    assert!(imports.contains(&(
        "file:qwtm/example@app.ts",
        "file:qwtm/example@special/bar.ts"
    )));
    assert!(!imports.contains(&(
        "file:qwtm/example@app.ts",
        "file:qwtm/example@src/foo/bar.ts"
    )));
}
