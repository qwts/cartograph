//! Registered identity through the real intake, reader and durable-state seams.

use crate::*;
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use rusqlite::Connection;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn directory(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    std::fs::create_dir_all(&path).unwrap();
    crate::paths::canonicalize(path).unwrap()
}

fn app_state(app_data: &Path) -> AppState {
    let state_path = app_data.join("state.db");
    // Match startup ordering: the registry migration sees the actual findings
    // table, and completes before any new intake can publish registered rows.
    let findings = FindingStore::open(&state_path).unwrap();
    let sources = SourceRegistry::open(&state_path, app_data).unwrap();
    AppState {
        graph: Mutex::new(SqliteGraphStore::open(app_data.join("graph.db")).unwrap()),
        jobs: Mutex::new(JobStore::open(&state_path).unwrap()),
        findings: Mutex::new(findings),
        settings: Mutex::new(settings::SettingsStore::open(&state_path).unwrap()),
        decisions: Mutex::new(agents::DecisionLog::open(&state_path).unwrap()),
        proposals: Mutex::new(
            agents::ProposalStore::open(app_data.join("proposals.sqlite")).unwrap(),
        ),
        extraction_caches: Mutex::new(ExtractionCaches::default()),
        sources: Arc::new(Mutex::new(sources)),
        metrics: Mutex::new(metrics::MetricsStore::open(&state_path).unwrap()),
    }
}

fn recover(state: &AppState, source: &RegisteredSource) -> (DeltaSummary, ReconcileStats) {
    let operation = source_operation(state, vec![(source.clone(), false)]).unwrap();
    let root = operation.root(&source.repo_key).unwrap();
    let (extraction, layers, delta) = {
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_incremental(
            root,
            &source.repo_key,
            "workdir",
            &[],
            &BTreeMap::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
        )
        .unwrap()
    };
    let reconciled = load_into_graph(
        &mut state.graph.lock().unwrap(),
        &extraction,
        &source.repo_key,
        root,
        "workdir",
    )
    .unwrap();
    relink_found_adrs(state, &operation).unwrap();
    let job = state
        .jobs
        .lock()
        .unwrap()
        .enqueue(&source.ingest_job_kind())
        .unwrap();
    record_ingest_metrics(
        state,
        job.id,
        &source.repo_key,
        "workdir",
        &layers,
        &BTreeSet::from([source.repo_key.clone()]),
    )
    .unwrap();
    (delta, reconciled)
}

fn owned_facts(state: &AppState, repo: &str) -> (Vec<Node>, Vec<Edge>) {
    let (nodes, edges) = state.graph.lock().unwrap().read_snapshot().unwrap();
    (
        nodes
            .into_iter()
            .filter(|node| {
                fact_owned_by_repo(&node.props, repo) || id_explicitly_owned_by_repo(&node.id, repo)
            })
            .collect(),
        edges
            .into_iter()
            .filter(|edge| fact_owned_by_repo(&edge.props, repo))
            .collect(),
    )
}

fn serialized(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap()
}

#[test]
fn registered_sources_isolate_intake_findings_and_recovery() {
    // AC-0142: same-basename, initially byte-identical TS sources must not share
    // cache ownership, findings replacement or deletion reconciliation.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let first_root = directory(dir.path(), "first/project");
    let second_root = directory(dir.path(), "second/project");
    for root in [&first_root, &second_root] {
        std::fs::write(root.join("app.ts"),
            "import { helper } from './helper';\nexport function run() { helper(); eval(getCode()); }\n").unwrap();
        std::fs::write(root.join("helper.ts"), "export function helper() {}\n").unwrap();
    }
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let state = app_state(&app_data);
    for root in [&first_root, &second_root] {
        let report = preflight_blocking(root.to_str().unwrap(), app.handle(), &state).unwrap();
        assert!(
            report
                .unsupported
                .iter()
                .any(|finding| finding.kind == "inline-eval")
        );
    }
    let first = state
        .sources
        .lock()
        .unwrap()
        .get_by_root(&first_root)
        .unwrap()
        .unwrap();
    let second = state
        .sources
        .lock()
        .unwrap()
        .get_by_root(&second_root)
        .unwrap()
        .unwrap();
    assert_eq!(first.display_name, second.display_name);
    assert_ne!(first.repo_key, second.repo_key);
    let second_findings = serialized(
        &state
            .findings
            .lock()
            .unwrap()
            .list_for(&second.repo_key)
            .unwrap(),
    );
    assert!(
        !state
            .findings
            .lock()
            .unwrap()
            .list_for(&first.repo_key)
            .unwrap()
            .is_empty()
    );
    assert_eq!(recover(&state, &first).0.recomputed_files, 2);
    assert_eq!(recover(&state, &second).0.recomputed_files, 2);
    assert_eq!(state.extraction_caches.lock().unwrap().repos.len(), 2);
    let first_helper = format!("sym:{}@helper.ts#helper", first.repo_key);
    let second_helper = format!("sym:{}@helper.ts#helper", second.repo_key);
    assert!(
        state
            .graph
            .lock()
            .unwrap()
            .get_node(&first_helper)
            .unwrap()
            .is_some()
    );
    assert!(
        state
            .graph
            .lock()
            .unwrap()
            .get_node(&second_helper)
            .unwrap()
            .is_some()
    );
    let second_facts = owned_facts(&state, &second.repo_key);
    assert!(!second_facts.0.is_empty());
    assert!(!second_facts.1.is_empty());
    let unchanged = recover(&state, &first);
    assert_eq!(unchanged.0.recomputed_files, 0);
    assert_eq!(unchanged.0.reused_files, 2);
    assert_eq!(unchanged.1.inserted_or_updated, 0);
    std::fs::write(
        first_root.join("helper.ts"),
        "export function replacement() {}\n",
    )
    .unwrap();
    let changed = recover(&state, &first);
    assert_eq!(changed.0.recomputed_files, 1);
    assert_eq!(changed.0.reused_files, 1);
    assert!(changed.1.deleted > 0);
    assert!(
        state
            .graph
            .lock()
            .unwrap()
            .get_node(&first_helper)
            .unwrap()
            .is_none()
    );
    assert_eq!(owned_facts(&state, &second.repo_key), second_facts);
    std::fs::remove_file(first_root.join("app.ts")).unwrap();
    assert_eq!(recover(&state, &first).0.deleted_files, 1);
    preflight_blocking(first_root.to_str().unwrap(), app.handle(), &state).unwrap();
    assert!(
        state
            .findings
            .lock()
            .unwrap()
            .list_for(&first.repo_key)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        serialized(
            &state
                .findings
                .lock()
                .unwrap()
                .list_for(&second.repo_key)
                .unwrap()
        ),
        second_findings
    );
    assert_eq!(owned_facts(&state, &second.repo_key), second_facts);
    let latest = state.metrics.lock().unwrap().history(1).unwrap();
    assert_eq!(latest[0].repo, first.repo_key);
    let snapshot = state.graph.lock().unwrap().read_snapshot().unwrap();
    drop(state);
    let restarted = app_state(&app_data);
    assert!(restarted.extraction_caches.lock().unwrap().repos.is_empty());
    assert_eq!(
        register_local_source(&restarted, &first_root)
            .unwrap()
            .source_id,
        first.source_id
    );
    assert_eq!(
        register_local_source(&restarted, &second_root)
            .unwrap()
            .source_id,
        second.source_id
    );
    assert_eq!(
        restarted.graph.lock().unwrap().read_snapshot().unwrap(),
        snapshot
    );
    assert_eq!(
        serialized(
            &restarted
                .findings
                .lock()
                .unwrap()
                .list_for(&second.repo_key)
                .unwrap()
        ),
        second_findings
    );
    assert_eq!(recover(&restarted, &second).0.recomputed_files, 2);
    assert_eq!(owned_facts(&restarted, &second.repo_key), second_facts);
}

