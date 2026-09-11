use super::*;

impl JobStore {
    pub(crate) fn advance_investigation(
        &mut self,
        execution: &JobExecution,
        expected_revision: u64,
        transition: InvestigationTransition,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        execution.verify()?;
        if execution.inner.namespace != self.namespace {
            return Err(InvestigationStoreError::Ownership);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        let id = task_id_for_job(&tx, execution.id())?.ok_or(InvestigationStoreError::Missing)?;
        let mut task = load_task(&tx, &id)?;
        check_identity(&task, execution)?;
        let late_finish = matches!(
            &transition,
            InvestigationTransition::ActionAdmitted {
                result: Some(_),
                ..
            }
        );
        let stopping = matches!(&transition, InvestigationTransition::StopWithElapsed { .. });
        if task.detail.summary.revision != expected_revision {
            return Err(InvestigationStoreError::Stale);
        }
        check_job(&tx, execution, late_finish || stopping)?;
        if task.phase == Phase::Terminal {
            return Err(InvestigationStoreError::Stale);
        }
        if task.detail.summary.cancel_requested && !late_finish && !stopping {
            return Err(InvestigationStoreError::Cancelled);
        }
        match transition {
            InvestigationTransition::InputPrepared { ledger, usage } => {
                if task.phase != Phase::Preparing || task.ledger_revision.is_some() {
                    return Err(InvestigationStoreError::Stale);
                }
                if task
                    .expected_graph_revision
                    .as_ref()
                    .is_some_and(|expected| expected != &ledger.graph_snapshot_id)
                {
                    return Err(InvestigationStoreError::Conflict);
                }
                admit_input(&tx, &mut task, &ledger, usage, true)?;
            }
            InvestigationTransition::InputAdmitted { ledger, usage } => {
                if task.phase != Phase::Tool {
                    return Err(InvestigationStoreError::Stale);
                }
                admit_input(&tx, &mut task, &ledger, usage, false)?;
            }
            InvestigationTransition::AwaitConsent { step } => {
                check_ready(&task, &step)?;
                if task.detail.provider.mode != InvestigationProviderMode::Cloud {
                    return Err(InvestigationStoreError::Invalid);
                }
                task.phase = Phase::Consent;
                task.detail.summary.status = InvestigationStatus::AwaitingConsent;
                task.pending = Some(step.clone());
                task.approved = false;
                bump(&tx, &mut task)?;
                event(
                    &tx,
                    &mut task,
                    InvestigationEventKind::ConsentRequired,
                    Some(&step.step_id),
                    None,
                    "Exact step consent is required; no provider call has started.",
                )?;
            }
            InvestigationTransition::BeginInvocation { step } => {
                validate_step(&step)?;
                if task.ledger_hash.as_ref() != Some(&step.input_ledger_hash) {
                    return Err(InvestigationStoreError::Conflict);
                }
                match task.detail.provider.mode {
                    InvestigationProviderMode::Local => check_ready(&task, &step)?,
                    InvestigationProviderMode::Cloud => {
                        if task.phase != Phase::Consent
                            || !task.approved
                            || task.pending.as_ref() != Some(&step)
                        {
                            return Err(InvestigationStoreError::Stale);
                        }
                    }
                }
                available_time(&task)?;
                let mut usage = task.detail.usage.clone();
                usage.model_invocations = usage
                    .model_invocations
                    .checked_add(1)
                    .ok_or(InvestigationStoreError::Capacity)?;
                usage.generated_token_reservations = usage
                    .generated_token_reservations
                    .checked_add(step.generated_tokens)
                    .ok_or(InvestigationStoreError::Capacity)?;
                validate_usage(&usage, &task.detail.limits)?;
                if read_body::<PendingInvestigationStep>(
                    &tx,
                    "investigation_invocations",
                    "step_id",
                    &id,
                    &step.step_id,
                )?
                .is_some()
                {
                    return Err(InvestigationStoreError::Conflict);
                }
                insert_body(
                    &tx,
                    "investigation_invocations",
                    &id,
                    &step.step_id,
                    &step,
                    4096,
                )?;
                task.detail.usage = usage;
                task.pending = Some(step.clone());
                task.approved = false;
                task.phase = Phase::Invocation;
                task.detail.summary.status = InvestigationStatus::Running;
                bump(&tx, &mut task)?;
                event(
                    &tx,
                    &mut task,
                    InvestigationEventKind::ModelStarted,
                    Some(&step.step_id),
                    None,
                    "Invocation boundary committed; the provider outcome is pending.",
                )?;
            }
            InvestigationTransition::ActionAdmitted {
                response,
                tool,
                result,
            } => {
                if task.phase != Phase::Invocation || tool.is_some() == result.is_some() {
                    return Err(InvestigationStoreError::Stale);
                }
                let pending = task
                    .pending
                    .as_ref()
                    .ok_or(InvestigationStoreError::Invalid)?;
                if response.step_id != pending.step_id
                    || !hash_valid(&response.response_hash)
                    || !hash_valid(&response.action_hash)
                    || response
                        .observed_model
                        .as_ref()
                        .is_some_and(|v| v.is_empty() || v.len() > 256)
                    || response.active_milliseconds < task.detail.usage.active_milliseconds
                {
                    return Err(InvestigationStoreError::Invalid);
                }
                if let Some(model) = &response.observed_model
                    && redacted_history_text(model, 256)? != *model
                {
                    return Err(InvestigationStoreError::Invalid);
                }
                let invoked: PendingInvestigationStep = read_body(
                    &tx,
                    "investigation_invocations",
                    "step_id",
                    &id,
                    &response.step_id,
                )?
                .ok_or(InvestigationStoreError::Invalid)?;
                if &invoked != pending {
                    return Err(InvestigationStoreError::Invalid);
                }
                let mut usage = task.detail.usage.clone();
                usage.reported_input_tokens = add_reported(
                    usage.reported_input_tokens,
                    response.reported_input_tokens,
                    usage.model_invocations == 1,
                )?;
                usage.reported_output_tokens = add_reported(
                    usage.reported_output_tokens,
                    response.reported_output_tokens,
                    usage.model_invocations == 1,
                )?;
                usage.active_milliseconds = response.active_milliseconds;
                validate_usage(&usage, &task.detail.limits)?;
                if let Some(result) = &result {
                    let ledger =
                        load_ledger(&tx, &task)?.ok_or(InvestigationStoreError::Invalid)?;
                    result.validate(&ledger)?;
                    if result.investigation_id != id
                        || result.observed_response_model != response.observed_model
                    {
                        return Err(InvestigationStoreError::Invalid);
                    }
                    let body = encode(result, MAX_RESULT_BYTES)?;
                    one(tx.execute(
                        "INSERT INTO investigation_results VALUES (?1,?2,?3)",
                        params![id, body, core_prov::content_hash(&body)],
                    )?)?;
                    task.detail.summary.status = InvestigationStatus::Completed;
                    task.detail.summary.has_result = true;
                    task.phase = Phase::Terminal;
                } else {
                    task.phase = Phase::Action;
                }
                insert_body(
                    &tx,
                    "investigation_responses",
                    &id,
                    &response.step_id,
                    &response,
                    4096,
                )?;
                task.detail.usage = usage;
                task.pending = None;
                task.approved = false;
                task.action_tool = tool;
                bump(&tx, &mut task)?;
                event(
                    &tx,
                    &mut task,
                    InvestigationEventKind::ModelCompleted,
                    Some(&response.step_id),
                    tool,
                    "A complete provider response was admitted and its identity persisted.",
                )?;
                event(
                    &tx,
                    &mut task,
                    if result.is_some() {
                        InvestigationEventKind::ResultPersisted
                    } else {
                        InvestigationEventKind::ActionAdmitted
                    },
                    Some(&response.step_id),
                    tool,
                    if result.is_some() {
                        "Cited findings were persisted against the original input ledger."
                    } else {
                        "A bounded tool action was admitted; its tool has not started."
                    },
                )?;
                if result.is_some() {
                    sync_terminal_job(&tx, &task)?;
                }
            }
            InvestigationTransition::BeginTool { tool } => {
                if task.phase != Phase::Action || task.action_tool != Some(tool) {
                    return Err(InvestigationStoreError::Stale);
                }
                available_time(&task)?;
                let mut usage = task.detail.usage.clone();
                usage.tool_actions = usage
                    .tool_actions
                    .checked_add(1)
                    .ok_or(InvestigationStoreError::Capacity)?;
                if tool == InvestigationTool::ReadEvidence {
                    usage.evidence_requests = usage
                        .evidence_requests
                        .checked_add(1)
                        .ok_or(InvestigationStoreError::Capacity)?;
                }
                validate_usage(&usage, &task.detail.limits)?;
                task.detail.usage = usage;
                task.phase = Phase::Tool;
                bump(&tx, &mut task)?;
                event(
                    &tx,
                    &mut task,
                    InvestigationEventKind::ToolStarted,
                    None,
                    Some(tool),
                    "Tool attempt budget reserved before execution.",
                )?;
            }
            InvestigationTransition::ChargeEvidence { validation_bytes } => {
                if task.phase != Phase::Tool
                    || task.action_tool != Some(InvestigationTool::ReadEvidence)
                    || validation_bytes == 0
                    || validation_bytes > 16 * 1024 * 1024
                {
                    return Err(InvestigationStoreError::Stale);
                }
                available_time(&task)?;
                let mut usage = task.detail.usage.clone();
                usage.captured_validation_bytes = usage
                    .captured_validation_bytes
                    .checked_add(validation_bytes)
                    .ok_or(InvestigationStoreError::Capacity)?;
                validate_usage(&usage, &task.detail.limits)?;
                task.detail.usage = usage;
                bump(&tx, &mut task)?;
                event(
                    &tx,
                    &mut task,
                    InvestigationEventKind::EvidenceValidationReserved,
                    None,
                    Some(InvestigationTool::ReadEvidence),
                    "Full captured file validation bytes reserved before reading.",
                )?;
            }
            InvestigationTransition::StopWithElapsed {
                reason,
                active_milliseconds,
            } => {
                if active_milliseconds < task.detail.usage.active_milliseconds {
                    return Err(InvestigationStoreError::Invalid);
                }
                task.detail.usage.active_milliseconds = active_milliseconds;
                stop(&tx, &mut task, reason)?;
            }
        }
        let terminal = task.phase == Phase::Terminal;
        save_task(&tx, &task, expected_revision, terminal)?;
        tx.commit()?;
        Ok(task.detail)
    }

    pub(crate) fn approve_investigation(
        &mut self,
        id: &str,
        revision: u64,
        step_id: &str,
        payload_hash: &str,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        self.consent_decision(id, revision, step_id, payload_hash, true)
    }

    pub(crate) fn decline_investigation(
        &mut self,
        id: &str,
        revision: u64,
        step_id: &str,
        payload_hash: &str,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        self.consent_decision(id, revision, step_id, payload_hash, false)
    }

    fn consent_decision(
        &mut self,
        id: &str,
        revision: u64,
        step_id: &str,
        payload_hash: &str,
        approved: bool,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        let mut task = load_task(&tx, id)?;
        if task.detail.summary.revision != revision
            || task.phase != Phase::Consent
            || task.approved
            || task
                .pending
                .as_ref()
                .is_none_or(|step| step.step_id != step_id || step.payload_hash != payload_hash)
        {
            return Err(InvestigationStoreError::Stale);
        }
        if task.detail.summary.cancel_requested {
            return Err(InvestigationStoreError::Cancelled);
        }
        if approved {
            task.approved = true;
            bump(&tx, &mut task)?;
            event(
                &tx,
                &mut task,
                InvestigationEventKind::ConsentApproved,
                Some(step_id),
                None,
                "Exact step approved; authorization will be consumed at invocation start.",
            )?;
        } else {
            task.detail.summary.cancel_requested = true;
            stop(&tx, &mut task, InvestigationStopReason::ConsentDeclined)?;
        }
        save_task(&tx, &task, revision, task.phase == Phase::Terminal)?;
        tx.commit()?;
        Ok(task.detail)
    }

    pub(crate) fn cancel_investigation(
        &mut self,
        id: &str,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        let mut task = load_task(&tx, id)?;
        if task.phase != Phase::Terminal && !task.detail.summary.cancel_requested {
            let expected = task.detail.summary.revision;
            request_cancel(&tx, &mut task)?;
            save_task(&tx, &task, expected, true)?;
        }
        tx.commit()?;
        Ok(task.detail)
    }

    pub(crate) fn investigation_recovery_candidates(
        &self,
    ) -> Result<Vec<InvestigationRecoveryCandidate>, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let ids=tx.prepare("SELECT CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB))<=128 THEN id END FROM investigation_tasks WHERE live=1 ORDER BY ordinal LIMIT 3")?
            .query_map([],|r|r.get::<_,Option<String>>(0))?.collect::<Result<Vec<_>,_>>()?;
        if ids.len() > MAX_LIVE_TASKS {
            return Err(InvestigationStoreError::Invalid);
        }
        let mut candidates = Vec::new();
        for id in ids {
            let id = id.ok_or(InvestigationStoreError::Invalid)?;
            let task = load_task(&tx, &id)?;
            // No owner was ever recorded for a queued task. A concurrent start
            // may still be moving toward claim, so an absent/new lock cannot
            // prove its process died. Such rows remain explicitly cancellable.
            if task.execution.is_none() {
                continue;
            }
            candidates.push(InvestigationRecoveryCandidate {
                id,
                revision: task.detail.summary.revision,
                identity: task.execution.clone(),
                target: JobLockTarget {
                    namespace: self.namespace.clone(),
                    id: task.detail.summary.job_id,
                    existing: task.execution.is_some(),
                },
            });
        }
        tx.commit()?;
        Ok(candidates)
    }

