//! The actual bounded specialist loop. Only the owned worker keeps source and
//! provider payloads; every admitted operation crosses the durable coordinator.

use super::{
    AcquisitionSink, HostError,
    context::{FrozenContext, bounded_json},
    runtime::{LiveTask, RunClock, descriptor},
};
use crate::jobs::investigations::{
    InvestigationResponseMeta, InvestigationStopReason, InvestigationStoreError,
    InvestigationTransition, PendingInvestigationStep,
};
use crate::{AppState, job_execution::JobExecution, jobs::ExecutionCheck};
use agents::investigation::*;
use core_prov::content_hash;
use llm::bounded::{
    BoundedCallError, BoundedCompletion, CallDirective, CompletionControl, CompletionLimits,
    FailureCode, InvocationOutcome, PreparedBoundedCompletion,
};
use llm::{ConsentGrant, EgressFirewall, LlmProvider, Locality};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::Manager;

#[derive(Debug)]
enum Failure {
    AlreadyTerminal,
    Host(HostError),
    Store(InvestigationStoreError),
    Stop(InvestigationStopReason),
}

impl From<HostError> for Failure {
    fn from(error: HostError) -> Self {
        Self::Host(error)
    }
}
impl From<InvestigationStoreError> for Failure {
    fn from(error: InvestigationStoreError) -> Self {
        Self::Store(error)
    }
}
impl From<InvestigationError> for Failure {
    fn from(error: InvestigationError) -> Self {
        Self::Host(error.into())
    }
}

impl Failure {
    fn reason(&self) -> Option<InvestigationStopReason> {
        use InvestigationStopReason as Stop;
        Some(match self {
            Self::AlreadyTerminal => return None,
            Self::Stop(reason) => *reason,
            Self::Host(HostError::InputChanged) => Stop::InputChanged,
            Self::Host(HostError::SourceUnavailable) => Stop::SourceUnavailable,
            Self::Host(HostError::LimitExceeded)
            | Self::Store(InvestigationStoreError::Capacity) => Stop::LimitExceeded,
            Self::Store(InvestigationStoreError::Cancelled) => Stop::Cancelled,
            Self::Host(HostError::InvalidInput) => Stop::PreparationFailed,
            Self::Host(HostError::Operational) | Self::Store(_) => Stop::Operational,
        })
    }
}

/// Called inside a blocking host worker. The execution's final Arc holder cannot
/// leave while a provider call is outstanding, even if its async waiter is lost.
pub(super) fn run<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    id: String,
    request: StartInvestigationRequest,
    execution: JobExecution,
    provider: Arc<dyn LlmProvider>,
    live: Arc<LiveTask>,
    started: Instant,
) {
    let state = app.state::<AppState>();
    let _cleanup = Cleanup {
        state: &state,
        id: &id,
        live: &live,
    };
    let mut clock = RunClock::new(started);
    let outcome = (|| {
        let detail = read_detail(&state, &id)?;
        let mut worker = Worker {
            app: &app,
            state: &state,
            id: &id,
            execution: &execution,
            live: &live,
            detail,
            clock: &mut clock,
        };
        worker.execute(&request, provider.as_ref())
    })();
    if let Err(failure) = outcome
        && let Some(reason) = failure.reason()
    {
        // No source/provider retry. If even terminal publication fails, the
        // durable pending phase remains available to exact-owner recovery.
        stop(
            &app,
            &state,
            &id,
            &execution,
            reason,
            clock.active_milliseconds(),
        );
    }
}

struct Cleanup<'a> {
    state: &'a AppState,
    id: &'a str,
    live: &'a LiveTask,
}
impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.live.pending.lock() {
            *pending = None;
        }
        self.state.investigations.remove(self.id);
    }
}

struct Worker<'a, R: tauri::Runtime> {
    app: &'a tauri::AppHandle<R>,
    state: &'a AppState,
    id: &'a str,
    execution: &'a JobExecution,
    live: &'a LiveTask,
    detail: InvestigationDetail,
    clock: &'a mut RunClock,
}