fn reference(repo: &str, path: &str, byte_end: u64) -> EvidenceRef {
    EvidenceRef {
        repo: repo.into(),
        path: path.into(),
        byte_start: 0,
        byte_end,
        commit_sha: "historical-unverified".into(),
    }
}

#[test]
fn registered_evidence_reads_never_fall_back() {
    // AC-0143: repository facts may contain tempting old roots; only the exact
    // live host registration authorizes a working-tree read, with no freshness claim.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let first_root = directory(dir.path(), "first/project");
    let second_root = directory(dir.path(), "second/project");
    std::fs::write(first_root.join("source.ts"), "FIRST").unwrap();
    std::fs::write(second_root.join("source.ts"), "OTHER").unwrap();
    let state = app_state(&app_data);
    let first = register_local_source(&state, &first_root).unwrap();
    let second = register_local_source(&state, &second_root).unwrap();
    for repo in [&first.repo_key, &second.repo_key, "local/legacy"] {
        state.graph.lock().unwrap().put_node(&Node {
            id: format!("repo:{repo}"), label: "Repo".into(),
            props: json!({"root": second_root.to_str().unwrap(), "commit": "historical-unverified"}),
        }).unwrap();
    }
    let (nodes, _, reader) = graph_and_reader(&state).unwrap();
    assert_eq!(nodes.len(), 3);
    assert_eq!(
        reader(&reference(&first.repo_key, "source.ts", 5)).as_deref(),
        Some("FIRST")
    );
    assert_eq!(
        reader(&reference(&second.repo_key, "source.ts", 5)).as_deref(),
        Some("OTHER")
    );
    for repo in ["local/legacy", "local/unknown", "project"] {
        assert!(reader(&reference(repo, "source.ts", 5)).is_none());
        let called = std::cell::Cell::new(false);
        assert!(
            source_access::with_registered_read(&state.sources, repo, |_| {
                called.set(true);
                Ok(())
            })
            .is_err()
        );
        assert!(!called.get());
    }
    assert!(
        reader(&reference(
            &first.repo_key,
            second_root.join("source.ts").to_str().unwrap(),
            5
        ))
        .is_none()
    );
    assert!(
        reader(&reference(
            &first.repo_key,
            "../../second/project/source.ts",
            5
        ))
        .is_none()
    );
    assert!(
        reader(&reference(
            &first.repo_key,
            "source.ts",
            evidence::MAX_EVIDENCE_BYTES + 1
        ))
        .is_none()
    );
    std::fs::remove_dir_all(&first_root).unwrap();
    // The previously assembled closure re-reads availability; it cannot retain
    // an old root association or use the other available repository as fallback.
    assert!(reader(&reference(&first.repo_key, "source.ts", 5)).is_none());
    assert!(
        source_access::with_registered_read(&state.sources, &first.repo_key, |_| Ok(())).is_err()
    );
    assert_eq!(
        reader(&reference(&second.repo_key, "source.ts", 5)).as_deref(),
        Some("OTHER")
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&second_root, &first_root).unwrap();
        assert!(reader(&reference(&first.repo_key, "source.ts", 5)).is_none());
    }
}

