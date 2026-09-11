use super::assessment::{Availability, BasisComparison, assess};
use super::tests::{
    GAP, ORIGINAL, capture, directory, install_task, replace_current_receipts, rule_pair,
};
use super::*;
use crate::registered_source_tests::app_state;
use llm::{
    Completion, EgressFirewall, EgressPolicy, Embedding, LlmProvider, Locality, ProviderCaps,
    ProviderCompletionRequest, ProviderError,
};
use std::sync::Mutex;

struct DuringCall<F> {
    task: AgentTask,
    expect_prepared: bool,
    change: Mutex<Option<F>>,
}

impl<F: FnOnce() + Send> LlmProvider for DuringCall<F> {
    fn id(&self) -> &str {
        "test:captured-task-completion"
    }
    fn locality(&self) -> Locality {
        Locality::Local
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: false,
            chat: true,
            tool_use: false,
        }
    }
    fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        Err(ProviderError::Unsupported("embeddings"))
    }
    fn complete(&self, request: &ProviderCompletionRequest) -> Result<Completion, ProviderError> {
        // This is the actual firewall-authorized, redacted provider request.
        assert_eq!(request.spans().len(), self.task.evidence.len());
        for (span, original) in request.spans().iter().zip(&self.task.evidence) {
            assert_eq!(span.text, original.text);
            assert_eq!(span.byte_start, original.source.byte_start);
            assert_eq!(span.byte_end, original.source.byte_end);
        }
        assert_eq!(
            request.prompt().contains("source_basis"),
            self.expect_prepared
        );
        self.change.lock().unwrap().take().expect("one model call")();
        Ok(Completion { text: serde_json::json!({
            "target_id": self.task.candidates[0].node_id,
            "annotation": "The cited observations suggest a relationship for review.",
            "citations": [self.task.source_evidence_ids[0], self.task.candidates[0].evidence_ids[0]],
        }).to_string() })
    }
}

