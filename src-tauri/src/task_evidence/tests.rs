use super::*;
use crate::primary_source;
use crate::registered_source_tests::app_state;
use adapters_lang_ts::captured::{RangeRole, Receipt};
use core_graph::rules::GuardedExitEvidence;
use core_graph::source::{SourceBinding, node_digest};
use core_graph::{GraphPatch, GraphStore};
use core_prov::{EvidenceRef, Provenance, Tier};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub(super) const ORIGINAL: &str = concat!(
    "// café — 🧭\r\n",
    "export function retained(enabled: boolean) {\r\n",
    "  const disabled = !enabled;\r\n",
    "  if (disabled) return false;\r\n",
    "  return true;\r\n",
    "}\r\n",
    "export function alternative(value: boolean) {\r\n",
    "  if (value) return true;\r\n",
    "  return false;\r\n",
    "}\r\n",
    "// trailing-A\r\n",
);
pub(super) const GAP: &str = "gap:task-fixture";

pub(super) fn directory(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    std::fs::create_dir_all(&path).unwrap();
    crate::paths::canonicalize(path).unwrap()
}

pub(super) struct Captured {
    pub(super) nodes: Vec<Node>,
    pub(super) receipts: Vec<Receipt>,
}

pub(super) fn capture(state: &AppState, source: &RegisteredSource, publish: bool) -> Captured {
    // The same application-only producer pipeline as primary_source_tests:
    // no mocked receipts and no claim about auxiliary live configuration reads.
    let layers = vec!["client".to_string()];
    let input = state
        .primary_sources
        .prepare(source, source.root(), &layers)
        .unwrap();
    assert!(input.capture.is_some());
    let mut receipts = Vec::new();
    let extraction = {
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        crate::extract_tree_with_primary(
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
        .0
    };
    assert!(!receipts.is_empty());
    state
        .primary_sources
        .persist(&input, source, &receipts)
        .unwrap();
    if publish {
        let bindings = primary_source::matching_bindings(&extraction, &receipts);
        crate::load_into_graph_with_bindings(
            &mut state.graph.lock().unwrap(),
            &extraction,
            &source.repo_key,
            source.root(),
            "workdir",
            &bindings,
        )
        .unwrap();
    }
    Captured {
        nodes: extraction.nodes,
        receipts,
    }
    // The producer retention guard ends before task preparation or forgetting.
}

pub(super) fn rule_pair(captured: &Captured) -> (Node, Node) {
    let rules: Vec<_> = captured
        .nodes
        .iter()
        .filter(|node| node.label == "BusinessRule")
        .collect();
    let source = rules
        .iter()
        .find(|node| {
            GuardedExitEvidence::from_value(node.props["rule"].clone())
                .unwrap()
                .local_definitions
                .is_some_and(|definitions| !definitions.is_empty())
        })
        .unwrap();
    let candidate = rules.iter().find(|node| node.id != source.id).unwrap();
    ((**source).clone(), (**candidate).clone())
}

fn fixture_node(id: &str, label: &str, reference: Option<EvidenceRef>) -> Node {
    let confidence = if label == "Gap" {
        ConfidenceTier::Gap
    } else {
        ConfidenceTier::Confirmed
    };
    let provenance = Provenance::new(
        Tier::Deterministic,
        confidence,
        reference.into_iter().collect(),
        "test.legacy-task-fixture",
        id.as_bytes(),
    )
    .unwrap();
    Node {
        id: id.into(),
        label: label.into(),
        props: json!({"name": id, "prov": provenance}),
    }
}

fn reference(source: &RegisteredSource, path: &str, len: u64) -> EvidenceRef {
    EvidenceRef {
        repo: source.repo_key.clone(),
        commit_sha: "workdir".into(),
        path: path.into(),
        byte_start: 0,
        byte_end: len,
    }
}

pub(super) fn install_task(state: &AppState, source: &Node, candidates: &[Node]) {
    let mut graph = state.graph.lock().unwrap();
    // Keep the real producer facts and receipts, but isolate this test's task
    // topology from unrelated interpretation gaps in the extracted graph.
    for edge in graph.read_snapshot().unwrap().1 {
        graph
            .delete_edge(&edge.src, &edge.dst, &edge.label)
            .unwrap();
    }
    for node in std::iter::once(source).chain(candidates) {
        if graph.get_node(&node.id).unwrap().as_ref() != Some(node) {
            graph.put_node(node).unwrap();
        }
    }
    graph.put_node(&fixture_node(GAP, "Gap", None)).unwrap();
    graph
        .put_edge(&Edge {
            src: source.id.clone(),
            dst: GAP.into(),
            label: "CALLS".into(),
            props: json!({}),
        })
        .unwrap();
    for candidate in candidates {
        graph
            .put_edge(&Edge {
                src: GAP.into(),
                dst: candidate.id.clone(),
                label: "REFERENCES".into(),
                props: json!({}),
            })
            .unwrap();
    }
}

pub(super) fn receipt_for<'a>(captured: &'a Captured, node: &Node) -> &'a Receipt {
    captured
        .receipts
        .iter()
        .find(|receipt| receipt.matches_node(node))
        .unwrap()
}