#[test]
fn repo_facts_keep_operational_roots_out_of_identity() {
    // AC-0144: vary only the operational root passed to the real loader. Full
    // fact equality and aggregate hashes must agree, beyond a field-name check.
    let dir = tempfile::tempdir().unwrap();
    let first_root = directory(dir.path(), "first");
    let second_root = directory(dir.path(), "second");
    std::fs::write(
        first_root.join("source.ts"),
        "export function handle() {}\n",
    )
    .unwrap();
    let repo = "local/src_11111111111111111111111111111111";
    let extraction = extract_tree(
        &first_root,
        repo,
        "revision-one",
        &[],
        &BTreeMap::new(),
        None,
        None,
        &[],
    )
    .unwrap();
    let mut first = SqliteGraphStore::open_in_memory().unwrap();
    let mut second = SqliteGraphStore::open_in_memory().unwrap();
    load_into_graph(&mut first, &extraction, repo, &first_root, "revision-one").unwrap();
    load_into_graph(&mut second, &extraction, repo, &second_root, "revision-one").unwrap();
    assert_eq!(
        first.read_snapshot().unwrap(),
        second.read_snapshot().unwrap()
    );
    assert_eq!(
        deterministic_graph_hashes(&first).unwrap(),
        deterministic_graph_hashes(&second).unwrap()
    );
    let facts = first.read_snapshot().unwrap();
    let original_hash =
        metrics::compute(&facts.0, &facts.1, &BTreeMap::new(), &BTreeSet::new()).content_hash;
    let node = first.get_node(&format!("repo:{repo}")).unwrap().unwrap();
    let props = node.props.as_object().unwrap();
    assert_eq!(
        props.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        BTreeSet::from(["commit", "prov"])
    );
    let fact_bytes = serialized(&node.props);
    assert!(!fact_bytes.contains(first_root.to_str().unwrap()));
    assert!(!fact_bytes.contains(second_root.to_str().unwrap()));
    let unchanged =
        load_into_graph(&mut first, &extraction, repo, &second_root, "revision-one").unwrap();
    assert_eq!(unchanged.inserted_or_updated, 0);
    load_into_graph(&mut second, &extraction, repo, &second_root, "revision-two").unwrap();
    let revised = second.read_snapshot().unwrap();
    let changed_hash =
        metrics::compute(&revised.0, &revised.1, &BTreeMap::new(), &BTreeSet::new()).content_hash;
    assert_ne!(
        original_hash, changed_hash,
        "recovered revision remains part of Repo identity"
    );
}

fn failed_job(state: &AppState, kind: &str) -> Job {
    let mut jobs = state.jobs.lock().unwrap();
    let job = jobs.enqueue(kind).unwrap();
    jobs.set_status(job.id, "running").unwrap();
    jobs.set_progress(job.id, "extract", 42.0).unwrap();
    jobs.fail(job.id, "preserve historical failure").unwrap()
}

#[test]
fn source_bound_retry_preserves_legacy_job_history() {
    // AC-0145: exercise the production preparation seam which resolves the
    // source and guard plan before it calls the mutating JobStore::retry.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "first/project");
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let bound = failed_job(&state, &source.ingest_job_kind());
    let legacy = failed_job(&state, &format!("ingest:{}", root.to_str().unwrap()));
    let unsupported = failed_job(&state, "add-repo:owner/project");
    let system = failed_job(&state, "add-system:system");
    let unknown = failed_job(
        &state,
        "ingest-source-v1:src_00000000000000000000000000000000",
    );
    drop(state);
    let restarted = app_state(&app_data);
    let other_root = directory(dir.path(), "second/project");
    let other = register_local_source(&restarted, &other_root).unwrap();
    assert_ne!(source.source_id, other.source_id);
    for old in [&legacy, &unsupported, &system, &unknown] {
        let error = prepare_job_retry(&restarted, old.id)
            .err()
            .expect("must refuse replay");
        if old.id == legacy.id {
            assert!(error.contains("re-run ingestion"));
        }
        assert_eq!(
            serialized(&restarted.jobs.lock().unwrap().get(old.id).unwrap()),
            serialized(old)
        );
    }
    let (queued, resolved, operation) = prepare_job_retry(&restarted, bound.id).unwrap();
    let resolved = resolved.unwrap();
    assert_eq!(resolved.source_id, source.source_id);
    assert_eq!(resolved.repo_key, source.repo_key);
    assert_eq!(operation.unwrap().root(&source.repo_key).unwrap(), root);
    assert_eq!(queued.id, bound.id);
    assert_eq!(queued.kind, bound.kind);
    assert_eq!(queued.created_at, bound.created_at);
    assert_eq!(queued.status, "queued");
    assert!(queued.error.is_none() && queued.stage.is_none() && queued.progress.is_none());
    let unavailable = failed_job(&restarted, &source.ingest_job_kind());
    std::fs::remove_dir_all(&root).unwrap();
    assert!(prepare_job_retry(&restarted, unavailable.id).is_err());
    assert_eq!(
        serialized(&restarted.jobs.lock().unwrap().get(unavailable.id).unwrap()),
        serialized(&unavailable)
    );
    assert_eq!(restarted.sources.lock().unwrap().list().unwrap().len(), 2);
}

struct ProposalFixtureProvider;

impl llm::LlmProvider for ProposalFixtureProvider {
    fn id(&self) -> &str {
        "fixture:historical-proposal"
    }
    fn locality(&self) -> llm::Locality {
        llm::Locality::Local
    }
    fn capabilities(&self) -> llm::ProviderCaps {
        llm::ProviderCaps {
            embeddings: false,
            chat: true,
            tool_use: false,
        }
    }
    fn embed(&self, _: &[String]) -> Result<Vec<llm::Embedding>, llm::ProviderError> {
        Err(llm::ProviderError::Unsupported("embeddings"))
    }
    fn complete(
        &self,
        _: &llm::ProviderCompletionRequest,
    ) -> Result<llm::Completion, llm::ProviderError> {
        Ok(llm::Completion {
            text: json!({
                "target_id": "sym:local/project@target.ts#target",
                "annotation": "Possible relationship awaiting context reconciliation.",
                "citations": ["source", "target"]
            })
            .to_string(),
        })
    }
}