    pub(crate) fn recover_investigation(
        &mut self,
        candidate: &InvestigationRecoveryCandidate,
        reservation: ExecutionReservation,
    ) -> Result<Option<InvestigationDetail>, InvestigationStoreError> {
        reservation.verify()?;
        if reservation.target.namespace != self.namespace
            || reservation.target.id != candidate.target.id
            || reservation.target.existing != candidate.target.existing
            || candidate.target.namespace != self.namespace
        {
            return Err(InvestigationStoreError::Ownership);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        let mut task = load_task(&tx, &candidate.id)?;
        if task.phase == Phase::Terminal
            || task.detail.summary.revision != candidate.revision
            || task.execution != candidate.identity
        {
            tx.commit()?;
            return Ok(None);
        }
        let expected = task.detail.summary.revision;
        let unknown = task.phase == Phase::Invocation;
        if unknown {
            clear_unmeasured_usage(&mut task);
        }
        task.phase = Phase::Terminal;
        task.pending = None;
        task.approved = false;
        task.action_tool = None;
        task.detail.summary.status = if unknown {
            InvestigationStatus::OutcomeUnknown
        } else {
            InvestigationStatus::Interrupted
        };
        task.detail.error=Some(if unknown{"A started invocation has no durable admitted outcome; it was not replayed."}else{"The execution owner stopped and its transient input was lost; work was not resumed."}.into());
        bump(&tx, &mut task)?;
        event(
            &tx,
            &mut task,
            if unknown {
                InvestigationEventKind::OutcomeUnknown
            } else {
                InvestigationEventKind::Interrupted
            },
            None,
            None,
            if unknown {
                "Owner death confirmed; the started invocation outcome is unknown."
            } else {
                "Owner death confirmed; transient preparation was interrupted."
            },
        )?;
        sync_terminal_job(&tx, &task)?;
        save_task(&tx, &task, expected, true)?;
        tx.commit()?;
        Ok(Some(task.detail))
    }
}

pub(super) fn add_reported(
    previous: Option<u64>,
    current: Option<u64>,
    first: bool,
) -> Result<Option<u64>, InvestigationStoreError> {
    if first {
        return Ok(current);
    }
    match (previous, current) {
        (Some(previous), Some(current)) => Ok(Some(
            previous
                .checked_add(current)
                .ok_or(InvestigationStoreError::Capacity)?,
        )),
        _ => Ok(None),
    }
}

fn check_ready(
    task: &StoredTask,
    step: &PendingInvestigationStep,
) -> Result<(), InvestigationStoreError> {
    validate_step(step)?;
    if task.phase != Phase::Ready || task.ledger_hash.as_ref() != Some(&step.input_ledger_hash) {
        return Err(InvestigationStoreError::Stale);
    }
    available_time(task)
}

fn available_time(task: &StoredTask) -> Result<(), InvestigationStoreError> {
    if task.detail.usage.active_milliseconds >= task.detail.limits.active_seconds * 1000 {
        Err(InvestigationStoreError::Capacity)
    } else {
        Ok(())
    }
}

fn admit_input(
    connection: &Connection,
    task: &mut StoredTask,
    ledger: &InvestigationInputLedger,
    usage: InvestigationUsage,
    first: bool,
) -> Result<(), InvestigationStoreError> {
    ledger.validate()?;
    validate_usage(&usage, &task.detail.limits)?;
    let prior = &task.detail.usage;
    if usage.model_invocations != prior.model_invocations
        || usage.tool_actions != prior.tool_actions
        || usage.evidence_requests != prior.evidence_requests
        || usage.captured_validation_bytes != prior.captured_validation_bytes
        || usage.generated_token_reservations != prior.generated_token_reservations
        || usage.reported_input_tokens != prior.reported_input_tokens
        || usage.reported_output_tokens != prior.reported_output_tokens
        || usage.active_milliseconds < prior.active_milliseconds
        || usage.selected_facts < prior.selected_facts
        || usage.evidence_items < prior.evidence_items
        || usage.evidence_bytes < prior.evidence_bytes
        || usage.selected_facts != ledger.selected_facts.len()
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let evidence = ledger
        .citations
        .iter()
        .filter(|c| !matches!(c.origin, InvestigationEvidenceOrigin::GraphMetadata));
    let mut items = 0usize;
    let mut bytes = 0u64;
    let mut captured = 0u64;
    for citation in evidence {
        items += 1;
        let source = citation
            .source
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?;
        bytes = bytes
            .checked_add(source.byte_end - source.byte_start)
            .ok_or(InvestigationStoreError::Capacity)?;
        if let InvestigationEvidenceOrigin::CapturedPrimarySource { captured: span, .. } =
            &citation.origin
        {
            captured = captured
                .checked_add(span.file.byte_len)
                .ok_or(InvestigationStoreError::Capacity)?;
        }
    }
    if usage.evidence_items != items
        || u64::try_from(usage.evidence_bytes).ok() != Some(bytes)
        || captured > usage.captured_validation_bytes
    {
        return Err(InvestigationStoreError::Invalid);
    }
    if let Some(previous) = load_ledger(connection, task)?
        && (ledger.revision <= previous.revision
            || ledger.graph_snapshot_id != previous.graph_snapshot_id
            || ledger.scope_snapshot_id != previous.scope_snapshot_id
            || ledger.supplied_history_hash != previous.supplied_history_hash
            || ledger.supplied_history_bytes != previous.supplied_history_bytes
            || !previous
                .selected_facts
                .iter()
                .all(|fact| ledger.selected_facts.contains(fact))
            || !previous
                .citations
                .iter()
                .all(|citation| ledger.citations.contains(citation))
            || !ledger.queries.starts_with(&previous.queries)
            || !previous
                .receipt_references
                .iter()
                .all(|reference| ledger.receipt_references.contains(reference)))
    {
        return Err(InvestigationStoreError::Conflict);
    }
    let id = &task.detail.summary.investigation_id;
    insert_body(
        connection,
        "investigation_inputs",
        id,
        &signed(ledger.revision)?,
        ledger,
        MAX_MANIFEST_BYTES,
    )?;
    for reference in &ledger.receipt_references {
        let exists:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM investigation_refs WHERE task_id=?1 AND source_id=?2 AND repo_key=?3 AND receipt_id=?4)",params![id,reference.source_id,reference.repo_key,reference.receipt_id],|r|r.get(0))?;
        if !exists {
            one(connection.execute(
                "INSERT INTO investigation_refs VALUES (?1,?2,?3,?4,?5)",
                params![
                    id,
                    signed(ledger.revision)?,
                    reference.source_id,
                    reference.repo_key,
                    reference.receipt_id
                ],
            )?)?;
        }
    }
    task.ledger_revision = Some(ledger.revision);
    task.ledger_hash = Some(ledger.fingerprint()?);
    task.detail.summary.graph_snapshot_id = Some(ledger.graph_snapshot_id.clone());
    task.detail.scope_snapshot_id = Some(ledger.scope_snapshot_id.clone());
    task.detail.citations = ledger.citations.clone();
    task.detail.usage = usage;
    let tool = task.action_tool.take();
    task.phase = Phase::Ready;
    task.detail.summary.status = InvestigationStatus::Running;
    bump(connection, task)?;
    event(
        connection,
        task,
        if first {
            InvestigationEventKind::ContextPrepared
        } else {
            InvestigationEventKind::ToolCompleted
        },
        None,
        tool,
        if first {
            "Frozen context and original input metadata committed."
        } else {
            "Tool input and its actual receipt references committed before lease release."
        },
    )
}