pub(super) fn replace_current_receipts(
    state: &AppState,
    source: &RegisteredSource,
    captured: &Captured,
) {
    let mut graph = state.graph.lock().unwrap();
    let expected = graph.read_snapshot().unwrap();
    let bindings: Vec<_> = captured
        .receipts
        .iter()
        .filter(|receipt| expected.0.iter().any(|node| receipt.matches_node(node)))
        .map(|receipt| SourceBinding {
            fact: receipt.fact_key().clone(),
            repo_key: source.repo_key.clone(),
            receipt_id: receipt.id().into(),
            emitted_fact_digest: receipt.fact_digest().into(),
        })
        .collect();
    assert!(!bindings.is_empty());
    assert!(
        graph
            .apply_patch_with_source_bindings_if_snapshot_matches(
                &expected,
                &GraphPatch::default(),
                &source.repo_key,
                &bindings,
            )
            .unwrap()
    );
    assert_eq!(graph.read_snapshot().unwrap(), expected);
}

struct RecordingProvider {
    response: String,
    seen: Mutex<Vec<llm::PayloadSpan>>,
}

impl llm::LlmProvider for RecordingProvider {
    fn id(&self) -> &str {
        "local:task-source-fixture"
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
        Err(llm::ProviderError::Unsupported("test embeddings"))
    }
    fn complete(
        &self,
        request: &llm::ProviderCompletionRequest,
    ) -> Result<llm::Completion, llm::ProviderError> {
        *self.seen.lock().unwrap() = request.spans().to_vec();
        Ok(llm::Completion {
            text: self.response.clone(),
        })
    }
}