impl<R: tauri::Runtime> Worker<'_, R> {
    fn execute(
        &mut self,
        request: &StartInvestigationRequest,
        provider: &dyn LlmProvider,
    ) -> Result<(), Failure> {
        self.check_new_work()?;
        let history = self.parent_history(request)?;
        let mut context = FrozenContext::prepare(self.state, request, &history)?;
        self.check_new_work()?;
        let mut usage = self.detail.usage.clone();
        usage.active_milliseconds = self.clock.active_milliseconds();
        self.transition(InvestigationTransition::InputPrepared {
            ledger: Box::new(context.ledger.clone()),
            usage,
        })?;

        loop {
            self.check_new_work()?;
            if self.detail.usage.model_invocations >= self.detail.limits.model_invocations {
                return Err(Failure::Stop(InvestigationStopReason::LimitExceeded));
            }
            if descriptor(provider, request.provider_mode)? != self.detail.provider {
                return Err(Failure::Stop(InvestigationStopReason::ProviderFailure));
            }
            let input = InvestigationInput::prepare(
                &request.question,
                request.specialist_id,
                context.ledger.clone(),
                &context.query_pages,
                &context.evidence,
                &history,
                &self.detail.usage,
            )?;
            let step_id = format!("step-{}", self.detail.usage.model_invocations + 1);
            let action = input.action(format!("{}:{step_id}", self.id))?;
            let input_bytes = bounded_json(&action.payload, 128 * 1024)?.len();
            let limits = CompletionLimits::default();
            let prepared = firewall(self.state)?
                .prepare_bounded(provider, &action, &limits)
                .map_err(provider_failure)?;
            let step = PendingInvestigationStep {
                step_id: step_id.clone(),
                payload_hash: prepared.preview().egress.payload_hash.clone(),
                input_ledger_hash: context.ledger.fingerprint()?,
                input_bytes,
                generated_tokens: limits.values().max_output_tokens,
            };
            self.check_new_work()?;
            let consent = if provider.locality() == Locality::Cloud {
                Some(self.await_consent(&step, &prepared)?)
            } else {
                None
            };
            self.check_new_work()?;
            // Policy is read again after human waiting. The in-memory grant is
            // constructed only after the durable exact-step approval is observed.
            let authorized = firewall(self.state)?
                .authorize_bounded(provider, &prepared, consent.as_ref())
                .map_err(provider_failure)?;
            self.check_new_work()?;
            self.transition(InvestigationTransition::BeginInvocation { step })?;
            self.clear_pending()?;
            let deadline = self.clock.deadline()?;
            let directive = || execution_directive(self.state, self.execution, self.id);
            let completion = provider
                .complete_bounded(
                    &authorized,
                    &CompletionControl {
                        deadline,
                        check: &directive,
                    },
                )
                .map_err(provider_failure)?;
            // Complete response bytes are still untrusted. A custom bounded
            // provider does not bypass output or immutable model identity checks.
            validate_completion(&completion, &prepared)?;
            if provider.locality() == Locality::Cloud {
                // This is the bounded provider's observed request-body count,
                // not an invented byte estimate for failed or unknown calls.
                let provider_label = format!(
                    "{}:{}",
                    self.detail.provider.provider_id, self.detail.provider.model
                );
                self.state
                    .settings
                    .lock()
                    .map_err(|_| HostError::Operational)?
                    .record_egress(&provider_label, completion.request_bytes)
                    .map_err(|_| HostError::Operational)?;
            }
            input
                .validate_response_metadata(completion.response_model.as_deref())
                .map_err(|_| Failure::Stop(InvestigationStopReason::InvalidResponse))?;
            let action = InvestigationAction::decode(&completion.text)
                .map_err(|_| Failure::Stop(InvestigationStopReason::InvalidResponse))?;
            let response = InvestigationResponseMeta {
                step_id,
                response_hash: content_hash(completion.text.as_bytes()),
                action_hash: content_hash(&bounded_json(&action, 32 * 1024)?),
                observed_model: completion.response_model.clone(),
                reported_input_tokens: completion
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.input_tokens),
                reported_output_tokens: completion
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.output_tokens),
                active_milliseconds: self.clock.active_milliseconds(),
            };
            if matches!(action, InvestigationAction::Finish { .. }) {
                let timestamp = self
                    .state
                    .jobs
                    .lock()
                    .map_err(|_| HostError::Operational)?
                    .investigation_timestamp(0)?;
                let result = input
                    .admit_result(self.id, &action, completion.response_model, timestamp)
                    .map_err(|_| Failure::Stop(InvestigationStopReason::InvalidResponse))?;
                // A valid finish belongs to the last supplied ledger, even if
                // cancellation/Jobs cleanup won while the response was in flight.
                self.admit_finish(response, result)?;
                return Ok(());
            }
            // Late tools do not run or acquire new source after cancellation.
            self.check_new_work()?;
            let tool = action
                .tool()
                .ok_or(Failure::Stop(InvestigationStopReason::InvalidResponse))?;
            self.transition(InvestigationTransition::ActionAdmitted {
                response,
                tool: Some(tool),
                result: None,
            })?;
            self.check_new_work()?;
            self.transition(InvestigationTransition::BeginTool { tool })?;
            let state = self.state;
            let mut sink = WorkerSink { worker: self };
            match action {
                InvestigationAction::QueryContext { query } => {
                    context.query(state, query, &mut sink)?
                }
                InvestigationAction::ReadEvidence { fact, role, index } => {
                    context.read_evidence(state, fact, role, index, &mut sink)?
                }
                InvestigationAction::Finish { .. } => unreachable!(),
            }
            self.check_new_work()?;
        }
    }

    fn check_new_work(&mut self) -> Result<(), Failure> {
        self.clock.deadline()?;
        let jobs = self.state.jobs.lock().map_err(|_| HostError::Operational)?;
        let detail = jobs.investigation(self.id)?;
        if detail.summary.job_id != self.execution.id() {
            return Err(Failure::Stop(InvestigationStopReason::Operational));
        }
        if terminal(detail.summary.status) {
            return Err(Failure::AlreadyTerminal);
        }
        let checked = jobs.check_execution(self.execution);
        if detail.summary.cancel_requested {
            self.detail = detail;
            return match checked {
                Ok(ExecutionCheck::Cancelled | ExecutionCheck::Running)
                | Err(crate::jobs::JobTransitionError::Missing) => {
                    Err(Failure::Stop(InvestigationStopReason::Cancelled))
                }
                _ => Err(Failure::Stop(InvestigationStopReason::Operational)),
            };
        }
        if !matches!(checked, Ok(ExecutionCheck::Running)) {
            return Err(Failure::Stop(InvestigationStopReason::Operational));
        }
        self.detail = detail;
        Ok(())
    }

    fn apply(&mut self, transition: InvestigationTransition) -> Result<(), Failure> {
        self.detail = self
            .state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational)?
            .advance_investigation(self.execution, self.detail.summary.revision, transition)?;
        Ok(())
    }

    fn transition(&mut self, transition: InvestigationTransition) -> Result<(), Failure> {
        self.apply(transition)?;
        super::emit_changed(self.app, &self.detail);
        Ok(())
    }

    fn clear_pending(&self) -> Result<(), Failure> {
        *self
            .live
            .pending
            .lock()
            .map_err(|_| HostError::Operational)? = None;
        Ok(())
    }

    fn parent_history(&self, request: &StartInvestigationRequest) -> Result<Vec<String>, Failure> {
        let Some(parent_id) = &request.parent_id else {
            return Ok(Vec::new());
        };
        let jobs = self.state.jobs.lock().map_err(|_| HostError::Operational)?;
        let parent = jobs.investigation(parent_id)?;
        if request.conversation_id.as_ref() != Some(&parent.summary.conversation_id) {
            return Err(Failure::Stop(InvestigationStopReason::PreparationFailed));
        }
        let result = jobs.investigation_result(parent_id)?;
        // No recursive conversation replay or current-source reconstruction.
        // The selected parent's complete saved representation must fit as a
        // whole; excess history fails explicitly rather than silently truncating.
        let raw = bounded_json(
            &serde_json::json!({
                "parent_investigation_id": parent_id,
                "original_graph_snapshot_id": parent.summary.graph_snapshot_id,
                "original_scope": parent.summary.scope,
                "saved_execution_status": parent.summary.status,
                "cancellation_requested": parent.summary.cancel_requested,
                "invocation_outcome_pending": parent.summary.invocation_pending,
                "authority": "Prior specialist findings remain T3/InferredWeak; execution status does not establish complete knowledge.",
                "saved_result": result,
            }),
            16 * 1024,
        )?;
        let history = vec![String::from_utf8(raw).map_err(|_| HostError::InvalidInput)?];
        bounded_json(&history, 16 * 1024)?;
        Ok(history)
    }

    fn await_consent(
        &mut self,
        step: &PendingInvestigationStep,
        prepared: &PreparedBoundedCompletion,
    ) -> Result<ConsentGrant, Failure> {
        let remaining = self.clock.remaining_wall().min(Duration::from_secs(900));
        if remaining.is_zero() {
            return Err(Failure::Stop(InvestigationStopReason::ConsentExpired));
        }
        let expires_at = self
            .state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational)?
            .investigation_timestamp(
                remaining
                    .as_secs()
                    .try_into()
                    .map_err(|_| HostError::InvalidInput)?,
            )?;
        self.apply(InvestigationTransition::AwaitConsent { step: step.clone() })?;
        *self
            .live
            .pending
            .lock()
            .map_err(|_| HostError::Operational)? = Some(InvestigationConsent {
            investigation_id: self.id.into(),
            step_id: step.step_id.clone(),
            revision: self.detail.summary.revision,
            preview: prepared.preview().egress.clone(),
            provider_profile: prepared.profile().clone(),
            completion_limits: *prepared.limits(),
            provider: self.detail.provider.clone(),
            expires_at,
        });
        // Publish the invalidation only after the exact in-memory preview exists.
        super::emit_changed(self.app, &self.detail);
        let waiting = Instant::now();
        let outcome = (|| loop {
            if waiting.elapsed() >= remaining || self.clock.remaining_wall().is_zero() {
                return Err(Failure::Stop(InvestigationStopReason::ConsentExpired));
            }
            let observed = {
                let jobs = self.state.jobs.lock().map_err(|_| HostError::Operational)?;
                let detail = jobs.investigation(self.id)?;
                if terminal(detail.summary.status) {
                    return Err(Failure::AlreadyTerminal);
                }
                if detail.summary.cancel_requested {
                    return Err(Failure::Stop(InvestigationStopReason::Cancelled));
                }
                if !matches!(
                    jobs.check_execution(self.execution),
                    Ok(ExecutionCheck::Running)
                ) {
                    return Err(Failure::Stop(InvestigationStopReason::Operational));
                }
                jobs.investigation_pending_step(self.id)?
            };
            let Some((revision, observed_step, approved)) = observed else {
                return Err(Failure::Stop(InvestigationStopReason::Operational));
            };
            if observed_step != *step {
                return Err(Failure::Stop(InvestigationStopReason::Operational));
            }
            if approved {
                self.detail.summary.revision = revision;
                return Ok(ConsentGrant::from_preview(&prepared.preview().egress));
            }
            self.live
                .wait(remaining.saturating_sub(waiting.elapsed()))?;
        })();
        self.clock.record_wait(waiting.elapsed());
        if outcome.is_err() {
            self.clear_pending()?;
        }
        outcome
    }

    fn admit_finish(
        &mut self,
        response: InvestigationResponseMeta,
        result: InvestigationResult,
    ) -> Result<(), Failure> {
        for _ in 0..3 {
            let updated = {
                let mut jobs = self.state.jobs.lock().map_err(|_| HostError::Operational)?;
                let current = jobs.investigation(self.id)?;
                jobs.advance_investigation(
                    self.execution,
                    current.summary.revision,
                    InvestigationTransition::ActionAdmitted {
                        response: response.clone(),
                        tool: None,
                        result: Some(Box::new(result.clone())),
                    },
                )
            };
            match updated {
                Ok(detail) => {
                    self.detail = detail;
                    super::emit_changed(self.app, &self.detail);
                    return Ok(());
                }
                Err(InvestigationStoreError::Stale) => continue,
                // A validated response that could not be durably admitted must
                // not be acknowledged; preserve the uncertainty of its old phase.
                Err(_) => return Err(Failure::Stop(InvestigationStopReason::OutcomeUnknown)),
            }
        }
        Err(Failure::Stop(InvestigationStopReason::OutcomeUnknown))
    }
}