fn check_identity(
    task: &StoredTask,
    execution: &JobExecution,
) -> Result<(), InvestigationStoreError> {
    let identity = task
        .execution
        .as_ref()
        .ok_or(InvestigationStoreError::Ownership)?;
    if identity.namespace != execution.inner.namespace.value
        || identity.job_id != execution.id()
        || identity.generation != execution.inner.generation
        || identity.owner != execution.inner.owner
    {
        return Err(InvestigationStoreError::Ownership);
    }
    Ok(())
}

fn check_job(
    connection: &Connection,
    execution: &JobExecution,
    allow_cancelled_or_missing: bool,
) -> Result<(), InvestigationStoreError> {
    let row=connection.query_row("SELECT CASE WHEN typeof(j.status)='text' AND length(CAST(j.status AS BLOB))<=32 THEN j.status END,a.generation,CASE WHEN typeof(a.owner)='text' AND length(CAST(a.owner AS BLOB))=32 THEN a.owner END FROM jobs j LEFT JOIN job_attempts a ON a.job_id=j.id WHERE j.id=?1",[execution.id()],|r|Ok((r.get::<_,Option<String>>(0)?,r.get::<_,Option<i64>>(1)?,r.get::<_,Option<String>>(2)?))).optional()?;
    match row {
        None if allow_cancelled_or_missing => Ok(()),
        Some((Some(status), generation, owner))
            if generation == Some(execution.inner.generation)
                && owner.as_deref() == Some(execution.inner.owner.as_str())
                && (status == "running"
                    || (allow_cancelled_or_missing && status == "cancelled")) =>
        {
            Ok(())
        }
        _ => Err(InvestigationStoreError::Ownership),
    }
}