#[test]
fn prepared_task_retains_original_bytes_after_checkout_change_deletion_and_restart() {
    // AC-0172, AC-0174: real captured producer facts supply exact bytes after
    // the root disappears; provider execution uses the already-owned task.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), ORIGINAL).unwrap();
    let state = app_state(&app_data);
    let source = crate::register_local_source(&state, &root).unwrap();
    let captured = capture(&state, &source, true);
    let (owner, candidate) = rule_pair(&captured);
    install_task(&state, &owner, &[candidate]);
    let initial = prepare(&state, GAP, "task:retained").unwrap();
    std::fs::write(
        root.join("source.ts"),
        "live-replacement-canary\n".repeat(100),
    )
    .unwrap();
    assert_eq!(prepare(&state, GAP, "task:retained").unwrap(), initial);
    std::fs::remove_dir_all(&root).unwrap();
    drop(state);
    let reopened = app_state(&app_data);
    let prepared = prepare(&reopened, GAP, "task:retained").unwrap();
    assert_eq!(prepared, initial);
    assert_eq!(prepared.task().evidence.len(), 2);
    for item in &prepared.task().evidence {
        assert_eq!(
            item.text.as_bytes(),
            &ORIGINAL.as_bytes()[item.source.byte_start as usize..item.source.byte_end as usize]
        );
        assert_eq!(item.source.commit_sha, "workdir");
    }
    assert!(
        prepared
            .source_basis()
            .evidence
            .iter()
            .all(|origin| matches!(
                &origin.origin,
                TaskEvidenceKind::CapturedPrimarySource {
                    registered_source_id,
                    scope: PrimarySourceScope::PrimarySourceOnly,
                    input_closure: InputClosureStatus::InputClosureNotEstablished,
                    ..
                } if registered_source_id == &source.source_id
            ))
    );
    let selected = &prepared.task().candidates[0];
    let provider = RecordingProvider {
        response: json!({"target_id": selected.node_id, "annotation": "Possible lexical connection", "citations": [prepared.task().source_evidence_ids[0], selected.evidence_ids[0]]}).to_string(),
        seen: Mutex::new(Vec::new()),
    };
    // Successful forgetting proves preparation released its retention guards.
    let preview = primary_source::preview(&reopened, &source.source_id).unwrap();
    primary_source::forget(&reopened, &source.source_id, &preview.fingerprint).unwrap();
    let broker = agents::AgentBroker::bounded_default();
    let firewall = llm::EgressFirewall::new(llm::EgressPolicy::default());
    let preview = broker
        .preview_prepared(&provider, &firewall, &prepared)
        .unwrap();
    let proposal = broker
        .propose_prepared(&provider, &firewall, &prepared, None)
        .unwrap();
    assert_eq!(
        provider.seen.lock().unwrap().as_slice(),
        preview.payload.spans
    );
    assert_eq!(proposal.basis_hash, prepared.basis_hash().unwrap());
    assert_eq!(proposal.provenance.tier, Tier::Agentic);
    assert_eq!(
        proposal.provenance.confidence_tier,
        ConfidenceTier::InferredWeak
    );
}

#[test]
fn pinned_v2_nested_ranges_survive_current_binding_and_fact_removal() {
    // AC-0172, AC-0174: nested roles read their original inventory occurrence,
    // not current graph association, an enclosing owner or caller offsets.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), ORIGINAL).unwrap();
    let state = app_state(&app_data);
    let source = crate::register_local_source(&state, &root).unwrap();
    let captured = capture(&state, &source, true);
    let (owner, other) = rule_pair(&captured);
    let original = receipt_for(&captured, &owner);
    assert!(original.id().starts_with("ts-primary-v2:"));
    let guard = state.primary_sources.task_guard(&source.source_id).unwrap();
    let pinned = state
        .primary_sources
        .task_receipt(
            &guard,
            original.fact_key(),
            original.fact_digest(),
            &source.repo_key,
            original.id(),
        )
        .unwrap();
    state.graph.lock().unwrap().delete_node(&owner.id).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    for role in [
        RangeRole::DefinitionInitializer,
        RangeRole::DefinitionExpression,
        RangeRole::DefinitionUse,
    ] {
        let (index, range) = pinned
            .ranges()
            .iter()
            .enumerate()
            .find(|(_, range)| range.role == role)
            .unwrap();
        assert_eq!(
            state
                .primary_sources
                .task_text(&guard, &pinned, index)
                .unwrap()
                .as_bytes(),
            &ORIGINAL.as_bytes()
                [range.evidence.byte_start as usize..range.evidence.byte_end as usize]
        );
    }
    assert_eq!(
        state
            .primary_sources
            .task_text(&guard, &pinned, pinned.ranges().len()),
        Err(PinnedReadError::Invalid)
    );
    assert!(matches!(
        state.primary_sources.task_receipt(
            &guard,
            &FactKey::from_node(&other),
            &node_digest(&other).unwrap(),
            &source.repo_key,
            original.id()
        ),
        Err(PinnedReadError::Invalid)
    ));
    assert_eq!(
        state
            .primary_sources
            .task_receipt(
                &guard,
                original.fact_key(),
                original.fact_digest(),
                &source.repo_key,
                original.id()
            )
            .unwrap()
            .id(),
        pinned.id()
    );
}