fn historical_proposal() -> (agents::AgentTask, agents::AgentProposal) {
    let evidence = [
        ("source", "export function source() { target(); }"),
        ("target", "export function target() {}"),
    ]
    .into_iter()
    .map(|(id, text)| agents::AgentEvidence {
        id: id.into(),
        source: reference("local/project", &format!("{id}.ts"), text.len() as u64),
        text: text.into(),
    })
    .collect();
    let task = agents::AgentTask {
        action_id: "historical:run".into(),
        gap_id: "gap:local/project:call".into(),
        source_id: "sym:local/project@source.ts#source".into(),
        edge_label: "CALLS".into(),
        existing_confidence: ConfidenceTier::Gap,
        source_evidence_ids: vec!["source".into()],
        evidence,
        candidates: vec![agents::AgentCandidate {
            node_id: "sym:local/project@target.ts#target".into(),
            label: "Symbol".into(),
            summary: "Candidate target function".into(),
            evidence_ids: vec!["target".into()],
        }],
    };
    let proposal = agents::AgentBroker::bounded_default()
        .propose(
            &ProposalFixtureProvider,
            &llm::EgressFirewall::new(llm::EgressPolicy::local_only()),
            &task,
            None,
        )
        .unwrap();
    (task, proposal)
}

fn staged_bytes(path: &Path, id: &str) -> String {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT immutable_json FROM staged_agent_proposals WHERE proposal_id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn source_identity_migration_preserves_historical_stages() {
    // AC-0146: use actual graph, jobs, findings, metrics, decision and proposal
    // stores. Only disposable schema-three graph facts and known generated
    // findings retire; historical JSON, identities and review revisions survive.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    let state_path = app_data.join("state.db");
    let graph_path = app_data.join("graph.db");
    let proposals_path = app_data.join("proposals.sqlite");
    let mut graph = SqliteGraphStore::open(&graph_path).unwrap();
    let legacy_prov = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        vec![],
        "migration.fixture",
        b"legacy-repo",
    )
    .unwrap();
    let legacy_nodes = vec![
        Node {
            id: "repo:local/project".into(),
            label: "Repo".into(),
            props: json!({"root": root.to_str().unwrap(), "prov": legacy_prov}),
        },
        Node {
            id: "file:local/project@source.ts".into(),
            label: "File".into(),
            props: json!({"prov": legacy_prov}),
        },
    ];
    for node in &legacy_nodes {
        graph.put_node(node).unwrap();
    }
    let legacy_edge = Edge {
        src: legacy_nodes[0].id.clone(),
        dst: legacy_nodes[1].id.clone(),
        label: "CONTAINS".into(),
        props: json!({"prov": legacy_prov}),
    };
    graph.put_edge(&legacy_edge).unwrap();
    drop(graph);
    Connection::open(&graph_path)
        .unwrap()
        .pragma_update(None, "user_version", 3)
        .unwrap();
    let mut findings = FindingStore::open(&state_path).unwrap();
    let finding = |detector: &'static str| NewFinding {
        kind: "unsupported",
        detector,
        path: "source.ts",
        line: 1,
        message: "historical finding",
    };
    findings
        .replace_for(
            "local/project",
            ingest::preflight::DETECTOR_ID,
            &[finding(ingest::preflight::DETECTOR_ID)],
        )
        .unwrap();
    findings
        .replace_for("local/project", "custom@1", &[finding("custom@1")])
        .unwrap();
    let custom = serialized(
        &findings
            .list()
            .unwrap()
            .into_iter()
            .filter(|row| row.detector == "custom@1")
            .collect::<Vec<_>>(),
    );
    let mut jobs = JobStore::open(&state_path).unwrap();
    let job = jobs.enqueue("ingest:/historical/project").unwrap();
    jobs.set_progress(job.id, "extract", 37.0).unwrap();
    jobs.fail(job.id, "historical failure").unwrap();
    let jobs_before = serialized(&jobs.list().unwrap());
    let computed = metrics::compute(
        &legacy_nodes,
        std::slice::from_ref(&legacy_edge),
        &BTreeMap::new(),
        &BTreeSet::new(),
    );
    let mut metrics_store = metrics::MetricsStore::open(&state_path).unwrap();
    metrics_store
        .record(job.id, "local/project", "legacy", &computed, 1, 0)
        .unwrap();
    let metrics_before = serialized(&metrics_store.history(50).unwrap());
    let (task, proposal) = historical_proposal();
    let mut decisions = agents::DecisionLog::open(&state_path).unwrap();
    decisions
        .record(
            &proposal,
            agents::ProposalDecision::Rejected,
            Some("historical decision"),
        )
        .unwrap();
    let assertion = agents::CuratableAssertion {
        subject_id: "historical:assertion".into(),
        summary: "Historical inferred design".into(),
        provenance: Provenance::new(
            Tier::Semantic,
            ConfidenceTier::InferredStrong,
            vec![reference("local/project", "source.ts", 12)],
            "migration.fixture",
            b"historical assertion",
        )
        .unwrap(),
    };
    decisions
        .record_assertion(
            &assertion,
            agents::AssertionDecision::Annotated,
            Some("Keep historical annotation"),
        )
        .unwrap();
    let decisions_before = serialized(&decisions.list().unwrap());
    let assertions_before = serialized(&decisions.list_assertions().unwrap());
    let mut proposals = agents::ProposalStore::open(&proposals_path).unwrap();
    let staged = proposals
        .stage(&task, &proposal, job.id, "snapshot:historical")
        .unwrap();
    proposals
        .review(
            &staged.proposal_id,
            0,
            agents::ProposalDecision::Accepted,
            Some("first review"),
        )
        .unwrap();
    let reviewed = proposals
        .review(
            &staged.proposal_id,
            1,
            agents::ProposalDecision::Rejected,
            Some("second review"),
        )
        .unwrap();
    let immutable_before = staged_bytes(&proposals_path, &staged.proposal_id);
    assert_eq!(reviewed.review_revision, 2);
    drop((findings, jobs, metrics_store, decisions, proposals));
    let state = app_state(&app_data);
    assert_eq!(core_graph::GRAPH_SCHEMA_VERSION, 4);
    assert_eq!(state.graph.lock().unwrap().fact_counts().unwrap(), (0, 0));
    assert_eq!(
        serialized(&state.findings.lock().unwrap().list().unwrap()),
        custom
    );
    assert_eq!(
        serialized(&state.jobs.lock().unwrap().list().unwrap()),
        jobs_before
    );
    assert_eq!(
        serialized(&state.metrics.lock().unwrap().history(50).unwrap()),
        metrics_before
    );
    assert_eq!(
        serialized(&state.decisions.lock().unwrap().list().unwrap()),
        decisions_before
    );
    assert_eq!(
        serialized(&state.decisions.lock().unwrap().list_assertions().unwrap()),
        assertions_before
    );
    assert_eq!(
        state
            .proposals
            .lock()
            .unwrap()
            .get(&staged.proposal_id)
            .unwrap()
            .unwrap(),
        reviewed
    );
    assert_eq!(
        staged_bytes(&proposals_path, &staged.proposal_id),
        immutable_before
    );
    let registered = register_local_source(&state, &root).unwrap();
    assert_ne!(registered.repo_key, "local/project");
    state
        .findings
        .lock()
        .unwrap()
        .replace_for(
            &registered.repo_key,
            ingest::preflight::DETECTOR_ID,
            &[finding(ingest::preflight::DETECTOR_ID)],
        )
        .unwrap();
    drop(state);
    let reopened = app_state(&app_data);
    assert_eq!(reopened.findings.lock().unwrap().list().unwrap().len(), 2);
    assert_eq!(
        reopened
            .proposals
            .lock()
            .unwrap()
            .get(&staged.proposal_id)
            .unwrap()
            .unwrap(),
        reviewed
    );
    reopened.graph.lock().unwrap().clear().unwrap();
    assert_eq!(reopened.jobs.lock().unwrap().clear_finished().unwrap(), 1);
    assert_eq!(
        reopened
            .sources
            .lock()
            .unwrap()
            .get_by_repo(&registered.repo_key)
            .unwrap()
            .unwrap()
            .source_id,
        registered.source_id
    );
    assert_eq!(
        staged_bytes(&proposals_path, &staged.proposal_id),
        immutable_before
    );
    assert_eq!(
        serialized(&reopened.decisions.lock().unwrap().list().unwrap()),
        decisions_before
    );
}