fn stop(
    connection: &Connection,
    task: &mut StoredTask,
    reason: InvestigationStopReason,
) -> Result<(), InvestigationStoreError> {
    if reason == InvestigationStopReason::OutcomeUnknown && task.phase != Phase::Invocation {
        return Err(InvestigationStoreError::Stale);
    }
    if task.phase == Phase::Invocation {
        clear_unmeasured_usage(task);
    }
    let cancelled = task.detail.summary.cancel_requested
        || matches!(
            reason,
            InvestigationStopReason::Cancelled | InvestigationStopReason::ConsentDeclined
        );
    let unknown = reason == InvestigationStopReason::OutcomeUnknown;
    task.phase = Phase::Terminal;
    task.pending = None;
    task.approved = false;
    task.action_tool = None;
    if cancelled {
        task.detail.summary.cancel_requested = true;
    }
    task.detail.summary.status = if unknown {
        InvestigationStatus::OutcomeUnknown
    } else if cancelled {
        InvestigationStatus::Cancelled
    } else {
        InvestigationStatus::Failed
    };
    task.detail.error = Some(reason.message().into());
    bump(connection, task)?;
    event(
        connection,
        task,
        if unknown {
            InvestigationEventKind::OutcomeUnknown
        } else if reason == InvestigationStopReason::ConsentDeclined {
            InvestigationEventKind::ConsentDeclined
        } else if cancelled {
            InvestigationEventKind::Cancelled
        } else {
            InvestigationEventKind::Failed
        },
        None,
        None,
        reason.message(),
    )?;
    sync_terminal_job(connection, task)
}

