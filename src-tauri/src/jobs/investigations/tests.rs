use super::*;
use crate::job_execution::JobExecutionLocks;
use crate::jobs::ClaimMode;
use agents::{TaskFactKey, TaskFactSelection, TaskSourceAssociation};

struct Fixture {
    directory: tempfile::TempDir,
    store: JobStore,
    locks: JobExecutionLocks,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let store = JobStore::open(directory.path().join("state.db")).unwrap();
        let locks = JobExecutionLocks::open(directory.path(), store.execution_namespace()).unwrap();
        Self {
            directory,
            store,
            locks,
        }
    }
    fn start(&mut self, nonce: &str, mode: InvestigationProviderMode) -> (String, JobExecution) {
        let detail = self
            .store
            .start_investigation(&request(nonce, mode), Some(&provider(mode)))
            .unwrap()
            .detail;
        let id = detail.summary.investigation_id;
        let plan = self
            .store
            .claim_plan(detail.summary.job_id, ClaimMode::StartQueued)
            .unwrap();
        let reservation = self.locks.try_reserve(plan.lock_target()).unwrap();
        let (job, execution) = self.store.claim_execution(&plan, reservation).unwrap();
        assert_eq!(job.investigation_id.as_deref(), Some(id.as_str()));
        assert_eq!(
            self.store.investigation(&id).unwrap().summary.status,
            InvestigationStatus::Preparing
        );
        (id, execution)
    }
    fn prepare(
        &mut self,
        id: &str,
        execution: &JobExecution,
        with_receipt: bool,
    ) -> InvestigationInputLedger {
        let ledger = ledger(with_receipt);
        let detail = self.store.investigation(id).unwrap();
        let usage = InvestigationUsage {
            selected_facts: ledger.selected_facts.len(),
            ..detail.usage
        };
        self.store
            .advance_investigation(
                execution,
                detail.summary.revision,
                InvestigationTransition::InputPrepared {
                    ledger: Box::new(ledger.clone()),
                    usage,
                },
            )
            .unwrap();
        ledger
    }
}