#[test]
fn managed_readiness_batch_rolls_back_on_late_invalid_member() {
    // AC-0147: neither unavailable nor ready publication can expose a prefix
    // when a later member fails validation, including after reopening state.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let local_root = directory(dir.path(), "direct");
    let state = app_state(&app_data);
    let first = state
        .sources
        .lock()
        .unwrap()
        .reserve_managed("https://github.com/fixture/first")
        .unwrap();
    let second = state
        .sources
        .lock()
        .unwrap()
        .reserve_managed("https://github.com/fixture/second")
        .unwrap();
    let local = register_local_source(&state, &local_root).unwrap();
    // Availability is the transaction under test. These host fixture folders
    // are not a clone/owner-guard attestation and no source read is performed.
    std::fs::create_dir_all(first.root()).unwrap();
    std::fs::create_dir_all(second.root()).unwrap();
    let ids = vec![first.source_id.clone(), second.source_id.clone()];
    state
        .sources
        .lock()
        .unwrap()
        .set_ready_batch(&ids, true)
        .unwrap();
    assert!(
        state
            .sources
            .lock()
            .unwrap()
            .set_ready_batch(&[first.source_id.clone(), local.source_id.clone()], false)
            .is_err()
    );
    for id in &ids {
        assert!(
            state
                .sources
                .lock()
                .unwrap()
                .get_by_id(id)
                .unwrap()
                .unwrap()
                .is_ready()
        );
    }
    state
        .sources
        .lock()
        .unwrap()
        .set_ready_batch(&ids, false)
        .unwrap();
    assert!(
        state
            .sources
            .lock()
            .unwrap()
            .set_ready_batch(
                &[
                    first.source_id.clone(),
                    "src_00000000000000000000000000000000".into()
                ],
                true
            )
            .is_err()
    );
    drop(state);
    let restarted = app_state(&app_data);
    for id in &ids {
        assert!(
            !restarted
                .sources
                .lock()
                .unwrap()
                .get_by_id(id)
                .unwrap()
                .unwrap()
                .is_ready()
        );
    }
    restarted
        .sources
        .lock()
        .unwrap()
        .set_ready_batch(&ids, true)
        .unwrap();
    for id in &ids {
        assert!(
            restarted
                .sources
                .lock()
                .unwrap()
                .get_by_id(id)
                .unwrap()
                .unwrap()
                .is_ready()
        );
    }
}