fn clear_unmeasured_usage(task: &mut StoredTask) {
    // A prior measured response cannot establish the complete total when a
    // later invocation has no admitted usage report, including owner death.
    task.detail.usage.reported_input_tokens = None;
    task.detail.usage.reported_output_tokens = None;
}

fn request_cancel(
    connection: &Connection,
    task: &mut StoredTask,
) -> Result<(), InvestigationStoreError> {
    task.detail.summary.cancel_requested = true;
    if task.phase == Phase::Queued {
        task.phase = Phase::Terminal;
        task.detail.summary.status = InvestigationStatus::Cancelled;
    }
    bump(connection, task)?;
    event(
        connection,
        task,
        InvestigationEventKind::CancelRequested,
        None,
        None,
        "Cancellation requested; outstanding provider work retains its execution lease.",
    )?;
    connection.execute("UPDATE jobs SET status='cancelled',updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id=?1 AND status IN ('queued','running')",[task.detail.summary.job_id])?;
    Ok(())
}

fn sync_terminal_job(
    connection: &Connection,
    task: &StoredTask,
) -> Result<(), InvestigationStoreError> {
    let status = if task.detail.summary.cancel_requested {
        "cancelled"
    } else {
        match task.detail.summary.status {
            InvestigationStatus::Completed => "done",
            InvestigationStatus::Interrupted | InvestigationStatus::OutcomeUnknown => "interrupted",
            _ => "failed",
        }
    };
    connection.execute("UPDATE jobs SET status=?2,progress=CASE WHEN ?2='done' THEN 100.0 ELSE progress END,error=?3,updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id=?1 AND status IN ('queued','running')",params![task.detail.summary.job_id,status,task.detail.error])?;
    Ok(())
}