fn request(nonce: &str, mode: InvestigationProviderMode) -> StartInvestigationRequest {
    StartInvestigationRequest {
        schema_version: 1,
        request_nonce: nonce.into(),
        specialist_id: SpecialistId::DomainAnalyst,
        question: "What evidence supports the current design?".into(),
        scope: InvestigationScope::All,
        provider_mode: mode,
        limit_profile: "investigation-v1".into(),
        expected_graph_revision: None,
        conversation_id: None,
        parent_id: None,
    }
}
fn provider(mode: InvestigationProviderMode) -> InvestigationProvider {
    InvestigationProvider {
        mode,
        provider_id: match mode {
            InvestigationProviderMode::Local => "local:fixture",
            InvestigationProviderMode::Cloud => "cloud:fixture",
        }
        .into(),
        model: "fixture-model".into(),
        endpoint: match mode {
            InvestigationProviderMode::Local => "http://127.0.0.1:11434",
            InvestigationProviderMode::Cloud => "https://provider.invalid",
        }
        .into(),
        deployment: None,
        available: true,
        unavailable_reason: None,
    }
}
fn hash(label: &str) -> String {
    core_prov::content_hash(label.as_bytes())
}
fn ledger(with_receipt: bool) -> InvestigationInputLedger {
    let fact = TaskFactKey::Node {
        id: "symbol:fixture".into(),
    };
    let digest = format!("node-v1:{}", hash("fact"));
    let receipt = format!("ts-primary-v2:{}", hash("receipt"));
    let mut ledger = InvestigationInputLedger {
        schema_version: 1,
        graph_snapshot_id: format!("context-v1:{}", hash("graph")),
        scope_snapshot_id: format!("context-v1:{}", hash("scope")),
        revision: 0,
        selected_facts: vec![TaskFactSelection {
            fact: fact.clone(),
            fact_digest: digest.clone(),
            binding: with_receipt.then(|| TaskSourceAssociation {
                repo_key: "repo:fixture".into(),
                receipt_id: receipt.clone(),
                emitted_fact_digest: digest.clone(),
            }),
        }],
        receipt_references: vec![],
        citations: vec![InvestigationCitation {
            citation_id: "citation:graph".into(),
            fact,
            fact_digest: digest,
            source: None,
            role: None,
            index: None,
            text_hash: None,
            origin: InvestigationEvidenceOrigin::GraphMetadata,
        }],
        queries: vec![InvestigationQueryManifest {
            query: InvestigationQuery {
                scope: InvestigationScope::All,
                kind: None,
                labels: vec![],
                max_facts: 1,
                max_bytes: 1024,
                cursor: None,
            },
            response_hash: hash("fixture-query-response"),
            response_bytes: 128,
            returned_facts: 1,
            total_selected: 1,
            has_more: false,
        }],
        supplied_history_hash: None,
        supplied_history_bytes: 0,
    };
    if with_receipt {
        ledger
            .receipt_references
            .push(agents::investigation::InvestigationReceiptReference {
                source_id: "src_0123456789abcdef0123456789abcdef".into(),
                repo_key: "repo:fixture".into(),
                receipt_id: receipt,
            });
    }
    ledger.validate().unwrap();
    ledger
}
fn step(ledger: &InvestigationInputLedger, index: u32) -> PendingInvestigationStep {
    PendingInvestigationStep {
        step_id: format!("step:{index}"),
        payload_hash: hash(&format!("payload:{index}")),
        input_ledger_hash: ledger.fingerprint().unwrap(),
        input_bytes: 1024,
        generated_tokens: 2048,
    }
}
fn begin(
    fixture: &mut Fixture,
    id: &str,
    execution: &JobExecution,
    step: PendingInvestigationStep,
) -> InvestigationDetail {
    let detail = fixture.store.investigation(id).unwrap();
    fixture
        .store
        .advance_investigation(
            execution,
            detail.summary.revision,
            InvestigationTransition::BeginInvocation { step },
        )
        .unwrap()
}
fn response(step_id: &str, active_milliseconds: u64) -> InvestigationResponseMeta {
    InvestigationResponseMeta {
        step_id: step_id.into(),
        response_hash: hash("response"),
        action_hash: hash("action"),
        observed_model: Some("fixture-model".into()),
        reported_input_tokens: None,
        reported_output_tokens: None,
        active_milliseconds,
    }
}
fn finish(id: &str, ledger: &InvestigationInputLedger) -> InvestigationResult {
    InvestigationResult::admit(
        id,
        ledger,
        &InvestigationAction::Finish {
            findings: vec![ProposedInvestigationFinding {
                claim_kind: InvestigationClaimKind::InferredInterpretation,
                title: "Bounded support".into(),
                statement: "The selected metadata suggests a responsibility worth examining."
                    .into(),
                citation_ids: vec!["citation:graph".into()],
                limitations: vec!["Implementation behavior remains unverified.".into()],
            }],
            knowledge_completeness: KnowledgeCompleteness::Partial,
            limitations: vec!["The investigation covers the selected input only.".into()],
        },
        std::iter::empty(),
        Some("fixture-model".into()),
        "2026-09-11T00:00:00Z".into(),
    )
    .unwrap()
}