fn isolated_managed_operation_case(name: &str) -> bool {
    use std::io::Read;
    use std::process::{Child, Command, Stdio};
    const MARKER: &str = "CARTOGRAPH_REGISTERED_OPERATION_CASE";
    let exact = format!("registered_source_tests::{name}");
    if std::env::var(MARKER).is_ok_and(|value| value == exact) {
        return false;
    }
    // Parallel fixtures spawn Git processes. CLOEXEC closes inherited lock
    // handles only at exec, so open this fixture's locks after its own exec.
    struct Cleanup(Child);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Cleanup(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &exact, "--nocapture", "--test-threads=1"])
            .env(MARKER, &exact)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut stdout = child.0.stdout.take().unwrap();
    let mut stderr = child.0.stderr.take().unwrap();
    let stdout = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let stderr = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let status = child.0.wait().unwrap();
    let stdout = stdout.join().unwrap();
    let stderr = stderr.join().unwrap();
    assert!(
        status.success(),
        "isolated {exact} failed:\n{}\n{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    true
}

fn ready_managed_source(state: &AppState, name: &str) -> RegisteredSource {
    let source = state
        .sources
        .lock()
        .unwrap()
        .reserve_managed(&format!("https://github.com/fixture/{name}"))
        .unwrap();
    let managed = source.managed().unwrap().unwrap();
    let guard = managed.try_write().unwrap();
    // The real slot initializer owns this destination; an empty Git checkout
    // suffices to exercise the actual host acquisition/validation boundary.
    std::fs::create_dir(guard.root()).unwrap();
    let output = std::process::Command::new("git")
        .args(["-c", "init.templateDir=", "init", "--quiet"])
        .arg(guard.root())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git fixture init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::write(guard.root().join("keep.ts"), "retained checkout canary").unwrap();
    drop(guard);
    state
        .sources
        .lock()
        .unwrap()
        .set_ready(&source.source_id, true)
        .unwrap();
    let source = state
        .sources
        .lock()
        .unwrap()
        .get_by_id(&source.source_id)
        .unwrap()
        .unwrap();
    assert!(source.is_ready());
    source
}

fn ready_after_reopen(app_data: &Path, source: &RegisteredSource) -> bool {
    SourceRegistry::open(app_data.join("state.db"), app_data)
        .unwrap()
        .get_by_id(&source.source_id)
        .unwrap()
        .unwrap()
        .is_ready()
}

#[test]
fn managed_plugin_gate_retains_source_guards_through_verdict() {
    if isolated_managed_operation_case("managed_plugin_gate_retains_source_guards_through_verdict")
    {
        return;
    }
    // AC-0147: exercise the real discovery/gate pipeline. Synchronous host
    // events place replacement attempts between the WASM and corpus reads,
    // before verdict storage, and after its durable publication. No timer or
    // test-only pipeline hook determines the ordering.
    use tauri::Listener;
    const PLUGIN: &str = "t0.plugin-fixture";
    const WASM: &[u8] =
        include_bytes!("../../crates/adapters-plugin-host/tests/fixtures/compiled/ok-adapter.wasm");
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let state = app_state(&app_data);
    let source = ready_managed_source(&state, "plugin-gate");
    let adapters = source.root().join(".cartograph/adapters");
    std::fs::create_dir_all(&adapters).unwrap();
    std::fs::write(adapters.join(format!("{PLUGIN}.wasm")), WASM).unwrap();
    let corpus_path = adapters.join(format!("{PLUGIN}.golden.json"));
    std::fs::write(
        &corpus_path,
        json!({
            "extensions": ["foo"],
            "cases": [{
                "path": "src/lib.rs",
                "source": "hello world",
                "nodes": [{"id":"golden:src/lib.rs","label":"TestNode","props":{"len":11}}],
                "edges": [{"src":"golden:src/lib.rs","dst":"golden:src/lib.rs","label":"SELF","props":{}}]
            }]
        })
        .to_string(),
    )
    .unwrap();
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(state);
    let state = app.state::<AppState>();
    let hash = core_prov::content_hash(WASM);

    for corpus_present in [true, false] {
        if !corpus_present {
            // The preceding gate released its guard. Change the fixture under
            // the real exclusive source lock before testing a failed verdict.
            let guard = source.managed().unwrap().unwrap().try_write().unwrap();
            std::fs::remove_file(&corpus_path).unwrap();
            drop(guard);
        }
        let job_id = {
            let mut jobs = state.jobs.lock().unwrap();
            let job = jobs.enqueue(&format!("plugin-gate:{PLUGIN}")).unwrap();
            jobs.set_status(job.id, "running").unwrap();
            job.id
        };
        let other_registry =
            Mutex::new(SourceRegistry::open(app_data.join("state.db"), &app_data).unwrap());
        let observed = Arc::new(Mutex::new(Vec::new()));
        let observations = Arc::clone(&observed);
        let source_for_attempt = source.clone();
        let event_id = app.listen("job://changed", move |event| {
            let job: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
            if job["id"].as_i64() != Some(job_id) {
                return;
            }
            let boundary = if job["status"] == "done" {
                "done"
            } else {
                match job["stage"].as_str() {
                    Some("gate") => "gate",
                    Some("record") => "record",
                    _ => return,
                }
            };
            // A second registry connection represents another participating
            // host operation. Busy must occur before it invalidates readiness.
            let blocked = matches!(
                SourceOperation::acquire(
                    &other_registry,
                    [(source_for_attempt.clone(), true)],
                ),
                Err(error) if error.contains("busy")
            );
            let ready = other_registry
                .lock()
                .unwrap()
                .get_by_id(&source_for_attempt.source_id)
                .unwrap()
                .unwrap()
                .is_ready();
            observations
                .lock()
                .unwrap()
                .push((boundary.to_string(), blocked, ready));
        });

        let report = plugin_gate_blocking(PLUGIN, job_id, app.handle()).unwrap();
        app.unlisten(event_id);
        assert_eq!(report["passed"], json!(corpus_present));
        assert_eq!(
            *observed.lock().unwrap(),
            vec![
                ("gate".to_string(), true, true),
                ("record".to_string(), true, true),
                ("done".to_string(), true, true),
            ]
        );
        let stored = state
            .settings
            .lock()
            .unwrap()
            .plugin_gate(PLUGIN, &hash)
            .unwrap()
            .unwrap();
        assert_eq!(stored.0, corpus_present);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stored.1).unwrap(),
            report
        );
        assert!(ready_after_reopen(&app_data, &source));
        // Both successful and failed verdicts release the owned guard on
        // return; this process spawns no children while the gate holds it.
        let released = source.managed().unwrap().unwrap().try_write().unwrap();
        drop(released);
    }
}