#[test]
fn preparation_rejects_equal_fact_receipt_change_after_source_guards() {
    // AC-0173: graph equality cannot hide a changed receipt. The authoritative
    // pass rejects it instead of substituting the new capture or legacy text.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), ORIGINAL).unwrap();
    let state = app_state(&app_data);
    let source = crate::register_local_source(&state, &root).unwrap();
    let before = capture(&state, &source, true);
    let (owner, candidate) = rule_pair(&before);
    install_task(&state, &owner, &[candidate]);
    let old = prepare(&state, GAP, "task:receipt-change").unwrap();
    let graph = state.graph.lock().unwrap().read_snapshot().unwrap();
    std::fs::write(
        root.join("source.ts"),
        ORIGINAL.replace("trailing-A", "trailing-B"),
    )
    .unwrap();
    let after = capture(&state, &source, false);
    assert_ne!(
        receipt_for(&before, &owner).id(),
        receipt_for(&after, &owner).id()
    );
    let error = prepare_from_snapshot(&state, graph.clone(), GAP, "task:receipt-change", || {
        replace_current_receipts(&state, &source, &after)
    })
    .unwrap_err();
    assert!(error.to_string().starts_with(CHANGED), "{error}");
    let report = error.selection.as_ref().unwrap();
    assert_eq!(report.stop_reason, SelectionStopReason::InvalidSelection);
    assert_eq!(
        (
            report.metadata_lookahead,
            report.acquisition_attempts,
            report.metadata_preselected_not_read
        ),
        (3, 1, 2)
    );
    assert_eq!(
        (
            report.supplied_candidates,
            report.supplied_evidence,
            report.captured_validation_bytes
        ),
        (0, 0, 0)
    );
    assert!(report.omissions.is_empty());
    assert!(!report.unread_tail);
    assert_eq!(state.graph.lock().unwrap().read_snapshot().unwrap(), graph);
    let new = prepare(&state, GAP, "task:receipt-change").unwrap();
    assert_eq!(old.task(), new.task());
    assert_eq!(
        old.source_basis().graph_snapshot_id,
        new.source_basis().graph_snapshot_id
    );
    assert_ne!(
        old.source_basis().selected_facts,
        new.source_basis().selected_facts
    );
    assert_ne!(old.basis_hash().unwrap(), new.basis_hash().unwrap());
}