fn export(state: &AppState) -> serde_json::Value {
    serde_json::to_value(
        crate::build_spec_bundle(
            &*state.graph.lock().unwrap(),
            &state.decisions.lock().unwrap(),
            spec::ExportMode::BestEffort,
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn completed_captured_task_keeps_old_available_receipts_after_equal_fact_reingest() {
    // AC-0175/0176/0177/0178: receipt-only publication changes the current
    // association comparison without making historical captured bytes unavailable.
    let dir = tempfile::tempdir().unwrap();
    let private = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    let path = root.join("source.ts");
    std::fs::write(&path, ORIGINAL).unwrap();
    let state = app_state(&private);
    let source = crate::register_local_source(&state, &root).unwrap();
    let captured = capture(&state, &source, true);
    let (owner, target) = rule_pair(&captured);
    install_task(&state, &owner, &[target]);
    let task = prepare(&state, GAP, "escalate:fixture").unwrap();
    let original_basis = task.source_basis().clone();
    let before = export(&state);
    let (_, execution) = crate::start_job(&state, "escalate:captured:local").unwrap();
    let provider = DuringCall {
        task: task.task().clone(),
        expect_prepared: true,
        change: Mutex::new(Some(|| {
            std::fs::write(&path, ORIGINAL.replace("trailing-A", "trailing-B")).unwrap();
            let next = capture(&state, &source, false);
            replace_current_receipts(&state, &source, &next);
        })),
    };
    let proposal = agents::AgentBroker::bounded_default()
        .propose_prepared(
            &provider,
            &EgressFirewall::new(EgressPolicy::local_only()),
            &task,
            None,
        )
        .unwrap();
    let staged =
        crate::stage_completed_prepared_job_proposal(&state, &execution, &task, &proposal).unwrap();
    assert_eq!(staged.source_basis.as_ref(), Some(&original_basis));
    let changed = assess(&state, &staged.proposal_id).unwrap();
    assert_eq!(changed.graph_comparison, BasisComparison::Unchanged);
    assert_eq!(changed.association_comparison, BasisComparison::Changed);
    assert!(
        changed
            .evidence
            .iter()
            .all(|item| item.status == Availability::Available)
    );
    let next = prepare(&state, GAP, "escalate:fixture").unwrap();
    assert_ne!(next.basis_hash().unwrap(), task.basis_hash().unwrap());
    assert_eq!(next.task().evidence, task.task().evidence);
    assert_eq!(export(&state), before);
    assert_eq!(
        state
            .proposals
            .lock()
            .unwrap()
            .get(&staged.proposal_id)
            .unwrap()
            .unwrap(),
        staged
    );
}

#[test]
fn completed_captured_task_survives_forgetting_cancel_cleanup_and_graph_clear() {
    // AC-0177/0178: source guards must end before the provider, and history
    // staging may not consult current captures or require a live job row.
    let dir = tempfile::tempdir().unwrap();
    let private = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), ORIGINAL).unwrap();
    let state = app_state(&private);
    let source = crate::register_local_source(&state, &root).unwrap();
    let captured = capture(&state, &source, true);
    let (owner, target) = rule_pair(&captured);
    install_task(&state, &owner, &[target]);
    let task = prepare(&state, GAP, "escalate:forgotten").unwrap();
    let before_graph = state.graph.lock().unwrap().read_snapshot().unwrap();
    let (job, execution) = crate::start_job(&state, "escalate:forgotten:local").unwrap();
    let provider = DuringCall {
        task: task.task().clone(),
        expect_prepared: true,
        change: Mutex::new(Some(|| {
            let preview = crate::primary_source::preview(&state, &source.source_id).unwrap();
            assert_eq!(preview.staged_references, 0);
            crate::primary_source::forget(&state, &source.source_id, &preview.fingerprint).unwrap();
            state.jobs.lock().unwrap().cancel(job.id).unwrap();
            state.jobs.lock().unwrap().clear_finished().unwrap();
        })),
    };
    let proposal = agents::AgentBroker::bounded_default()
        .propose_prepared(
            &provider,
            &EgressFirewall::new(EgressPolicy::local_only()),
            &task,
            None,
        )
        .unwrap();
    let staged =
        crate::stage_completed_prepared_job_proposal(&state, &execution, &task, &proposal).unwrap();
    assert!(state.jobs.lock().unwrap().get(job.id).is_err());
    assert_eq!(staged.source_basis.as_ref(), Some(task.source_basis()));
    assert!(
        !serde_json::to_string(&staged)
            .unwrap()
            .contains("const disabled")
    );
    assert_eq!(
        state.graph.lock().unwrap().read_snapshot().unwrap(),
        before_graph
    );
    let unavailable = assess(&state, &staged.proposal_id).unwrap();
    assert_eq!(unavailable.graph_comparison, BasisComparison::Unchanged);
    assert_eq!(
        unavailable.association_comparison,
        BasisComparison::Unchanged
    );
    assert!(
        unavailable
            .evidence
            .iter()
            .all(|item| item.status == Availability::Unavailable)
    );
    let preview = crate::primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(preview.staged_references, task.task().evidence.len());
    let reviewed = state
        .proposals
        .lock()
        .unwrap()
        .review(
            &staged.proposal_id,
            0,
            agents::ProposalDecision::Accepted,
            None,
        )
        .unwrap();
    assert_eq!(reviewed.source_basis, staged.source_basis);
    assert_eq!(
        reviewed.proposal.provenance.confidence_tier,
        ConfidenceTier::InferredWeak
    );
    assert_eq!(
        reviewed.context_status,
        agents::ContextStatus::AwaitingReconciliation
    );
    {
        let mut graph = state.graph.lock().unwrap();
        use core_graph::GraphStore;
        for node in before_graph.0 {
            graph.delete_node(&node.id).unwrap();
        }
    }
    let changed = assess(&state, &staged.proposal_id).unwrap();
    assert_eq!(changed.graph_comparison, BasisComparison::Changed);
    assert_eq!(changed.association_comparison, BasisComparison::Changed);
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
    assert!(state.decisions.lock().unwrap().list().unwrap().is_empty());
}

#[test]
fn captured_class_prepares_only_reached_instances_and_preserves_completed_results() {
    // AC-0176/0178: the same generic production loop holds only gap IDs. A
    // cancellation during the first call prevents the next source preparation.
    let dir = tempfile::tempdir().unwrap();
    let private = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), ORIGINAL).unwrap();
    let state = app_state(&private);
    let source = crate::register_local_source(&state, &root).unwrap();
    let captured = capture(&state, &source, true);
    let (owner, target) = rule_pair(&captured);
    install_task(&state, &owner, &[target]);
    let (job, execution) = crate::start_job(&state, "escalate-class:2:local").unwrap();
    let mut preparations = 0;
    let outcome = crate::run_job_staged_batch(
        &state,
        &execution,
        vec![
            (GAP.into(), Ok(GAP.to_string())),
            ("unread".into(), Ok("unread".to_string())),
        ],
        |_, _| Ok(()),
        |gap_id| {
            preparations += 1;
            let task = prepare(&state, gap_id, "escalate:class")?;
            let provider = DuringCall {
                task: task.task().clone(),
                expect_prepared: true,
                change: Mutex::new(Some(|| {
                    state.jobs.lock().unwrap().cancel(job.id).unwrap();
                })),
            };
            let proposal = agents::AgentBroker::bounded_default()
                .propose_prepared(
                    &provider,
                    &EgressFirewall::new(EgressPolicy::local_only()),
                    &task,
                    None,
                )
                .map_err(|error| error.to_string())?;
            crate::stage_completed_prepared_job_proposal(&state, &execution, &task, &proposal)
        },
    )
    .unwrap();
    assert_eq!(preparations, 1);
    assert!(outcome.cancelled);
    assert!(outcome.failures.is_empty());
    assert_eq!(outcome.proposals.len(), 1);
}