#[test]
fn managed_operation_invalidates_ready_source_before_checkout_validation() {
    if isolated_managed_operation_case(
        "managed_operation_invalidates_ready_source_before_checkout_validation",
    ) {
        return;
    }
    // AC-0147: re-add of a previously ready but corrupt checkout fails only
    // after durable unavailability; an existing directory cannot revive it.
    let directory_root = tempfile::tempdir().unwrap();
    let app_data = directory(directory_root.path(), "private");
    let state = app_state(&app_data);
    let source = ready_managed_source(&state, "corrupt");
    std::fs::remove_dir_all(source.root().join(".git")).unwrap();
    assert!(
        ready_after_reopen(&app_data, &source),
        "the old ready flag is observable before re-add"
    );
    assert!(SourceOperation::acquire(&state.sources, [(source.clone(), true)]).is_err());
    assert!(!ready_after_reopen(&app_data, &source));
    assert_eq!(
        std::fs::read_to_string(source.root().join("keep.ts")).unwrap(),
        "retained checkout canary"
    );
    assert!(
        source_access::with_registered_read(&state.sources, &source.repo_key, |_| Ok(())).is_err()
    );
}

#[test]
fn managed_operation_busy_lock_preserves_all_ready_flags() {
    if isolated_managed_operation_case("managed_operation_busy_lock_preserves_all_ready_flags") {
        return;
    }
    // AC-0147: a later busy lock means no full reservation was obtained. The
    // earlier reserved source remains ready, and its temporary handle releases.
    let directory_root = tempfile::tempdir().unwrap();
    let app_data = directory(directory_root.path(), "private");
    let state = app_state(&app_data);
    let mut sources = [
        ready_managed_source(&state, "first"),
        ready_managed_source(&state, "second"),
    ];
    sources.sort_by(|a, b| a.source_id.cmp(&b.source_id));
    let busy = sources[1]
        .managed()
        .unwrap()
        .unwrap()
        .try_reserve_write()
        .unwrap();
    let result = SourceOperation::acquire(
        &state.sources,
        sources.iter().cloned().map(|source| (source, true)),
    );
    assert!(matches!(result, Err(error) if error.contains("busy")));
    for source in &sources {
        assert!(ready_after_reopen(&app_data, source));
    }
    let released = sources[0]
        .managed()
        .unwrap()
        .unwrap()
        .try_reserve_write()
        .unwrap();
    drop(released);
    drop(busy);
}

#[test]
fn managed_operation_late_validation_failure_keeps_all_writes_unavailable() {
    if isolated_managed_operation_case(
        "managed_operation_late_validation_failure_keeps_all_writes_unavailable",
    ) {
        return;
    }
    // AC-0147: once every lock is reserved, all planned writes become unavailable
    // atomically before either write initialization or read validation can fail.
    let directory_root = tempfile::tempdir().unwrap();
    let app_data = directory(directory_root.path(), "private");
    let state = app_state(&app_data);
    let mut sources = [
        ready_managed_source(&state, "first"),
        ready_managed_source(&state, "second"),
    ];
    sources.sort_by(|a, b| a.source_id.cmp(&b.source_id));
    std::fs::remove_dir_all(sources[1].root().join(".git")).unwrap();
    assert!(
        SourceOperation::acquire(
            &state.sources,
            sources.iter().cloned().map(|source| (source, true))
        )
        .is_err()
    );
    for source in &sources {
        assert!(!ready_after_reopen(&app_data, source));
        assert_eq!(
            std::fs::read_to_string(source.root().join("keep.ts")).unwrap(),
            "retained checkout canary"
        );
    }
    let ids: Vec<_> = sources
        .iter()
        .map(|source| source.source_id.clone())
        .collect();
    state
        .sources
        .lock()
        .unwrap()
        .set_ready_batch(&ids, true)
        .unwrap();
    // The corrupt later source may also be an ADR read member. Its failed read
    // validation must not leave the already-reserved write source marked ready.
    assert!(
        SourceOperation::acquire(
            &state.sources,
            [(sources[0].clone(), true), (sources[1].clone(), false)]
        )
        .is_err()
    );
    assert!(!ready_after_reopen(&app_data, &sources[0]));
    assert!(
        ready_after_reopen(&app_data, &sources[1]),
        "read-only members do not mutate readiness"
    );
}