#[test]
fn participating_corrupt_or_forgotten_capture_never_falls_back_to_readable_checkout() {
    // AC-0172, AC-0174: successful working-tree reads cannot rescue a selected
    // participating receipt whose immutable payload or retained object failed.
    for failure in ["receipt", "object", "forgotten"] {
        let dir = tempfile::tempdir().unwrap();
        let app_data = directory(dir.path(), "private");
        let root = directory(dir.path(), "project");
        std::fs::write(root.join("source.ts"), ORIGINAL).unwrap();
        let state = app_state(&app_data);
        let source = crate::register_local_source(&state, &root).unwrap();
        let captured = capture(&state, &source, true);
        let (owner, candidate) = rule_pair(&captured);
        install_task(&state, &owner, &[candidate]);
        prepare(&state, GAP, "task:before-corruption").unwrap();
        std::fs::write(root.join("source.ts"), "live-fallback-canary\n".repeat(100)).unwrap();
        let original = receipt_for(&captured, &owner);
        let evidence = original
            .ranges()
            .iter()
            .find(|range| range.role == RangeRole::Provenance && range.index == 0)
            .unwrap();
        assert!(
            crate::evidence::read_span_exact(
                &root,
                "source.ts",
                &(evidence.evidence.byte_start..evidence.evidence.byte_end)
            )
            .is_ok()
        );
        match failure {
            "receipt" => {
                rusqlite::Connection::open(app_data.join("retained-source/receipts.sqlite"))
                    .unwrap()
                    .execute(
                        "UPDATE receipts SET payload = '{}' WHERE id = ?1",
                        [original.id()],
                    )
                    .unwrap();
            }
            "object" => {
                rusqlite::Connection::open(app_data.join("retained-source/captures.sqlite"))
                    .unwrap()
                    .execute(
                        "UPDATE objects SET bytes = zeroblob(length(bytes)) WHERE digest = ?1",
                        [&original.file().digest],
                    )
                    .unwrap();
            }
            "forgotten" => {
                let preview = primary_source::preview(&state, &source.source_id).unwrap();
                primary_source::forget(&state, &source.source_id, &preview.fingerprint).unwrap();
            }
            _ => unreachable!(),
        }
        let graph = state.graph.lock().unwrap().read_snapshot().unwrap();
        let error =
            prepare_from_snapshot(&state, graph, GAP, "task:after-corruption", || {}).unwrap_err();
        assert!(
            error.to_string().contains("Retained task evidence"),
            "{error}"
        );
        assert!(!error.to_string().contains("live-fallback-canary"));
        let report = error.selection.as_ref().unwrap();
        assert_eq!(
            report.stop_reason,
            SelectionStopReason::ParticipatingSourceUnavailable
        );
        assert_eq!(
            (
                report.metadata_lookahead,
                report.acquisition_attempts,
                report.metadata_preselected_not_read
            ),
            (3, 1, 2)
        );
        assert_eq!(
            (report.supplied_evidence, report.supplied_candidates),
            (0, 0)
        );
        assert_eq!(
            report.captured_validation_bytes,
            if failure == "receipt" {
                0
            } else {
                ORIGINAL.len() as u64
            }
        );
        assert!(report.omissions.is_empty());
        assert!(
            state
                .graph
                .lock()
                .unwrap()
                .current_source_binding(&FactKey::from_node(&owner))
                .unwrap()
                .is_some()
        );
    }
}

fn legacy_task(state: &AppState, source: &RegisteredSource, readable: &[bool]) -> Vec<Node> {
    std::fs::write(source.root().join("source.txt"), "source").unwrap();
    std::fs::write(source.root().join("candidate.txt"), "target").unwrap();
    let owner = fixture_node(
        "source:legacy",
        "Symbol",
        Some(reference(source, "source.txt", 6)),
    );
    let candidates: Vec<_> = readable
        .iter()
        .enumerate()
        .map(|(index, readable)| {
            fixture_node(
                &format!("candidate:{index:03}"),
                "Symbol",
                Some(reference(
                    source,
                    if *readable {
                        "candidate.txt"
                    } else {
                        "missing.txt"
                    },
                    6,
                )),
            )
        })
        .collect();
    install_task(state, &owner, &candidates);
    candidates
}

#[test]
fn legacy_preparation_scans_past_ten_unreadable_candidates() {
    // AC-0173, AC-0174: unreadable legacy probes consume acquisition work, not
    // supplied slots. Eight later readable candidates retain unverified origin.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    let state = app_state(&app_data);
    let source = crate::register_local_source(&state, &root).unwrap();
    let mut readable = vec![false; 10];
    readable.extend([true; 8]);
    legacy_task(&state, &source, &readable);
    let prepared = prepare(&state, GAP, "task:legacy-scan").unwrap();
    assert_eq!(
        prepared
            .task()
            .candidates
            .iter()
            .map(|candidate| candidate.node_id.as_str())
            .collect::<Vec<_>>(),
        (10..18)
            .map(|index| format!("candidate:{index:03}"))
            .collect::<Vec<_>>()
    );
    let report = &prepared.source_basis().selection;
    assert_eq!(
        (
            report.metadata_lookahead,
            report.acquisition_attempts,
            report.supplied_candidates,
            report.supplied_evidence
        ),
        (20, 20, 8, 9)
    );
    assert_eq!(report.stop_reason, SelectionStopReason::CandidateLimit);
    assert_eq!(report.omissions.len(), 11);
    assert_eq!(report.captured_validation_bytes, 0);
    assert!(
        prepared
            .source_basis()
            .evidence
            .iter()
            .all(|item| item.origin == TaskEvidenceKind::WorkingTreeUnverified)
    );
}

