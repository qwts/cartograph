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