#[test]
fn investigation_start_deduplicates_concurrently_before_availability_and_capacity() {
    // AC-0181/0188: two independent connections contend on the same request,
    // producing one task/conversation/job, even when the start acknowledgment is lost.
    let mut fixture = Fixture::new();
    let other = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let second = barrier.clone();
    let thread = std::thread::spawn(move || {
        let mut other = other;
        second.wait();
        other
            .start_investigation(
                &request("same", InvestigationProviderMode::Local),
                Some(&provider(InvestigationProviderMode::Local)),
            )
            .unwrap()
    });
    barrier.wait();
    let first = fixture
        .store
        .start_investigation(
            &request("same", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap();
    let second = thread.join().unwrap();
    assert_eq!(
        first.detail.summary.investigation_id,
        second.detail.summary.investigation_id
    );
    assert_ne!(first.created, second.created);
    assert_eq!(fixture.store.list().unwrap().len(), 1);
    fixture
        .store
        .start_investigation(
            &request("second", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap();
    assert_eq!(
        fixture
            .store
            .start_investigation(
                &request("third", InvestigationProviderMode::Local),
                Some(&provider(InvestigationProviderMode::Local))
            )
            .unwrap_err(),
        InvestigationStoreError::Capacity
    );
    let prior = fixture
        .store
        .start_investigation(&request("same", InvestigationProviderMode::Local), None)
        .unwrap();
    assert!(!prior.created);
    let mut changed = request("same", InvestigationProviderMode::Local);
    changed.question = "Different input".into();
    assert_eq!(
        fixture
            .store
            .start_investigation(&changed, None)
            .unwrap_err(),
        InvestigationStoreError::Conflict
    );
}

#[test]
fn investigation_execution_attachment_rolls_back_job_claim_on_storage_failure() {
    // AC-0181: Job claim updates happen before the coordinator attachment hook,
    // and any hook failure rolls both the running status and generation back.
    let mut fixture = Fixture::new();
    let detail = fixture
        .store
        .start_investigation(
            &request("atomic", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail;
    fixture.store.conn.execute("CREATE TRIGGER unexpected_attachment_trigger BEFORE INSERT ON investigation_events BEGIN SELECT RAISE(IGNORE); END",[]).unwrap();
    let plan = fixture
        .store
        .claim_plan(detail.summary.job_id, ClaimMode::StartQueued)
        .unwrap();
    let reservation = fixture.locks.try_reserve(plan.lock_target()).unwrap();
    assert!(fixture.store.claim_execution(&plan, reservation).is_err());
    let state:(String,i64)=fixture.store.conn.query_row("SELECT j.status,a.generation FROM jobs j JOIN job_attempts a ON a.job_id=j.id WHERE j.id=?1",[detail.summary.job_id],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(state, ("queued".into(), 0));
    fixture
        .store
        .conn
        .execute("DROP TRIGGER unexpected_attachment_trigger", [])
        .unwrap();
    assert_eq!(
        fixture
            .store
            .investigation(&detail.summary.investigation_id)
            .unwrap()
            .summary
            .status,
        InvestigationStatus::Queued
    );
    let plan = fixture
        .store
        .claim_plan(detail.summary.job_id, ClaimMode::StartQueued)
        .unwrap();
    let reservation = fixture.locks.try_reserve(plan.lock_target()).unwrap();
    let (_, execution) = fixture.store.claim_execution(&plan, reservation).unwrap();
    let task = load_task(&fixture.store.conn, &detail.summary.investigation_id).unwrap();
    let identity = task.execution.unwrap();
    assert_eq!(identity.owner, execution.inner.owner);
    assert_eq!(identity.generation, execution.inner.generation);
    assert_eq!(task.phase, Phase::Preparing);
}

#[test]
fn investigation_cancelled_finish_survives_job_cleanup_restart_and_original_basis() {
    // AC-0187/0188: the response uses its admitted original ledger after cancel
    // and cleanup. History is independent of Jobs, raw questions are redacted,
    // and observed cooperative time overshoot remains visible rather than clamped.
    let mut fixture = Fixture::new();
    let mut request = request("late-finish", InvestigationProviderMode::Local);
    let canary = "ghp_abcdefgh1234_private_question_canary";
    request.question = format!("Inspect this question token {canary}");
    let created = fixture
        .store
        .start_investigation(&request, Some(&provider(InvestigationProviderMode::Local)))
        .unwrap()
        .detail;
    let id = created.summary.investigation_id;
    let plan = fixture
        .store
        .claim_plan(created.summary.job_id, ClaimMode::StartQueued)
        .unwrap();
    let reservation = fixture.locks.try_reserve(plan.lock_target()).unwrap();
    let (_, execution) = fixture.store.claim_execution(&plan, reservation).unwrap();
    let ledger = fixture.prepare(&id, &execution, true);
    let started = begin(&mut fixture, &id, &execution, step(&ledger, 1));
    fixture.store.cancel(started.summary.job_id).unwrap();
    assert_eq!(fixture.store.clear_finished().unwrap(), 1);
    assert!(fixture.store.get(started.summary.job_id).is_err());
    let result = finish(&id, &ledger);
    let current = fixture.store.investigation(&id).unwrap();
    let completed = fixture
        .store
        .advance_investigation(
            &execution,
            current.summary.revision,
            InvestigationTransition::ActionAdmitted {
                response: response("step:1", 600_123),
                tool: None,
                result: Some(Box::new(result.clone())),
            },
        )
        .unwrap();
    assert_eq!(completed.summary.status, InvestigationStatus::Completed);
    assert!(completed.summary.cancel_requested && completed.summary.has_result);
    assert_eq!(completed.usage.active_milliseconds, 600_123);
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation_result(&id).unwrap(), Some(result));
    assert_eq!(reopened.investigation_ledger(&id).unwrap(), Some(ledger));
    assert!(reopened.list().unwrap().is_empty());
    let serialized = serde_json::to_string(&reopened.investigation(&id).unwrap()).unwrap();
    assert!(!serialized.contains(canary));
    assert!(!serialized.contains("\"owner\":"));
    assert!(!serialized.contains(&execution.inner.owner));
    let events = reopened.investigation_events(&id, 0).unwrap();
    assert!(
        events
            .items
            .iter()
            .any(|event| event.kind == InvestigationEventKind::ResultPersisted)
    );
    assert!(!serde_json::to_string(&events).unwrap().contains(canary));
    for table in [
        "investigation_events",
        "investigation_inputs",
        "investigation_results",
    ] {
        assert!(
            fixture
                .store
                .conn
                .execute(&format!("DELETE FROM {table} WHERE task_id=?1"), [&id])
                .is_err()
        );
    }
}

#[test]
fn investigation_exact_step_consent_is_single_use_and_decline_never_changes_provider() {
    // AC-0186: a safe identity can wake the owner but cannot reconstruct its
    // preview. Matching approval and invocation budget consumption share a CAS.
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start("cloud", InvestigationProviderMode::Cloud);
    let ledger = fixture.prepare(&id, &execution, false);
    let step = step(&ledger, 1);
    let detail = fixture.store.investigation(&id).unwrap();
    let waiting = fixture
        .store
        .advance_investigation(
            &execution,
            detail.summary.revision,
            InvestigationTransition::AwaitConsent { step: step.clone() },
        )
        .unwrap();
    assert_eq!(
        fixture.store.investigation_pending_step(&id).unwrap(),
        Some((waiting.summary.revision, step.clone(), false))
    );
    assert_eq!(
        fixture
            .store
            .approve_investigation(&id, waiting.summary.revision, &step.step_id, &hash("wrong"))
            .unwrap_err(),
        InvestigationStoreError::Stale
    );
    assert_eq!(
        fixture
            .store
            .investigation(&id)
            .unwrap()
            .usage
            .model_invocations,
        0
    );
    let approved = fixture
        .store
        .approve_investigation(
            &id,
            waiting.summary.revision,
            &step.step_id,
            &step.payload_hash,
        )
        .unwrap();
    assert_eq!(
        fixture.store.investigation_pending_step(&id).unwrap(),
        Some((approved.summary.revision, step.clone(), true))
    );
    assert!(
        fixture
            .store
            .approve_investigation(
                &id,
                waiting.summary.revision,
                &step.step_id,
                &step.payload_hash
            )
            .is_err()
    );
    let started = fixture
        .store
        .advance_investigation(
            &execution,
            approved.summary.revision,
            InvestigationTransition::BeginInvocation { step: step.clone() },
        )
        .unwrap();
    assert_eq!(started.usage.model_invocations, 1);
    assert_eq!(started.usage.generated_token_reservations, 2048);
    assert!(
        fixture
            .store
            .advance_investigation(
                &execution,
                started.summary.revision,
                InvestigationTransition::BeginInvocation { step }
            )
            .is_err()
    );
    let (declined_id, declined_execution) =
        fixture.start("decline", InvestigationProviderMode::Cloud);
    let ledger = fixture.prepare(&declined_id, &declined_execution, false);
    let step = self::step(&ledger, 1);
    let detail = fixture.store.investigation(&declined_id).unwrap();
    let waiting = fixture
        .store
        .advance_investigation(
            &declined_execution,
            detail.summary.revision,
            InvestigationTransition::AwaitConsent { step: step.clone() },
        )
        .unwrap();
    let declined = fixture
        .store
        .decline_investigation(
            &declined_id,
            waiting.summary.revision,
            &step.step_id,
            &step.payload_hash,
        )
        .unwrap();
    assert_eq!(declined.summary.status, InvestigationStatus::Cancelled);
    assert_eq!(declined.provider.mode, InvestigationProviderMode::Cloud);
    assert_eq!(declined.usage.model_invocations, 0);
    assert!(
        fixture
            .store
            .investigation_pending_step(&declined_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn investigation_tool_and_repeated_file_validation_reservations_cannot_reset() {
    // AC-0183/0187: every attempted read reserves tool/request counts, and a
    // corrupt or repeated captured read spends full-file bytes before validation.
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start("read-budgets", InvestigationProviderMode::Local);
    let mut ledger = fixture.prepare(&id, &execution, false);
    for index in 1..=8 {
        let started = begin(&mut fixture, &id, &execution, step(&ledger, index));
        let action = fixture
            .store
            .advance_investigation(
                &execution,
                started.summary.revision,
                InvestigationTransition::ActionAdmitted {
                    response: response(&format!("step:{index}"), u64::from(index)),
                    tool: Some(InvestigationTool::ReadEvidence),
                    result: None,
                },
            )
            .unwrap();
        let reading = fixture
            .store
            .advance_investigation(
                &execution,
                action.summary.revision,
                InvestigationTransition::BeginTool {
                    tool: InvestigationTool::ReadEvidence,
                },
            )
            .unwrap();
        assert_eq!(reading.usage.evidence_requests, index);
        let charged = fixture
            .store
            .advance_investigation(
                &execution,
                reading.summary.revision,
                InvestigationTransition::ChargeEvidence {
                    validation_bytes: 16 * 1024 * 1024,
                },
            )
            .unwrap();
        if index < 8 {
            ledger.revision += 1;
            let mut reset = charged.usage.clone();
            reset.captured_validation_bytes = 0;
            assert_eq!(
                fixture
                    .store
                    .advance_investigation(
                        &execution,
                        charged.summary.revision,
                        InvestigationTransition::InputAdmitted {
                            ledger: Box::new(ledger.clone()),
                            usage: reset
                        }
                    )
                    .unwrap_err(),
                InvestigationStoreError::Invalid
            );
            fixture
                .store
                .advance_investigation(
                    &execution,
                    charged.summary.revision,
                    InvestigationTransition::InputAdmitted {
                        ledger: Box::new(ledger.clone()),
                        usage: charged.usage,
                    },
                )
                .unwrap();
        } else {
            assert_eq!(charged.usage.captured_validation_bytes, 128 * 1024 * 1024);
            assert_eq!(
                fixture
                    .store
                    .advance_investigation(
                        &execution,
                        charged.summary.revision,
                        InvestigationTransition::ChargeEvidence {
                            validation_bytes: 1
                        }
                    )
                    .unwrap_err(),
                InvestigationStoreError::Capacity
            );
            let stopped = fixture
                .store
                .advance_investigation(
                    &execution,
                    charged.summary.revision,
                    InvestigationTransition::StopWithElapsed {
                        reason: InvestigationStopReason::LimitExceeded,
                        active_milliseconds: charged.usage.active_milliseconds,
                    },
                )
                .unwrap();
            assert_eq!(stopped.usage.tool_actions, 8);
            assert_eq!(stopped.usage.evidence_requests, 8);
            assert_eq!(stopped.usage.evidence_items, 0);
            assert_eq!(stopped.usage.captured_validation_bytes, 128 * 1024 * 1024);
        }
    }
}

#[test]
fn investigation_recovery_uses_retained_owner_after_cancel_and_job_cleanup() {
    // AC-0187: generic Job recovery cannot find this cancelled/cleared row.
    // The coordinator retains exact ownership, refuses a live lease, and only
    // marks uncertain dispatch after that lease is actually released.
    for pending in [false, true] {
        let mut fixture = Fixture::new();
        let (id, execution) = fixture.start("recover", InvestigationProviderMode::Local);
        if pending {
            let ledger = fixture.prepare(&id, &execution, false);
            begin(&mut fixture, &id, &execution, step(&ledger, 1));
        }
        fixture.store.cancel_investigation(&id).unwrap();
        fixture.store.clear_finished().unwrap();
        assert!(fixture.store.recovery_candidates().unwrap().is_empty());
        let candidate = fixture
            .store
            .investigation_recovery_candidates()
            .unwrap()
            .remove(0);
        assert!(matches!(
            fixture.locks.try_reserve(candidate.lock_target()),
            Err(JobTransitionError::Busy)
        ));
        drop(execution);
        let reservation = fixture.locks.try_reserve(candidate.lock_target()).unwrap();
        let recovered = fixture
            .store
            .recover_investigation(&candidate, reservation)
            .unwrap()
            .unwrap();
        assert_eq!(
            recovered.summary.status,
            if pending {
                InvestigationStatus::OutcomeUnknown
            } else {
                InvestigationStatus::Interrupted
            }
        );
        assert!(recovered.summary.cancel_requested);
        assert!(
            fixture
                .store
                .investigation_recovery_candidates()
                .unwrap()
                .is_empty()
        );
        assert!(
            !fixture
                .store
                .start_investigation(&request("recover", InvestigationProviderMode::Local), None)
                .unwrap()
                .created
        );
        let events = fixture.store.investigation_events(&id, 0).unwrap();
        assert_eq!(
            events
                .items
                .iter()
                .filter(|event| event.kind == InvestigationEventKind::ModelStarted)
                .count(),
            usize::from(pending)
        );
    }
}

#[test]
fn investigation_unclaimed_queue_is_not_abandoned_and_missing_claimed_lock_fails_closed() {
    // AC-0181/0187: the start→claim gap is not proof of death. An actually
    // claimed missing lock is an operational failure, never automatically recreated.
    let mut fixture = Fixture::new();
    let queued = fixture
        .store
        .start_investigation(
            &request("queued", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail;
    assert!(
        fixture
            .store
            .investigation_recovery_candidates()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fixture
            .store
            .investigation(&queued.summary.investigation_id)
            .unwrap()
            .summary
            .status,
        InvestigationStatus::Queued
    );
    fixture
        .store
        .cancel_investigation(&queued.summary.investigation_id)
        .unwrap();
    let (id, execution) = fixture.start("missing-lock", InvestigationProviderMode::Local);
    let job_id = execution.id();
    drop(execution);
    let candidate = fixture
        .store
        .investigation_recovery_candidates()
        .unwrap()
        .remove(0);
    let path = fixture
        .directory
        .path()
        .join("job-executions")
        .join(&fixture.store.namespace.value)
        .join(job_id.to_string());
    std::fs::remove_file(&path).unwrap();
    assert!(matches!(
        fixture.locks.try_reserve(candidate.lock_target()),
        Err(JobTransitionError::LockUnavailable)
    ));
    assert!(!path.exists());
    assert_eq!(
        fixture.store.investigation(&id).unwrap().summary.status,
        InvestigationStatus::Preparing
    );
}

#[test]
fn investigation_selected_receipts_are_indexed_before_source_reads_and_validated() {
    // AC-0188/0189: a receipt identity is already supplied by the selected fact
    // inventory. It belongs in retention previews before any source text is read.
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start("refs", InvestigationProviderMode::Local);
    let ledger = fixture.prepare(&id, &execution, true);
    let source = &ledger.receipt_references[0];
    let uses = fixture
        .store
        .investigation_receipt_references(&source.source_id)
        .unwrap();
    assert_eq!(
        uses,
        vec![InvestigationReceiptUse {
            investigation_id: id.clone(),
            source_id: source.source_id.clone(),
            repo_key: source.repo_key.clone(),
            receipt_id: source.receipt_id.clone()
        }]
    );
    assert_eq!(
        fixture
            .store
            .investigation(&id)
            .unwrap()
            .usage
            .evidence_items,
        0
    );
    assert!(
        fixture
            .store
            .conn
            .execute(
                "UPDATE investigation_refs SET receipt_id='invalid' WHERE task_id=?1",
                [&id]
            )
            .is_err()
    );
    fixture.store.cancel_investigation(&id).unwrap();
    fixture.store.clear_finished().unwrap();
    assert_eq!(
        fixture
            .store
            .investigation_receipt_references(&source.source_id)
            .unwrap(),
        uses
    );
    assert_eq!(
        fixture.store.investigation_ledger(&id).unwrap(),
        Some(ledger)
    );
}

#[test]
fn investigation_history_is_bounded_stable_and_idempotent_after_full_admission() {
    // AC-0181/0188: a history cursor binds a high-water mark and conversation;
    // job cleanup is independent, and a full store still resolves prior nonces.
    let mut fixture = Fixture::new();
    for index in 0..55 {
        let detail = fixture
            .store
            .start_investigation(
                &request(
                    &format!("history:{index}"),
                    InvestigationProviderMode::Local,
                ),
                Some(&provider(InvestigationProviderMode::Local)),
            )
            .unwrap()
            .detail;
        fixture
            .store
            .cancel_investigation(&detail.summary.investigation_id)
            .unwrap();
    }
    let first = fixture.store.investigation_history(None, None).unwrap();
    assert_eq!(first.items.len(), 50);
    let cursor = first.next_cursor.unwrap();
    for index in 55..128 {
        let detail = fixture
            .store
            .start_investigation(
                &request(
                    &format!("history:{index}"),
                    InvestigationProviderMode::Local,
                ),
                Some(&provider(InvestigationProviderMode::Local)),
            )
            .unwrap()
            .detail;
        fixture
            .store
            .cancel_investigation(&detail.summary.investigation_id)
            .unwrap();
    }
    let second = fixture
        .store
        .investigation_history(None, Some(&cursor))
        .unwrap();
    assert_eq!(second.items.len(), 5);
    assert!(second.next_cursor.is_none());
    assert!(
        fixture
            .store
            .investigation_history(Some(&first.items[0].conversation_id), Some(&cursor))
            .is_err()
    );
    assert_eq!(
        fixture
            .store
            .start_investigation(
                &request("overflow", InvestigationProviderMode::Local),
                Some(&provider(InvestigationProviderMode::Local))
            )
            .unwrap_err(),
        InvestigationStoreError::Capacity
    );
    assert!(
        !fixture
            .store
            .start_investigation(
                &request("history:0", InvestigationProviderMode::Local),
                None
            )
            .unwrap()
            .created
    );
    assert_eq!(fixture.store.clear_finished().unwrap(), 128);
    assert_eq!(
        fixture
            .store
            .investigation_history(None, None)
            .unwrap()
            .items
            .len(),
        50
    );
    assert!(serde_json::to_vec(&second).unwrap().len() < 256 * 1024);
}

#[test]
fn investigation_strict_storage_rejects_oversize_records_and_partial_schemas() {
    // AC-0188: SQL rejects oversized bodies before retrieving them; private
    // schema migration never CREATE-repairs a partial or future owned schema.
    let mut fixture = Fixture::new();
    let detail = fixture
        .store
        .start_investigation(
            &request("corrupt", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail;
    fixture
        .store
        .conn
        .execute(
            "UPDATE investigation_tasks SET record=zeroblob(262145) WHERE id=?1",
            [&detail.summary.investigation_id],
        )
        .unwrap();
    assert_eq!(
        fixture
            .store
            .investigation(&detail.summary.investigation_id)
            .unwrap_err(),
        InvestigationStoreError::Invalid
    );
    for sql in [
        "CREATE TABLE investigation_meta(future TEXT)",
        "CREATE VIEW investigation_tasks AS SELECT 1 AS id",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.db");
        Connection::open(&path).unwrap().execute(sql, []).unwrap();
        assert!(JobStore::open(&path).is_err());
    }
}

#[test]
fn investigation_unknown_usage_stays_unknown_and_stale_decisions_do_not_write() {
    // AC-0185/0186: later measured responses cannot turn an earlier unknown
    // invocation into a complete measured total; stale revisions cause no writes.
    assert_eq!(
        transitions::add_reported(None, Some(7), true).unwrap(),
        Some(7)
    );
    assert_eq!(
        transitions::add_reported(None, Some(7), false).unwrap(),
        None
    );
    assert_eq!(
        transitions::add_reported(Some(5), None, false).unwrap(),
        None
    );
    assert_eq!(
        transitions::add_reported(Some(5), Some(7), false).unwrap(),
        Some(12)
    );
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start("stale", InvestigationProviderMode::Local);
    let ledger = fixture.prepare(&id, &execution, false);
    let before = fixture.store.investigation(&id).unwrap();
    assert_eq!(
        fixture
            .store
            .advance_investigation(
                &execution,
                before.summary.revision - 1,
                InvestigationTransition::BeginInvocation {
                    step: step(&ledger, 1)
                }
            )
            .unwrap_err(),
        InvestigationStoreError::Stale
    );
    assert_eq!(fixture.store.investigation(&id).unwrap(), before);
    assert!(
        fixture
            .store
            .investigation_timestamp(0)
            .unwrap()
            .ends_with('Z')
    );
    assert!(fixture.store.investigation_timestamp(3601).is_err());
}

#[test]
fn investigation_receipt_index_detects_missing_and_relabelled_entries_before_filtering() {
    // AC-0188/0189: canonical triggers are insufficient to certify data after
    // corruption. Reconcile the complete immutable ledger union before filtering.
    for remove in [true, false] {
        let mut fixture = Fixture::new();
        let (id, execution) = fixture.start("corrupt-index", InvestigationProviderMode::Local);
        let ledger = fixture.prepare(&id, &execution, true);
        let source = &ledger.receipt_references[0].source_id;
        let trigger = if remove {
            "investigation_refs_no_delete"
        } else {
            "investigation_refs_no_update"
        };
        let sql: String = fixture
            .store
            .conn
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name=?1",
                [trigger],
                |row| row.get(0),
            )
            .unwrap();
        fixture
            .store
            .conn
            .execute_batch(&format!("DROP TRIGGER {trigger}"))
            .unwrap();
        if remove {
            fixture
                .store
                .conn
                .execute("DELETE FROM investigation_refs WHERE task_id=?1", [&id])
                .unwrap();
        } else {
            fixture.store.conn.execute(
                "UPDATE investigation_refs SET source_id='src_fedcba9876543210fedcba9876543210' WHERE task_id=?1", [&id],
            ).unwrap();
        }
        fixture.store.conn.execute_batch(&sql).unwrap();
        for requested in [source.as_str(), "src_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"] {
            assert_eq!(
                fixture
                    .store
                    .investigation_receipt_references(requested)
                    .unwrap_err(),
                InvestigationStoreError::Invalid
            );
        }
        // The authoritative input remains intact and independently readable.
        assert_eq!(
            fixture.store.investigation_ledger(&id).unwrap(),
            Some(ledger)
        );
    }
}

#[test]
fn investigation_failed_invocation_retains_elapsed_and_marks_aggregate_usage_unknown() {
    // AC-0185/0186/0187: a known failed call and an uncertain call retain actual
    // elapsed time, including cooperative overshoot, without inventing token use.
    for reason in [
        InvestigationStopReason::ProviderFailure,
        InvestigationStopReason::OutcomeUnknown,
    ] {
        let mut fixture = Fixture::new();
        let (id, execution) = fixture.start("failed-metrics", InvestigationProviderMode::Local);
        let mut ledger = fixture.prepare(&id, &execution, false);
        let started = begin(&mut fixture, &id, &execution, step(&ledger, 1));
        let mut measured = response("step:1", 100);
        measured.reported_input_tokens = Some(120);
        measured.reported_output_tokens = Some(24);
        let action = fixture
            .store
            .advance_investigation(
                &execution,
                started.summary.revision,
                InvestigationTransition::ActionAdmitted {
                    response: measured,
                    tool: Some(InvestigationTool::QueryContext),
                    result: None,
                },
            )
            .unwrap();
        let tool = fixture
            .store
            .advance_investigation(
                &execution,
                action.summary.revision,
                InvestigationTransition::BeginTool {
                    tool: InvestigationTool::QueryContext,
                },
            )
            .unwrap();
        ledger.revision += 1;
        fixture
            .store
            .advance_investigation(
                &execution,
                tool.summary.revision,
                InvestigationTransition::InputAdmitted {
                    ledger: Box::new(ledger.clone()),
                    usage: tool.usage,
                },
            )
            .unwrap();
        let pending = begin(&mut fixture, &id, &execution, step(&ledger, 2));
        assert_eq!(pending.usage.reported_input_tokens, Some(120));
        assert_eq!(
            fixture
                .store
                .advance_investigation(
                    &execution,
                    pending.summary.revision,
                    InvestigationTransition::StopWithElapsed {
                        reason,
                        active_milliseconds: 99
                    }
                )
                .unwrap_err(),
            InvestigationStoreError::Invalid
        );
        assert_eq!(fixture.store.investigation(&id).unwrap(), pending);
        let stopped = fixture
            .store
            .advance_investigation(
                &execution,
                pending.summary.revision,
                InvestigationTransition::StopWithElapsed {
                    reason,
                    active_milliseconds: 600_123,
                },
            )
            .unwrap();
        assert_eq!(stopped.usage.active_milliseconds, 600_123);
        assert_eq!(stopped.usage.reported_input_tokens, None);
        assert_eq!(stopped.usage.reported_output_tokens, None);
        assert_eq!(stopped.usage.model_invocations, 2);
        assert_eq!(
            stopped.summary.status,
            if reason == InvestigationStopReason::OutcomeUnknown {
                InvestigationStatus::OutcomeUnknown
            } else {
                InvestigationStatus::Failed
            }
        );
        drop(fixture.store);
        let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
        assert_eq!(reopened.investigation(&id).unwrap(), stopped);
    }
}