#[test]
fn legacy_preparation_stops_at_sixty_four_attempts_and_reports_uninspected_tail() {
    // AC-0173, AC-0174: the explicit cap includes source and Gap; a late good
    // candidate beyond the window is neither read nor staged as unavailable.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    let state = app_state(&app_data);
    let source = crate::register_local_source(&state, &root).unwrap();
    let mut readable = vec![false; 70];
    readable[0] = true;
    readable[69] = true;
    legacy_task(&state, &source, &readable);
    let prepared = prepare(&state, GAP, "task:attempt-cap").unwrap();
    let report = &prepared.source_basis().selection;
    assert_eq!(
        (report.metadata_lookahead, report.acquisition_attempts),
        (64, 64)
    );
    assert_eq!(report.stop_reason, SelectionStopReason::AttemptLimit);
    assert_eq!(
        (report.supplied_candidates, report.supplied_evidence),
        (1, 2)
    );
    assert!(report.unread_tail);
    assert_eq!(report.metadata_preselected_not_read, 0);
    assert_eq!(report.omissions.len(), 62);
    assert_eq!(prepared.source_basis().selected_facts.len(), 65);
    assert!(
        !prepared
            .source_basis()
            .selected_facts
            .iter()
            .any(|item| item.fact
                == TaskFactKey::Node {
                    id: "candidate:069".into()
                })
    );
}

#[test]
fn unreadable_legacy_membership_failure_retains_exact_bounded_report() {
    // AC-0173, AC-0174: a failed task retains actual attempted/omitted work and
    // a closed stop reason; diagnostics do not reproduce arbitrary fact IDs.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    let state = app_state(&app_data);
    let source = crate::register_local_source(&state, &root).unwrap();
    legacy_task(&state, &source, &[false; 4]);
    let mut graph = state.graph.lock().unwrap().read_snapshot().unwrap();
    let old = graph
        .0
        .iter()
        .find(|node| node.id == "candidate:000")
        .unwrap()
        .clone();
    let mut renamed = old.clone();
    renamed.id = "candidate:SOURCE_ID_CANARY".into();
    {
        let mut store = state.graph.lock().unwrap();
        store.delete_node(&old.id).unwrap();
        store.put_node(&renamed).unwrap();
        store
            .put_edge(&Edge {
                src: GAP.into(),
                dst: renamed.id,
                label: "REFERENCES".into(),
                props: json!({}),
            })
            .unwrap();
        graph = store.read_snapshot().unwrap();
    }
    let error =
        prepare_from_snapshot(&state, graph, GAP, "task:missing-membership", || {}).unwrap_err();
    let report = error.selection.as_ref().unwrap();
    assert_eq!(
        report.stop_reason,
        SelectionStopReason::RequiredMembershipMissing
    );
    assert_eq!(
        (report.metadata_lookahead, report.acquisition_attempts),
        (6, 6)
    );
    assert_eq!(
        (report.supplied_evidence, report.supplied_candidates),
        (1, 0)
    );
    assert_eq!(report.omissions.len(), 5);
    assert_eq!(
        report.omissions[0].reason,
        SelectionOmissionReason::MissingCitation
    );
    assert!(
        report.omissions[1..]
            .iter()
            .all(|omission| omission.reason == SelectionOmissionReason::LegacyReadUnavailable)
    );
    assert_eq!(report.metadata_preselected_not_read, 0);
    assert!(!report.unread_tail);
    assert_eq!(report.limits, SelectionLimits::default());
    assert!(!error.to_string().contains("SOURCE_ID_CANARY"));
    assert!(error.to_string().contains("required_membership_missing"));
}