#[cfg(unix)]
#[test]
fn managed_origin_rebinding_is_rejected_before_clone() {
    // AC-0141, AC-0147: a retained file origin cannot be redirected to a new
    // canonical path between admission and the guarded clone attempt.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let origin = directory(dir.path(), "origin");
    let other = directory(dir.path(), "unrelated");
    std::fs::write(other.join("keep.ts"), "unrelated source content").unwrap();
    let mut url = "file://".to_string();
    for byte in origin.to_str().unwrap().bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            url.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(&mut url, "%{byte:02X}").unwrap();
        }
    }
    let state = app_state(&app_data);
    let source = state.sources.lock().unwrap().reserve_managed(&url).unwrap();
    let mut operation = SourceOperation::acquire(&state.sources, [(source.clone(), true)]).unwrap();
    std::fs::rename(&origin, dir.path().join("moved-origin")).unwrap();
    std::os::unix::fs::symlink(&other, &origin).unwrap();
    let error = operation
        .clone_source(&source, None)
        .expect_err("changed origin must fail");
    assert!(error.contains("registered clone origin changed"));
    assert!(!source.root().exists());
    assert_eq!(
        std::fs::read_to_string(other.join("keep.ts")).unwrap(),
        "unrelated source content"
    );
    drop(operation);
    let retained = state
        .sources
        .lock()
        .unwrap()
        .get_by_id(&source.source_id)
        .unwrap()
        .unwrap();
    assert_eq!(retained.clone_url(), source.clone_url());
    assert!(!retained.is_ready());
}

#[test]
fn registered_git_worktrees_keep_distinct_identity_across_dirty_state() {
    // AC-0140: real Git worktrees share history and a common Git directory but
    // remain distinct logical sources, both before and after one becomes dirty.
    fn git(root: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(["-c", "commit.gpgsign=false", "-c", "init.templateDir="])
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "Cartograph fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Cartograph fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_COMMON_DIR")
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    // Include directory entries, working files, the linked .git pointer, and
    // every common/worktree Git metadata file. Status runs use optional-locks=0
    // and all Git commands precede each snapshot comparison.
    fn tree_bytes(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        let mut snapshot = BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let kind = entry.file_type().unwrap();
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                if kind.is_dir() {
                    snapshot.insert(relative, None);
                    pending.push(path);
                } else {
                    assert!(kind.is_file(), "fixture contains an unexpected file type");
                    snapshot.insert(relative, Some(std::fs::read(path).unwrap()));
                }
            }
        }
        snapshot
    }

    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let checkout = directory(dir.path(), "main/project");
    let linked_parent = directory(dir.path(), "linked");
    let worktree = linked_parent.join("project");
    std::fs::write(checkout.join("app.ts"), "export const value = 1;\n").unwrap();
    git(&checkout, &["init", "-q", "-b", "main"]);
    git(&checkout, &["add", "app.ts"]);
    git(&checkout, &["commit", "-q", "-m", "offline fixture"]);
    git(
        &checkout,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            worktree.to_str().unwrap(),
            "HEAD",
        ],
    );
    let worktree = crate::paths::canonicalize(worktree).unwrap();
    assert_eq!(checkout.file_name(), worktree.file_name());
    assert!(checkout.join(".git").is_dir());
    assert!(worktree.join(".git").is_file());
    let common = |root: &Path| {
        let value = git(root, &["rev-parse", "--git-common-dir"]);
        crate::paths::canonicalize(root.join(value.trim())).unwrap()
    };
    assert_eq!(common(&checkout), common(&worktree));
    assert_eq!(
        git(&checkout, &["rev-parse", "HEAD"]),
        git(&worktree, &["rev-parse", "HEAD"])
    );
    assert!(git(&checkout, &["status", "--porcelain=v1"]).is_empty());
    assert!(git(&worktree, &["status", "--porcelain=v1"]).is_empty());
    let clean_checkout = tree_bytes(&checkout);
    let clean_worktree = tree_bytes(&worktree);
    let state_path = app_data.join("state.db");
    let mut registry = SourceRegistry::open(&state_path, &app_data).unwrap();
    let first = registry.register_local(&checkout).unwrap();
    let second = registry.register_local(&worktree).unwrap();
    assert_ne!(first.source_id, second.source_id);
    assert_ne!(first.repo_key, second.repo_key);
    assert_eq!(tree_bytes(&checkout), clean_checkout);
    assert_eq!(tree_bytes(&worktree), clean_worktree);

    std::fs::write(worktree.join("app.ts"), "export const value = 2;\n").unwrap();
    assert!(git(&checkout, &["status", "--porcelain=v1"]).is_empty());
    assert!(git(&worktree, &["status", "--porcelain=v1"]).contains("M app.ts"));
    assert_eq!(
        git(&checkout, &["rev-parse", "HEAD"]),
        git(&worktree, &["rev-parse", "HEAD"])
    );
    let before_checkout = tree_bytes(&checkout);
    let before_worktree = tree_bytes(&worktree);
    assert_eq!(
        registry.register_local(&worktree).unwrap().source_id,
        second.source_id
    );
    drop(registry);
    let mut reopened = SourceRegistry::open(&state_path, &app_data).unwrap();
    assert_eq!(
        reopened
            .register_local(&checkout.join("."))
            .unwrap()
            .source_id,
        first.source_id
    );
    assert_eq!(
        reopened
            .register_local(&worktree.join("."))
            .unwrap()
            .source_id,
        second.source_id
    );
    #[cfg(unix)]
    {
        let alias = dir.path().join("worktree-alias");
        std::os::unix::fs::symlink(&worktree, &alias).unwrap();
        assert_eq!(
            reopened.register_local(&alias).unwrap().source_id,
            second.source_id
        );
    }
    assert_eq!(reopened.list().unwrap().len(), 2);
    assert_eq!(
        reopened
            .get_by_repo(&first.repo_key)
            .unwrap()
            .unwrap()
            .root(),
        checkout
    );
    assert_eq!(
        reopened
            .get_by_repo(&second.repo_key)
            .unwrap()
            .unwrap()
            .root(),
        worktree
    );
    assert_eq!(tree_bytes(&checkout), before_checkout);
    assert_eq!(tree_bytes(&worktree), before_worktree);
}