pub(super) fn task_id_for_job(
    connection: &Connection,
    job_id: i64,
) -> Result<Option<String>, InvestigationStoreError> {
    let id:Option<Option<String>>=connection.query_row("SELECT CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB))<=128 THEN id END FROM investigation_tasks WHERE job_id=?1",[job_id],|r|r.get(0)).optional()?;
    id.map(|id| id.ok_or(InvestigationStoreError::Invalid))
        .transpose()
}

pub(in crate::jobs) fn attach_execution(
    connection: &Connection,
    namespace: &ExecutionNamespace,
    job_id: i64,
    kind: &str,
    generation: i64,
    owner: &str,
) -> Result<(), InvestigationStoreError> {
    let id = task_id_for_job(connection, job_id)?;
    if !kind.starts_with(KIND_PREFIX) && id.is_none() {
        return Ok(());
    }
    validate(connection, namespace)?;
    let id = id.ok_or(InvestigationStoreError::Invalid)?;
    if kind != format!("{KIND_PREFIX}{id}") {
        return Err(InvestigationStoreError::Invalid);
    }
    let mut task = load_task(connection, &id)?;
    if task.phase != Phase::Queued
        || task.execution.is_some()
        || task.detail.summary.cancel_requested
        || generation != 1
    {
        return Err(InvestigationStoreError::Stale);
    }
    let revision = task.detail.summary.revision;
    task.execution = Some(StoredExecution {
        namespace: namespace.value.clone(),
        job_id,
        generation,
        owner: owner.into(),
    });
    task.phase = Phase::Preparing;
    task.detail.summary.status = InvestigationStatus::Preparing;
    bump(connection, &mut task)?;
    event(
        connection,
        &mut task,
        InvestigationEventKind::PreparationStarted,
        None,
        None,
        "Execution ownership attached atomically; preparation started.",
    )?;
    save_task(connection, &task, revision, false)
}

pub(in crate::jobs) fn cancel_for_job(
    connection: &Connection,
    namespace: &ExecutionNamespace,
    job_id: i64,
) -> Result<(), InvestigationStoreError> {
    let Some(id) = task_id_for_job(connection, job_id)? else {
        return Ok(());
    };
    validate(connection, namespace)?;
    let mut task = load_task(connection, &id)?;
    if task.phase != Phase::Terminal && !task.detail.summary.cancel_requested {
        let revision = task.detail.summary.revision;
        request_cancel(connection, &mut task)?;
        save_task(connection, &task, revision, true)?;
    }
    Ok(())
}