struct WorkerSink<'a, 'worker, R: tauri::Runtime> {
    worker: &'a mut Worker<'worker, R>,
}
impl<R: tauri::Runtime> AcquisitionSink for WorkerSink<'_, '_, R> {
    fn usage(&self) -> &InvestigationUsage {
        &self.worker.detail.usage
    }
    fn reserve_validation(&mut self, bytes: u64) -> Result<(), HostError> {
        self.worker.check_new_work().map_err(acquisition_error)?;
        self.worker
            .transition(InvestigationTransition::ChargeEvidence {
                validation_bytes: bytes,
            })
            .map_err(acquisition_error)
    }
    fn publish_input(
        &mut self,
        ledger: &InvestigationInputLedger,
        mut usage: InvestigationUsage,
    ) -> Result<(), HostError> {
        self.worker.check_new_work().map_err(acquisition_error)?;
        usage.active_milliseconds = self.worker.clock.active_milliseconds();
        self.worker
            .transition(InvestigationTransition::InputAdmitted {
                ledger: Box::new(ledger.clone()),
                usage,
            })
            .map_err(acquisition_error)
    }
}

fn acquisition_error(error: Failure) -> HostError {
    match error {
        Failure::Host(error) => error,
        Failure::Store(InvestigationStoreError::Capacity)
        | Failure::Stop(InvestigationStopReason::LimitExceeded) => HostError::LimitExceeded,
        _ => HostError::Operational,
    }
}
fn firewall(state: &AppState) -> Result<EgressFirewall, HostError> {
    Ok(EgressFirewall::new(
        state
            .settings
            .lock()
            .map_err(|_| HostError::Operational)?
            .egress_policy()
            .map_err(|_| HostError::Operational)?,
    ))
}
fn read_detail(state: &AppState, id: &str) -> Result<InvestigationDetail, Failure> {
    Ok(state
        .jobs
        .lock()
        .map_err(|_| HostError::Operational)?
        .investigation(id)?)
}
fn terminal(status: InvestigationStatus) -> bool {
    matches!(
        status,
        InvestigationStatus::Completed
            | InvestigationStatus::Failed
            | InvestigationStatus::Cancelled
            | InvestigationStatus::Interrupted
            | InvestigationStatus::OutcomeUnknown
    )
}
fn execution_directive(state: &AppState, execution: &JobExecution, id: &str) -> CallDirective {
    let Ok(jobs) = state.jobs.lock() else {
        return CallDirective::OwnershipLost;
    };
    let Ok(detail) = jobs.investigation(id) else {
        return CallDirective::OwnershipLost;
    };
    if detail.summary.job_id != execution.id() {
        return CallDirective::OwnershipLost;
    }
    match jobs.check_execution(execution) {
        Ok(ExecutionCheck::Running)
            if !detail.summary.cancel_requested && !terminal(detail.summary.status) =>
        {
            CallDirective::Continue
        }
        Ok(ExecutionCheck::Cancelled) => CallDirective::Cancelled,
        _ => CallDirective::OwnershipLost,
    }
}
fn provider_failure(error: BoundedCallError) -> Failure {
    Failure::Stop(if error.outcome == InvocationOutcome::Unknown {
        InvestigationStopReason::OutcomeUnknown
    } else {
        match error.code {
            FailureCode::Cancelled => InvestigationStopReason::Cancelled,
            FailureCode::OwnershipLost => InvestigationStopReason::Operational,
            FailureCode::DeadlineExceeded
            | FailureCode::InputTooLarge
            | FailureCode::RequestTooLarge
            | FailureCode::ResponseTooLarge
            | FailureCode::OutputTooLarge => InvestigationStopReason::LimitExceeded,
            _ => InvestigationStopReason::ProviderFailure,
        }
    })
}
fn validate_completion(
    completion: &BoundedCompletion,
    prepared: &PreparedBoundedCompletion,
) -> Result<(), Failure> {
    let limits = prepared.limits().values();
    if completion.text.is_empty()
        || completion.text.len() as u64 > limits.output_text_bytes
        || completion.requested_model != prepared.profile().requested_model
        || completion.request_bytes > limits.request_bytes
        || completion.response_bytes > limits.response_bytes
    {
        return Err(Failure::Stop(InvestigationStopReason::InvalidResponse));
    }
    Ok(())
}
fn stop<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    id: &str,
    execution: &JobExecution,
    reason: InvestigationStopReason,
    active_milliseconds: u64,
) {
    for _ in 0..3 {
        let attempt = (|| {
            let mut jobs = state
                .jobs
                .lock()
                .map_err(|_| InvestigationStoreError::Storage)?;
            let current = jobs.investigation(id)?;
            if terminal(current.summary.status) {
                return Ok(None);
            }
            jobs.advance_investigation(
                execution,
                current.summary.revision,
                InvestigationTransition::StopWithElapsed {
                    reason,
                    active_milliseconds: active_milliseconds.max(current.usage.active_milliseconds),
                },
            )
            .map(Some)
        })();
        match attempt {
            Ok(Some(detail)) => {
                super::emit_changed(app, &detail);
                return;
            }
            Err(InvestigationStoreError::Stale) => continue,
            _ => return,
        }
    }
}

#[cfg(test)]
#[path = "worker/tests.rs"]
mod tests;
