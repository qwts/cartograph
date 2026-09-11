//! Production capture, publication, inspection and retention across app restarts.

use crate::registered_source_tests::{app_state, historical_proposal, staged_bytes};
use crate::*;
use adapters_lang_ts::captured::Receipt;
use core_graph::rules::{GuardedExitEvidence, InterpretationStatus};
use core_graph::source::{FactKey, SourceBinding};
use core_prov::{ConfidenceTier, Provenance, Tier};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const ORIGINAL: &str = concat!(
    "// café — 🧭\n",
    "export function retained(enabled: boolean) {\n",
    "  if (enabled /* primary-comment-canary */) return { ",
    "password: \"primary-password-canary\", token: \"\\x67hp_primarycapture1234\" };\n",
    "  if (!enabled) return false;\n",
    "}\n",
    "// primary-trailing-source-canary\n",
);
const STABLE_A: &str =
    "export function stable(enabled: boolean) { if (enabled) return false; }\n// trailing-A\n";
const STABLE_B: &str =
    "export function stable(enabled: boolean) { if (enabled) return false; }\n// trailing-B\n";

fn directory(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    std::fs::create_dir_all(&path).unwrap();
    crate::paths::canonicalize(path).unwrap()
}

struct Parsed {
    // The production retention guard must survive persistence and publication.
    input: primary_source::PrimaryInput,
    extraction: adapters_lang_ts::Extraction,
    receipts: Vec<Receipt>,
    delta: DeltaSummary,
}

fn parse(state: &AppState, source: &RegisteredSource, after_capture: impl FnOnce()) -> Parsed {
    // Application-only selection avoids making claims about auxiliary adapters'
    // live config reads. This is the actual shared production extraction branch.
    let layers = vec!["client".to_string()];
    let input = state
        .primary_sources
        .prepare(source, source.root(), &layers)
        .unwrap();
    assert!(input.capture.is_some());
    after_capture();
    let mut receipts = Vec::new();
    let (extraction, _, delta) = {
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_with_primary(
            source.root(),
            &source.repo_key,
            "workdir",
            &layers,
            &BTreeMap::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
            input.capture.as_ref(),
            &mut receipts,
        )
        .unwrap()
    };
    assert!(!receipts.is_empty());
    Parsed {
        input,
        extraction,
        receipts,
        delta,
    }
}

fn publish(state: &AppState, source: &RegisteredSource, parsed: &Parsed) -> Vec<SourceBinding> {
    state
        .primary_sources
        .persist(&parsed.input, source, &parsed.receipts)
        .unwrap();
    let bindings = primary_source::matching_bindings(&parsed.extraction, &parsed.receipts);
    load_into_graph_with_bindings(
        &mut state.graph.lock().unwrap(),
        &parsed.extraction,
        &source.repo_key,
        source.root(),
        "workdir",
        &bindings,
    )
    .unwrap();
    bindings
}

fn rule_node(parsed: &Parsed) -> Node {
    parsed
        .extraction
        .nodes
        .iter()
        .find(|node| node.label == "BusinessRule")
        .unwrap()
        .clone()
}

fn assert_original_ranges(
    state: &AppState,
    description: &primary_source::CapturedDescription,
    original: &str,
) {
    assert_eq!(description.scope, "primary_source_only");
    assert_eq!(description.input_closure, "input_closure_not_established");
    assert!(!description.ranges.is_empty());
    for range in &description.ranges {
        let read = primary_source::read(
            state,
            &description.fact,
            &description.receipt_id,
            range.index,
        )
        .unwrap();
        assert_eq!(read.receipt_id, description.receipt_id);
        assert_eq!(read.range_index, range.index);
        assert_eq!(read.path, "source.ts");
        assert_eq!(read.path, range.path);
        assert_eq!(
            (read.byte_start, read.byte_end),
            (range.byte_start, range.byte_end)
        );
        assert_eq!(
            read.text.as_bytes(),
            &original.as_bytes()[range.byte_start as usize..range.byte_end as usize]
        );
    }
}