#[test]
fn unused_captured_tail_guard_failure_does_not_abort_completed_candidate_prefix() {
    // AC-0173: an actual exclusive retention lease makes the captured tail
    // unavailable, but that request stays metadata-only after eight candidates.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "legacy");
    let retained_root = directory(dir.path(), "retained");
    let state = app_state(&app_data);
    let source = crate::register_local_source(&state, &root).unwrap();
    std::fs::write(retained_root.join("source.ts"), ORIGINAL).unwrap();
    let retained = crate::register_local_source(&state, &retained_root).unwrap();
    let captured = capture(&state, &retained, true);
    let (tail, _) = rule_pair(&captured);
    let mut candidates = legacy_task(&state, &source, &[true; 8]);
    candidates.push(tail.clone());
    let owner = state
        .graph
        .lock()
        .unwrap()
        .get_node("source:legacy")
        .unwrap()
        .unwrap();
    install_task(&state, &owner, &candidates);
    drop(
        state
            .primary_sources
            .task_guard(&retained.source_id)
            .unwrap(),
    );
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(
            app_data
                .join("retained-source/locks")
                .join(format!("{}.lock", retained.source_id)),
        )
        .unwrap();
    lock.try_lock().unwrap();
    assert!(matches!(
        state.primary_sources.task_guard(&retained.source_id),
        Err(PinnedReadError::Operational)
    ));
    let prepared = prepare(&state, GAP, "task:unused-tail").unwrap();
    assert_eq!(
        prepared.source_basis().selection.stop_reason,
        SelectionStopReason::CandidateLimit
    );
    assert_eq!(
        (
            prepared.source_basis().selection.metadata_lookahead,
            prepared.source_basis().selection.acquisition_attempts,
            prepared
                .source_basis()
                .selection
                .metadata_preselected_not_read
        ),
        (11, 10, 1)
    );
    assert!(
        !prepared
            .source_basis()
            .selected_facts
            .iter()
            .any(|selection| selection.fact == task_fact(&FactKey::from_node(&tail)))
    );
    lock.unlock().unwrap();
}

#[test]
fn small_captured_task_spans_charge_the_complete_file_for_each_read() {
    // AC-0174: payload bytes and validation work are separate bounds. Two tiny
    // ranges from one retained file incur two complete-file validations.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    let text = format!("{ORIGINAL}// {}\n", "padding".repeat(2048));
    std::fs::write(root.join("source.ts"), &text).unwrap();
    let state = app_state(&app_data);
    let source = crate::register_local_source(&state, &root).unwrap();
    let captured = capture(&state, &source, true);
    let (owner, candidate) = rule_pair(&captured);
    install_task(&state, &owner, &[candidate]);
    let prepared = prepare(&state, GAP, "task:validation-bytes").unwrap();
    assert_eq!(prepared.task().evidence.len(), 2);
    assert!(
        prepared
            .task()
            .evidence
            .iter()
            .all(|item| item.text.len() < 100)
    );
    assert!(text.len() > 8192);
    assert_eq!(
        prepared.source_basis().selection.captured_validation_bytes,
        2 * text.len() as u64
    );
}