#[test]
fn selected_basis_assessment_distinguishes_storage_failure_and_legacy_unverified() {
    // AC-0177: current graph storage failure is neither graph equality nor
    // missing retained source; v1 assessment never rereads a working tree.
    let dir = tempfile::tempdir().unwrap();
    let private = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), ORIGINAL).unwrap();
    let state = app_state(&private);
    let source = crate::register_local_source(&state, &root).unwrap();
    let captured = capture(&state, &source, true);
    let (owner, target) = rule_pair(&captured);
    install_task(&state, &owner, &[target]);
    let task = prepare(&state, GAP, "escalate:assessment").unwrap();
    let provider = DuringCall {
        task: task.task().clone(),
        expect_prepared: true,
        change: Mutex::new(Some(|| {})),
    };
    let broker = agents::AgentBroker::bounded_default();
    let firewall = EgressFirewall::new(EgressPolicy::local_only());
    let proposal = broker
        .propose_prepared(&provider, &firewall, &task, None)
        .unwrap();
    let staged = state
        .proposals
        .lock()
        .unwrap()
        .stage_prepared(&task, &proposal, 1, &task.source_basis().graph_snapshot_id)
        .unwrap();
    let provider = DuringCall {
        task: task.task().clone(),
        expect_prepared: false,
        change: Mutex::new(Some(|| {})),
    };
    let legacy_result = broker
        .propose(&provider, &firewall, task.task(), None)
        .unwrap();
    let legacy = state
        .proposals
        .lock()
        .unwrap()
        .stage(
            task.task(),
            &legacy_result,
            2,
            &task.source_basis().graph_snapshot_id,
        )
        .unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    // Poisoning represents an operational failure, not an absent association.
    // It does not mutate graph rows or retained receipts.
    std::thread::scope(|scope| {
        assert!(
            scope
                .spawn(|| {
                    let _graph = state.graph.lock().unwrap();
                    panic!("controlled graph-lock failure");
                })
                .join()
                .is_err()
        );
    });
    let observed = assess(&state, &staged.proposal_id).unwrap();
    assert_eq!(
        observed.graph_comparison,
        BasisComparison::OperationalFailure
    );
    assert_eq!(
        observed.association_comparison,
        BasisComparison::OperationalFailure
    );
    assert!(observed.current_graph_snapshot_id.is_none());
    assert!(
        observed
            .evidence
            .iter()
            .all(|item| item.status == Availability::Available)
    );
    let old = assess(&state, &legacy.proposal_id).unwrap();
    assert_eq!(old.graph_comparison, BasisComparison::LegacyUnverified);
    assert_eq!(
        old.association_comparison,
        BasisComparison::LegacyUnverified
    );
    assert_eq!(old.unverified_evidence, legacy.basis.evidence.len());
    assert_eq!(old.captured_validation_bytes, 0);
    assert!(
        old.evidence
            .iter()
            .all(|item| item.status == Availability::WorkingTreeUnverified)
    );
    assert_eq!(
        state
            .proposals
            .lock()
            .unwrap()
            .get(&legacy.proposal_id)
            .unwrap()
            .unwrap(),
        legacy
    );
}