fn receipt_rows(app_data: &Path) -> Vec<(String, String, String, String)> {
    let conn = rusqlite::Connection::open_with_flags(
        app_data.join("retained-source/receipts.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut query = conn
        .prepare("SELECT id,source_id,repo_key,payload FROM receipts ORDER BY id")
        .unwrap();
    query
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn proposal_page(state: &AppState) -> serde_json::Value {
    serde_json::to_value(state.proposals.lock().unwrap().list(20, None).unwrap()).unwrap()
}

/// Publish an actual captured fact's selected binding through the durable
/// coordinator before reading any source. This exercises retention's JobStore
/// seam without fabricating a source citation or invoking a provider.
fn retain_investigation_selection(
    state: &AppState,
    source: &RegisteredSource,
    binding: &SourceBinding,
    nonce: &str,
) -> (String, agents::investigation::InvestigationInputLedger) {
    use crate::jobs::investigations::{InvestigationStopReason, InvestigationTransition};
    use agents::investigation::*;
    use agents::{TaskFactSelection, TaskSourceAssociation};
    let (nodes, edges) = state.graph.lock().unwrap().read_snapshot().unwrap();
    let snapshot = context_hub::ContextSnapshot::new(nodes, edges).unwrap();
    let page = snapshot
        .query(context_hub::QueryRequest {
            scope: context_hub::QueryScope::All,
            kind: Some(context_hub::FactKind::Node),
            labels: vec!["BusinessRule".into()],
            max_facts: 32,
            max_bytes: 32 * 1024,
            cursor: None,
        })
        .unwrap();
    let raw = serde_json::to_vec(&page).unwrap();
    let fact = crate::task_evidence::task_fact(&binding.fact);
    let ledger = InvestigationInputLedger {
        schema_version: 1,
        graph_snapshot_id: snapshot.id().into(),
        scope_snapshot_id: snapshot.id().into(),
        revision: 1,
        selected_facts: vec![TaskFactSelection {
            fact: fact.clone(),
            fact_digest: binding.emitted_fact_digest.clone(),
            binding: Some(TaskSourceAssociation {
                repo_key: binding.repo_key.clone(),
                receipt_id: binding.receipt_id.clone(),
                emitted_fact_digest: binding.emitted_fact_digest.clone(),
            }),
        }],
        receipt_references: vec![InvestigationReceiptReference {
            source_id: source.source_id.clone(),
            repo_key: binding.repo_key.clone(),
            receipt_id: binding.receipt_id.clone(),
        }],
        citations: vec![InvestigationCitation {
            citation_id: "fact-1".into(),
            fact,
            fact_digest: binding.emitted_fact_digest.clone(),
            source: None,
            role: None,
            index: None,
            text_hash: None,
            origin: InvestigationEvidenceOrigin::GraphMetadata,
        }],
        queries: vec![InvestigationQueryManifest {
            query: InvestigationQuery {
                scope: InvestigationScope::All,
                kind: Some(InvestigationFactKind::Node),
                labels: vec!["BusinessRule".into()],
                max_facts: 32,
                max_bytes: 32 * 1024,
                cursor: None,
            },
            response_hash: core_prov::content_hash(&raw),
            response_bytes: raw.len(),
            returned_facts: page.facts.len(),
            total_selected: page.total_selected,
            has_more: page.next_cursor.is_some(),
        }],
        supplied_history_hash: None,
        supplied_history_bytes: 0,
    };
    ledger.validate().unwrap();
    let request = StartInvestigationRequest {
        schema_version: 1,
        request_nonce: nonce.into(),
        specialist_id: SpecialistId::DomainAnalyst,
        question: "Which recovered rule needs further evidence?".into(),
        scope: InvestigationScope::All,
        provider_mode: InvestigationProviderMode::Local,
        limit_profile: "investigation-v1".into(),
        expected_graph_revision: Some(snapshot.id().into()),
        conversation_id: None,
        parent_id: None,
    };
    let provider = InvestigationProvider {
        mode: InvestigationProviderMode::Local,
        provider_id: "local:retention-fixture".into(),
        model: "fixture".into(),
        endpoint: "http://127.0.0.1:11434".into(),
        deployment: None,
        available: true,
        unavailable_reason: None,
    };
    let detail = state
        .jobs
        .lock()
        .unwrap()
        .start_investigation(&request, Some(&provider))
        .unwrap()
        .detail;
    let plan = state
        .jobs
        .lock()
        .unwrap()
        .claim_plan(detail.summary.job_id, ClaimMode::StartQueued)
        .unwrap();
    let (_, execution) = claim_job(state, &plan).unwrap();
    let id = detail.summary.investigation_id;
    {
        // The same source-guard → short JobStore ordering as production input
        // acquisition, retaining the lease until its immutable index is durable.
        let guard = state.primary_sources.task_guard(&source.source_id).unwrap();
        state
            .primary_sources
            .task_receipt(
                &guard,
                &binding.fact,
                &binding.emitted_fact_digest,
                &binding.repo_key,
                &binding.receipt_id,
            )
            .unwrap();
        let mut jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&id).unwrap();
        let admitted = jobs
            .advance_investigation(
                &execution,
                detail.summary.revision,
                InvestigationTransition::InputPrepared {
                    ledger: Box::new(ledger.clone()),
                    usage: InvestigationUsage {
                        selected_facts: 1,
                        ..detail.usage
                    },
                },
            )
            .unwrap();
        assert_eq!(admitted.usage.evidence_items, 0);
        assert_eq!(admitted.usage.model_invocations, 0);
    }
    let mut jobs = state.jobs.lock().unwrap();
    let cancelled = jobs.cancel_investigation(&id).unwrap();
    jobs.advance_investigation(
        &execution,
        cancelled.summary.revision,
        InvestigationTransition::StopWithElapsed {
            reason: InvestigationStopReason::Cancelled,
            active_milliseconds: cancelled.usage.active_milliseconds,
        },
    )
    .unwrap();
    (id, ledger)
}

struct HistoricalStage {
    id: String,
    immutable_json: String,
    reviewed_json: String,
}

fn reviewed_historical_stage(state: &AppState, app_data: &Path) -> HistoricalStage {
    let conn = rusqlite::Connection::open(app_data.join("state.db")).unwrap();
    conn.execute("INSERT INTO jobs(kind,status,error) VALUES ('ingest:/historical/project','failed','historical failure')", []).unwrap();
    let job = state
        .jobs
        .lock()
        .unwrap()
        .get(conn.last_insert_rowid())
        .unwrap();
    let (task, proposal) = historical_proposal();
    let mut store = state.proposals.lock().unwrap();
    let staged = store
        .stage(&task, &proposal, job.id, "snapshot:historical")
        .unwrap();
    let immutable_json = staged_bytes(&app_data.join("proposals.sqlite"), &staged.proposal_id);
    let accepted = store
        .review(
            &staged.proposal_id,
            0,
            agents::ProposalDecision::Accepted,
            Some("first review"),
        )
        .unwrap();
    assert_eq!(accepted.proposal_id, staged.proposal_id);
    assert_eq!(accepted.review_revision, 1);
    assert_eq!(
        accepted.evidence_binding,
        agents::EvidenceBinding::WorkingTreeUnverified
    );
    assert_eq!(
        accepted.context_status,
        agents::ContextStatus::AwaitingReconciliation
    );
    assert_eq!(accepted.proposal.provenance, proposal.provenance);
    let reviewed = store
        .review(
            &staged.proposal_id,
            1,
            agents::ProposalDecision::Rejected,
            Some("second review"),
        )
        .unwrap();
    assert_eq!(reviewed.proposal_id, staged.proposal_id);
    assert_eq!(reviewed.job_id, job.id);
    assert_eq!(
        staged_bytes(&app_data.join("proposals.sqlite"), &staged.proposal_id),
        immutable_json
    );
    HistoricalStage {
        id: staged.proposal_id,
        immutable_json,
        reviewed_json: serde_json::to_string(&reviewed).unwrap(),
    }
}

fn assert_historical_stage(state: &AppState, app_data: &Path, historical: &HistoricalStage) {
    let reviewed = state
        .proposals
        .lock()
        .unwrap()
        .get(&historical.id)
        .unwrap()
        .unwrap();
    assert_eq!(reviewed.proposal_id, historical.id);
    assert_eq!(reviewed.review_revision, 2);
    assert_eq!(
        reviewed.review_decision,
        Some(agents::ProposalDecision::Rejected)
    );
    assert_eq!(
        reviewed.evidence_binding,
        agents::EvidenceBinding::WorkingTreeUnverified
    );
    assert_eq!(
        reviewed.context_status,
        agents::ContextStatus::AwaitingReconciliation
    );
    assert_eq!(reviewed.proposal.provenance.tier, Tier::Agentic);
    assert_eq!(
        reviewed.proposal.provenance.confidence_tier,
        ConfidenceTier::InferredWeak
    );
    assert_eq!(
        serde_json::to_string(&reviewed).unwrap(),
        historical.reviewed_json
    );
    assert_eq!(
        staged_bytes(&app_data.join("proposals.sqlite"), &historical.id),
        historical.immutable_json
    );
}

#[test]
fn primary_source_pipeline_reads_original_bytes_after_restart_and_source_deletion() {
    // AC-0151, AC-0152, AC-0155: actual captured parsing and publication, exact
    // original ranges after deletion/restart, unchanged legacy fact semantics.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), ORIGINAL).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let proposals_before = proposal_page(&state);
    let parsed = parse(&state, &source, || {
        std::fs::write(
            root.join("source.ts"),
            "export function replacement() { return 'live-replacement-canary'; }\n",
        )
        .unwrap();
    });
    assert_eq!(parsed.delta.recomputed_files, 1);
    assert_eq!(parsed.delta.reused_files, 0);
    let rule = rule_node(&parsed);
    let fact = FactKey::from_node(&rule);
    let evidence = GuardedExitEvidence::from_value(rule.props["rule"].clone()).unwrap();
    let prov: Provenance = serde_json::from_value(rule.props["prov"].clone()).unwrap();
    assert_eq!(prov.tier, Tier::Deterministic);
    assert_eq!(prov.confidence_tier, ConfidenceTier::Confirmed);
    assert_eq!(
        evidence.interpretation.consumer_effect,
        InterpretationStatus::NotEstablished
    );
    let legacy_ref = serde_json::to_value(&evidence.exit_source).unwrap();
    assert_eq!(
        legacy_ref,
        json!({
            "repo": source.repo_key, "commit_sha": "workdir", "path": "source.ts",
            "byte_start": evidence.exit_source.byte_start, "byte_end": evidence.exit_source.byte_end,
        })
    );
    publish(&state, &source, &parsed);
    let description = primary_source::describe(&state, &fact).unwrap();
    assert_eq!(description.source_id, source.source_id);
    assert_eq!(description.repo_key, source.repo_key);
    assert_original_ranges(&state, &description, ORIGINAL);
    let snapshot = state.graph.lock().unwrap().read_snapshot().unwrap();
    assert_eq!(
        snapshot.0.iter().find(|node| node.id == rule.id).unwrap(),
        &rule
    );
    let page = context_hub::ContextSnapshot::new(snapshot.0.clone(), snapshot.1.clone())
        .unwrap()
        .query(context_hub::QueryRequest::default())
        .unwrap();
    for mode in [spec::ExportMode::VerifiedOnly, spec::ExportMode::BestEffort] {
        let bundle = spec::compile_spec(&snapshot.0, &snapshot.1, &[], mode, &BTreeSet::new());
        let surfaces = serde_json::to_string(&(
            &snapshot,
            &page,
            &bundle,
            &receipt_rows(&app_data),
            &proposals_before,
        ))
        .unwrap();
        for canary in [
            "primary-comment-canary",
            "primary-password-canary",
            "ghp_primarycapture1234",
            "primary-trailing-source-canary",
            "live-replacement-canary",
        ] {
            assert!(!surfaces.contains(canary), "source disclosed: {canary}");
        }
    }
    drop(parsed);
    std::fs::remove_dir_all(&root).unwrap();
    drop(state);

    let state = app_state(&app_data);
    let restored = primary_source::describe(&state, &fact).unwrap();
    assert_eq!(restored.receipt_id, description.receipt_id);
    assert_original_ranges(&state, &restored, ORIGINAL);
    assert!(primary_source::read(&state, &fact, &restored.receipt_id, usize::MAX).is_err());
    assert!(
        primary_source::describe(
            &state,
            &FactKey::Node {
                id: "missing".into()
            }
        )
        .is_err()
    );
    assert_eq!(
        state.graph.lock().unwrap().read_snapshot().unwrap(),
        snapshot
    );
    // This fixture covers an unchanged empty proposal store, not recertification
    // of a populated immutable historical staging record.
    assert_eq!(proposal_page(&state), proposals_before);
}

#[test]
fn primary_source_equal_facts_publish_distinct_current_receipts() {
    // AC-0151, AC-0152: a complete fact/graph digest cannot select a raw capture;
    // the ordinary no-change reconciliation still replaces current receipts.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let first = parse(&state, &source, || {});
    publish(&state, &source, &first);
    let fact = FactKey::from_node(&rule_node(&first));
    let old = primary_source::describe(&state, &fact).unwrap();
    let before = state.graph.lock().unwrap().read_snapshot().unwrap();
    std::fs::write(root.join("source.ts"), STABLE_B).unwrap();
    let second = parse(&state, &source, || {});
    assert_eq!(second.delta.reused_files, 0);
    assert_eq!(second.delta.recomputed_files, 1);
    assert_eq!(first.extraction.nodes, second.extraction.nodes);
    assert_eq!(first.extraction.edges, second.extraction.edges);
    assert_ne!(
        first.input.capture.as_ref().unwrap().id(),
        second.input.capture.as_ref().unwrap().id()
    );
    publish(&state, &source, &second);
    let current = primary_source::describe(&state, &fact).unwrap();
    assert_eq!(current.emitted_fact_digest, old.emitted_fact_digest);
    assert_ne!(current.receipt_id, old.receipt_id);
    assert_eq!(state.graph.lock().unwrap().read_snapshot().unwrap(), before);
    assert_eq!(
        primary_source::read(&state, &fact, &old.receipt_id, 0)
            .err()
            .unwrap(),
        "Captured source selection is stale; select the fact again."
    );
    assert_original_ranges(&state, &current, STABLE_B);
    drop(first);
    drop(second);
    drop(state);
    let state = app_state(&app_data);
    assert_eq!(
        primary_source::describe(&state, &fact).unwrap().receipt_id,
        current.receipt_id
    );
    assert!(primary_source::read(&state, &fact, &old.receipt_id, 0).is_err());
}

#[test]
fn primary_source_republication_invalidates_historical_retention_preview() {
    // AC-0153: republishing identical retained content changes the current
    // associations without changing the immutable capture/receipt inventory.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let first = parse(&state, &source, || {});
    let original_bindings = publish(&state, &source, &first);
    let fact = FactKey::from_node(&rule_node(&first));
    let original = primary_source::describe(&state, &fact).unwrap();
    let original_graph = state.graph.lock().unwrap().read_snapshot().unwrap();
    let original_receipts = receipt_rows(&app_data);
    drop(first);

    state.graph.lock().unwrap().clear().unwrap();
    let historical = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(historical.captures, 1);
    assert_eq!(historical.current_references, 0);
    assert_eq!(historical.historical_references, historical.receipts);
    assert!(historical.receipts > 0);

    let republished = parse(&state, &source, || {});
    assert_eq!(publish(&state, &source, &republished), original_bindings);
    drop(republished);
    let current = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(current.capture_ids, historical.capture_ids);
    assert_eq!(current.receipts, historical.receipts);
    assert_eq!(receipt_rows(&app_data), original_receipts);
    assert_eq!(
        state.graph.lock().unwrap().read_snapshot().unwrap(),
        original_graph
    );
    assert!(current.current_references > historical.current_references);
    assert!(current.historical_references < historical.historical_references);
    assert_ne!(current.fingerprint, historical.fingerprint);
    assert_eq!(
        primary_source::forget(&state, &source.source_id, &historical.fingerprint)
            .err()
            .unwrap(),
        "Retained source changed; refresh the preview before forgetting."
    );
    assert_original_ranges(&state, &original, STABLE_A);
    assert_eq!(
        primary_source::forget(&state, &source.source_id, &current.fingerprint).unwrap(),
        current.captures as u64
    );
    assert!(primary_source::read(&state, &fact, &original.receipt_id, 0).is_err());
    assert_eq!(receipt_rows(&app_data), original_receipts);
}

#[test]
fn primary_source_investigation_references_bind_forget_confirmation() {
    // AC-0188/0189: selecting a present receipt already creates a durable use,
    // before source reads. New uses invalidate confirmations even when the
    // capture, receipt and current graph inventories have not changed.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let parsed = parse(&state, &source, || {});
    let fact = FactKey::from_node(&rule_node(&parsed));
    let binding = publish(&state, &source, &parsed)
        .into_iter()
        .find(|binding| binding.fact == fact)
        .unwrap();
    drop(parsed);
    let before = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(before.investigation_references, 0);
    let (first_id, first_ledger) =
        retain_investigation_selection(&state, &source, &binding, "retention-first");
    let first = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(first.investigation_references, 1);
    assert_eq!(first.capture_ids, before.capture_ids);
    assert_eq!(first.receipts, before.receipts);
    assert_eq!(first.current_references, before.current_references);
    assert_eq!(first.staged_references, before.staged_references);
    assert_ne!(first.fingerprint, before.fingerprint);
    let (second_id, second_ledger) =
        retain_investigation_selection(&state, &source, &binding, "retention-second");
    let second = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(second.investigation_references, 2);
    assert_eq!(second.capture_ids, first.capture_ids);
    assert_eq!(second.receipts, first.receipts);
    assert_eq!(second.current_references, first.current_references);
    assert_ne!(second.fingerprint, first.fingerprint);
    assert_eq!(
        primary_source::forget(&state, &source.source_id, &first.fingerprint)
            .err()
            .unwrap(),
        "Retained source changed; refresh the preview before forgetting."
    );
    let other_root = directory(dir.path(), "other/project");
    let other = register_local_source(&state, &other_root).unwrap();
    assert_eq!(
        primary_source::preview(&state, &other.source_id)
            .unwrap()
            .investigation_references,
        0
    );
    let receipts = receipt_rows(&app_data);
    state.jobs.lock().unwrap().clear_finished().unwrap();
    state.graph.lock().unwrap().clear().unwrap();
    std::fs::remove_dir_all(source.root()).unwrap();
    drop(state);
    let state = app_state(&app_data);
    let resumed = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(resumed.investigation_references, 2);
    assert_eq!(resumed.capture_ids, before.capture_ids);
    assert_eq!(resumed.current_references, 0);
    let retained = [&first_id, &second_id]
        .into_iter()
        .map(|id| {
            state
                .jobs
                .lock()
                .unwrap()
                .investigation_ledger(id)
                .unwrap()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(retained, vec![first_ledger, second_ledger]);
    assert_eq!(
        primary_source::forget(&state, &source.source_id, &resumed.fingerprint).unwrap(),
        resumed.captures as u64
    );
    let forgotten = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(forgotten.captures, 0);
    assert_eq!(forgotten.investigation_references, 2);
    assert_eq!(receipt_rows(&app_data), receipts);
    for (id, ledger) in [&first_id, &second_id].into_iter().zip(retained) {
        assert_eq!(
            state.jobs.lock().unwrap().investigation_ledger(id).unwrap(),
            Some(ledger)
        );
    }
}

#[test]
fn primary_source_invalid_investigation_reference_blocks_forgetting() {
    // AC-0188/0189: a corrupt reference index is an explicit failure, never a
    // smaller reassuring preview or authority to delete retained content.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let parsed = parse(&state, &source, || {});
    let fact = FactKey::from_node(&rule_node(&parsed));
    let binding = publish(&state, &source, &parsed)
        .into_iter()
        .find(|binding| binding.fact == fact)
        .unwrap();
    drop(parsed);
    let (id, ledger) =
        retain_investigation_selection(&state, &source, &binding, "retention-corrupt");
    let preview = primary_source::preview(&state, &source.source_id).unwrap();
    let conn = rusqlite::Connection::open(app_data.join("state.db")).unwrap();
    conn.execute(
        "INSERT INTO investigation_refs (task_id,input_revision,source_id,repo_key,receipt_id) VALUES (?1,?2,?3,?4,?5)",
        rusqlite::params![id, i64::try_from(ledger.revision).unwrap(), source.source_id, source.repo_key,
            format!("ts-primary-v2:{}", "0".repeat(64))],
    ).unwrap();
    assert!(primary_source::preview(&state, &source.source_id).is_err());
    assert!(primary_source::forget(&state, &source.source_id, &preview.fingerprint).is_err());
    let captures = source_capture::CaptureStore::open(
        app_data.join("retained-source/captures.sqlite"),
        source_capture::StoreLimits::default(),
    )
    .unwrap();
    let still_retained = captures
        .source_inventory(&source_capture::SourceId::new(&source.source_id).unwrap())
        .unwrap();
    assert_eq!(
        still_retained
            .into_iter()
            .map(|capture| capture.capture_id)
            .collect::<Vec<_>>(),
        preview.capture_ids
    );
}

#[test]
fn primary_source_same_name_sources_forget_only_previewed_retention() {
    // AC-0151, AC-0153, AC-0155: source identity and preview-bound forgetting
    // survive restart while shared raw objects and populated, reviewed historical
    // staging bytes remain unchanged and cannot acquire captured-source semantics.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let first_root = directory(dir.path(), "first/project");
    let second_root = directory(dir.path(), "second/project");
    for root in [&first_root, &second_root] {
        std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    }
    let state = app_state(&app_data);
    let historical = reviewed_historical_stage(&state, &app_data);
    assert_historical_stage(&state, &app_data, &historical);
    let jobs = serde_json::to_value(state.jobs.lock().unwrap().list().unwrap()).unwrap();
    let proposals = proposal_page(&state);
    let first_source = register_local_source(&state, &first_root).unwrap();
    let second_source = register_local_source(&state, &second_root).unwrap();
    assert_eq!(first_source.display_name, second_source.display_name);
    assert_ne!(first_source.source_id, second_source.source_id);
    assert_ne!(first_source.repo_key, second_source.repo_key);
    let first = parse(&state, &first_source, || {});
    let second = parse(&state, &second_source, || {});
    let first_capture = first.input.capture.as_ref().unwrap();
    let second_capture = second.input.capture.as_ref().unwrap();
    assert_ne!(first_capture.id(), second_capture.id());
    assert_eq!(
        first_capture.file("source.ts").unwrap().reference().digest,
        second_capture.file("source.ts").unwrap().reference().digest,
    );
    publish(&state, &first_source, &first);
    publish(&state, &second_source, &second);
    assert_historical_stage(&state, &app_data, &historical);
    assert_eq!(proposal_page(&state), proposals);
    let first_fact = FactKey::from_node(&rule_node(&first));
    let second_fact = FactKey::from_node(&rule_node(&second));
    let second_description = primary_source::describe(&state, &second_fact).unwrap();
    assert!(primary_source::read(&state, &first_fact, &second_description.receipt_id, 0).is_err());
    drop(first);
    drop(second);
    let old_preview = primary_source::preview(&state, &first_source.source_id).unwrap();
    std::fs::write(first_root.join("source.ts"), STABLE_B).unwrap();
    let changed = parse(&state, &first_source, || {});
    publish(&state, &first_source, &changed);
    assert_historical_stage(&state, &app_data, &historical);
    drop(changed);
    assert!(
        primary_source::forget(&state, &first_source.source_id, &old_preview.fingerprint).is_err()
    );
    let preview = primary_source::preview(&state, &first_source.source_id).unwrap();
    assert_eq!(preview.captures, 2);
    assert!(preview.current_references > 0);
    assert!(preview.historical_references > 0);
    let first_description = primary_source::describe(&state, &first_fact).unwrap();
    let snapshot = state.graph.lock().unwrap().read_snapshot().unwrap();
    let bindings = state
        .graph
        .lock()
        .unwrap()
        .source_bindings_for_repo(&first_source.repo_key)
        .unwrap();
    let receipts = receipt_rows(&app_data);

    // A fresh shared guard held by actual acquisition excludes forgetting. Drop
    // must explicitly unlock; an unrelated test fork may inherit a file handle.
    // No sleeps/retries mask an accidentally retained lock after this drop.
    let active = state
        .primary_sources
        .prepare(&first_source, &first_root, &["client".into()])
        .unwrap();
    assert!(primary_source::forget(&state, &first_source.source_id, &preview.fingerprint).is_err());
    drop(active);
    assert_eq!(
        primary_source::forget(&state, &first_source.source_id, &preview.fingerprint).unwrap(),
        preview.captures as u64
    );
    let forgotten = primary_source::preview(&state, &first_source.source_id).unwrap();
    assert_eq!(
        (forgotten.captures, forgotten.files, forgotten.bytes),
        (0, 0, 0)
    );
    assert_eq!(forgotten.receipts, preview.receipts);
    assert_eq!(receipt_rows(&app_data), receipts);
    assert_eq!(
        state.graph.lock().unwrap().read_snapshot().unwrap(),
        snapshot
    );
    assert_eq!(
        state
            .graph
            .lock()
            .unwrap()
            .source_bindings_for_repo(&first_source.repo_key)
            .unwrap(),
        bindings
    );
    assert!(primary_source::read(&state, &first_fact, &first_description.receipt_id, 0).is_err());
    assert_original_ranges(&state, &second_description, STABLE_A);
    assert_eq!(
        serde_json::to_value(state.jobs.lock().unwrap().list().unwrap()).unwrap(),
        jobs
    );
    assert_eq!(proposal_page(&state), proposals);
    assert_historical_stage(&state, &app_data, &historical);
    drop(state);

    let state = app_state(&app_data);
    assert_original_ranges(&state, &second_description, STABLE_A);
    assert!(primary_source::read(&state, &first_fact, &first_description.receipt_id, 0).is_err());
    assert_eq!(receipt_rows(&app_data), receipts);
    assert_eq!(
        serde_json::to_value(state.jobs.lock().unwrap().list().unwrap()).unwrap(),
        jobs
    );
    assert_eq!(proposal_page(&state), proposals);
    assert_historical_stage(&state, &app_data, &historical);
}

#[test]
fn primary_source_enrichment_mutation_cannot_reuse_direct_receipt() {
    // AC-0151, AC-0152, AC-0155: unchanged provenance/span is insufficient when
    // a later enrichment changes any complete node or edge property.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let first = parse(&state, &source, || {});
    publish(&state, &source, &first);
    let rule = rule_node(&first);
    let fact = FactKey::from_node(&rule);
    let evidence = GuardedExitEvidence::from_value(rule.props["rule"].clone()).unwrap();
    let owner = FactKey::Node {
        id: evidence.owner_id,
    };
    let edge = first
        .extraction
        .edges
        .iter()
        .find(|edge| edge.label == "GOVERNS" && edge.src == rule.id)
        .unwrap()
        .clone();
    let edge_fact = FactKey::from_edge(&edge);
    let old = primary_source::describe(&state, &fact).unwrap();
    primary_source::describe(&state, &edge_fact).unwrap();
    drop(first);
    let mut enriched = parse(&state, &source, || {});
    let final_node = enriched
        .extraction
        .nodes
        .iter_mut()
        .find(|node| node.id == rule.id)
        .unwrap();
    final_node.props["later_enrichment"] = json!(true);
    assert_eq!(final_node.props["prov"], rule.props["prov"]);
    let final_edge = enriched
        .extraction
        .edges
        .iter_mut()
        .find(|candidate| FactKey::from_edge(candidate) == edge_fact)
        .unwrap();
    final_edge.props["later_enrichment"] = json!(true);
    assert_eq!(final_edge.props["prov"], edge.props["prov"]);
    let bindings = publish(&state, &source, &enriched);
    assert!(
        !bindings
            .iter()
            .any(|binding| binding.fact == fact || binding.fact == edge_fact)
    );
    assert!(bindings.iter().any(|binding| binding.fact == owner));
    assert!(
        state
            .graph
            .lock()
            .unwrap()
            .current_source_binding(&fact)
            .unwrap()
            .is_none()
    );
    assert!(
        state
            .graph
            .lock()
            .unwrap()
            .current_source_binding(&edge_fact)
            .unwrap()
            .is_none()
    );
    assert!(primary_source::read(&state, &fact, &old.receipt_id, 0).is_err());
    assert_original_ranges(
        &state,
        &primary_source::describe(&state, &owner).unwrap(),
        STABLE_A,
    );
    let snapshot = state.graph.lock().unwrap().read_snapshot().unwrap();
    let stored = snapshot.0.iter().find(|node| node.id == rule.id).unwrap();
    assert_eq!(stored.props["later_enrichment"], json!(true));
    assert_eq!(stored.props["prov"], rule.props["prov"]);
}

#[test]
fn primary_source_persistence_rejects_foreign_receipts_before_publication() {
    // AC-0151: an actual ownership failure in durable receipt persistence must
    // leave existing published facts/current bindings intact. Orphans are allowed.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let first_root = directory(dir.path(), "first");
    let second_root = directory(dir.path(), "second");
    std::fs::write(first_root.join("source.ts"), STABLE_A).unwrap();
    std::fs::write(second_root.join("source.ts"), STABLE_A).unwrap();
    let state = app_state(&app_data);
    let first_source = register_local_source(&state, &first_root).unwrap();
    let second_source = register_local_source(&state, &second_root).unwrap();
    let first = parse(&state, &first_source, || {});
    publish(&state, &first_source, &first);
    let fact = FactKey::from_node(&rule_node(&first));
    let old = primary_source::describe(&state, &fact).unwrap();
    let before = state.graph.lock().unwrap().read_snapshot().unwrap();
    let receipts = receipt_rows(&app_data);
    drop(first);
    std::fs::write(first_root.join("source.ts"), STABLE_B).unwrap();
    let changed = parse(&state, &first_source, || {});
    assert!(
        state
            .primary_sources
            .persist(&changed.input, &second_source, &changed.receipts)
            .is_err()
    );
    assert_eq!(state.graph.lock().unwrap().read_snapshot().unwrap(), before);
    assert_eq!(receipt_rows(&app_data), receipts);
    assert_eq!(
        primary_source::describe(&state, &fact).unwrap().receipt_id,
        old.receipt_id
    );
    assert_original_ranges(&state, &old, STABLE_A);
    assert!(
        state
            .graph
            .lock()
            .unwrap()
            .source_bindings_for_repo(&second_source.repo_key)
            .unwrap()
            .is_empty()
    );
    publish(&state, &first_source, &changed);
    assert_ne!(
        primary_source::describe(&state, &fact).unwrap().receipt_id,
        old.receipt_id
    );
}