#[test]
fn captured_task_refuses_ninth_file_validation_before_exceeding_128_mib() {
    // AC-0174: use the actual fixed production budget and real producer receipts.
    // Nine tiny spans would require 135 MiB of complete-file validation, although
    // their supplied source text fits both the 8 KiB and 48 KiB payload limits.
    use std::io::Write;

    const FILE_BYTES: usize = 15 * 1024 * 1024;
    const SOURCE_CANARY: &str = "validation-byte-budget-source-canary";
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    let path = root.join("source.ts");
    let mut header = String::new();
    for index in 0..9 {
        header.push_str(&format!(
            "export function budget{index}(ready: boolean) {{ if (ready) return {index}; }}\n"
        ));
    }
    header.push_str(&format!("/* {SOURCE_CANARY}\n"));
    {
        // Stream the padding instead of retaining another 15 MiB fixture String
        // beside the production capture buffer and parser/cache allocations.
        let mut file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        file.write_all(header.as_bytes()).unwrap();
        let padding = [b'x'; 4096];
        let mut remaining = FILE_BYTES - header.len() - b"*/\n".len();
        while remaining > 0 {
            let count = remaining.min(padding.len());
            file.write_all(&padding[..count]).unwrap();
            remaining -= count;
        }
        file.write_all(b"*/\n").unwrap();
        file.flush().unwrap();
    }
    assert_eq!(std::fs::metadata(&path).unwrap().len(), FILE_BYTES as u64);
    let state = app_state(&app_data);
    let source = crate::register_local_source(&state, &root).unwrap();
    let captured = capture(&state, &source, true);
    let mut rules: Vec<_> = captured
        .nodes
        .iter()
        .filter(|node| node.label == "BusinessRule")
        .cloned()
        .collect();
    assert_eq!(rules.len(), 9);
    let mut supplied_bytes = 0u64;
    for rule in &rules {
        let receipt = receipt_for(&captured, rule);
        let range = receipt
            .ranges()
            .iter()
            .find(|range| range.role == RangeRole::Provenance && range.index == 0)
            .unwrap();
        assert_eq!(range.captured.file.byte_len, FILE_BYTES as u64);
        let bytes = range.captured.byte_end - range.captured.byte_start;
        assert!(bytes > 0 && bytes < 100);
        supplied_bytes += bytes;
    }
    assert!(supplied_bytes < SelectionLimits::default().total_evidence_bytes as u64);
    let owner = rules.remove(0);
    install_task(&state, &owner, &rules);
    let graph = state.graph.lock().unwrap().read_snapshot().unwrap();
    let error =
        prepare_from_snapshot(&state, graph, GAP, "task:validation-cap", || {}).unwrap_err();
    let report = error.selection.as_ref().unwrap();
    assert_eq!(report.stop_reason, SelectionStopReason::ValidationByteLimit);
    assert_eq!(report.limits.captured_validation_bytes, 128 * 1024 * 1024);
    assert_eq!(report.captured_validation_bytes, 8 * FILE_BYTES as u64);
    assert!(report.captured_validation_bytes <= report.limits.captured_validation_bytes);
    assert!(
        report.captured_validation_bytes + FILE_BYTES as u64
            > report.limits.captured_validation_bytes
    );
    assert_eq!(
        (
            report.metadata_lookahead,
            report.acquisition_attempts,
            report.metadata_preselected_not_read
        ),
        (10, 10, 0)
    );
    assert_eq!(
        (report.supplied_evidence, report.supplied_candidates),
        (8, 7)
    );
    assert!(!report.unread_tail);
    assert_eq!(report.omissions.len(), 1);
    assert_eq!(report.omissions[0].request_index, 1);
    assert_eq!(
        report.omissions[0].reason,
        SelectionOmissionReason::MissingCitation
    );
    let displayed = error.to_string();
    assert!(displayed.contains("validation_byte_limit"));
    assert!(!displayed.contains(SOURCE_CANARY));
    assert!(!displayed.contains(&owner.id));
    assert!(!displayed.contains(root.to_str().unwrap()));
    // The failed preparation must release its shared retention leases too.
    let preview = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(
        (preview.captures, preview.files, preview.bytes),
        (1, 1, FILE_BYTES as u64)
    );
    primary_source::forget(&state, &source.source_id, &preview.fingerprint).unwrap();
    let forgotten = primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(
        (forgotten.captures, forgotten.files, forgotten.bytes),
        (0, 0, 0)
    );
}
