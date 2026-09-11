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
mod tests {
    use super::*;
    use adapters_lang_ts::captured;
    use core_graph::GraphStore;
    use core_prov::{ConfidenceTier, Tier};
    use llm::bounded::{AuthorizedBoundedCompletion, ProviderProfile, ReportedUsage};
    use llm::{Embedding, ProviderCaps, ProviderError};
    use serde_json::{Value, json};
    use std::path::PathBuf;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    const ORIGINAL: &str = "// private trailing-source marker is never output\nexport function enabled(ok: boolean) { const limited = ok === false; if (limited) return false; }\n";

    #[derive(Clone, Copy)]
    enum Mode {
        Finish,
        CancelFinish,
        CancelTool,
        Malformed,
        Unknown,
    }

    /// One deterministic provider action per actual host invocation. The fixture
    /// learns exact fact/citation/range IDs only from its admitted prompt; no
    /// independent expected answer is placed in that prompt or target source.
    struct Scripted {
        calls: AtomicUsize,
        mode: Mode,
        cancel: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
        copied: Mutex<Vec<String>>,
    }
    impl Scripted {
        fn new(mode: Mode) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                mode,
                cancel: Mutex::new(None),
                copied: Mutex::new(vec![]),
            }
        }
    }
    impl LlmProvider for Scripted {
        fn id(&self) -> &str {
            "local-scripted"
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
            Err(ProviderError::Unsupported("fixture embeddings"))
        }
        fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
            Ok(ProviderProfile {
                protocol_version: llm::bounded::PROTOCOL_VERSION.into(),
                provider_id: self.id().into(),
                locality: Locality::Local,
                endpoint_id: "http://127.0.0.1:11434/api/chat".into(),
                requested_model: "scripted-model".into(),
            })
        }
        fn complete_bounded(
            &self,
            request: &AuthorizedBoundedCompletion,
            control: &CompletionControl<'_>,
        ) -> Result<BoundedCompletion, BoundedCallError> {
            assert_eq!((control.check)(), CallDirective::Continue);
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if matches!(self.mode, Mode::Unknown) {
                return Err(BoundedCallError {
                    code: FailureCode::Transport,
                    outcome: InvocationOutcome::Unknown,
                });
            }
            let prompt: Value = serde_json::from_str(&request.payload().prompt).unwrap();
            let action = if call == 0 {
                json!({ "type": "query_context", "query": { "scope": {"type":"all"}, "kind":"node",
                    "labels":["BusinessRule"], "max_facts":1, "max_bytes":32768, "cursor":null } })
            } else if call == 1 {
                let option = &prompt["context_pages"][0]["evidence_options"][0];
                let range = option["ranges"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|range| range["role"] == "definition_initializer")
                    .unwrap();
                json!({ "type":"read_evidence", "fact":option["fact"], "role":range["role"], "index":range["index"] })
            } else {
                assert_eq!(call, 2, "no hidden repair, retry or extra model step");
                let citations = prompt["input_ledger"]["citations"].as_array().unwrap();
                let citation = citations
                    .iter()
                    .find(|citation| citation["origin"]["kind"] == "captured_primary_source")
                    .unwrap();
                let span = request
                    .payload()
                    .spans
                    .iter()
                    .find(|span| span.id == citation["citation_id"])
                    .unwrap();
                self.copied.lock().unwrap().push(span.text.clone());
                json!({ "type":"finish", "findings":[{ "claim_kind":"inferred_interpretation",
                    "title":"A guarded path was observed", "statement":"The inspected callable can exit conditionally; external policy remains unresolved.",
                    "citation_ids":[citation["citation_id"]], "limitations":["Runtime inputs and broader feature coverage remain unknown."] }],
                    "knowledge_completeness":"partial", "limitations":["This scoped result establishes no complete business policy."] })
            };
            if (matches!(self.mode, Mode::CancelFinish) && call == 2)
                || (matches!(self.mode, Mode::CancelTool) && call == 1)
            {
                self.cancel.lock().unwrap().as_ref().unwrap()();
                // The response was already in flight: emulate its valid return
                // after cancellation instead of fabricating remote termination.
            }
            let text = if matches!(self.mode, Mode::Malformed) {
                "```json\n{}\n```".into()
            } else {
                serde_json::to_string(&action).unwrap()
            };
            Ok(BoundedCompletion {
                text,
                requested_model: "scripted-model".into(),
                response_model: Some("scripted-model-observed".into()),
                provider_request_id: None,
                usage: Some(ReportedUsage {
                    input_tokens: Some(100),
                    output_tokens: Some(40),
                    cache_read_input_tokens: None,
                    cache_creation_input_tokens: None,
                }),
                request_bytes: 100,
                response_bytes: 100,
            })
        }
    }

    struct Fixture {
        _directory: tempfile::TempDir,
        app_data: PathBuf,
        app: tauri::App<tauri::test::MockRuntime>,
        request: StartInvestigationRequest,
        id: String,
        execution: JobExecution,
        live: Arc<LiveTask>,
        graph_before: String,
    }

    impl Fixture {
        fn new(provider: &dyn LlmProvider) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let app_data = directory.path().join("private");
            std::fs::create_dir(&app_data).unwrap();
            let app_data = crate::paths::canonicalize(app_data).unwrap();
            let target = directory.path().join("target");
            std::fs::create_dir(&target).unwrap();
            std::fs::write(target.join("rule.ts"), ORIGINAL).unwrap();
            let state = crate::registered_source_tests::app_state(&app_data);
            let source = state
                .sources
                .lock()
                .unwrap()
                .register_local(&target)
                .unwrap();
            let input = state
                .primary_sources
                .prepare(&source, source.root(), &["client".into()])
                .unwrap();
            let capture = input.capture.as_ref().unwrap();
            let (extraction, receipts) = captured::extract_file(
                capture.file("rule.ts").unwrap(),
                &adapters_lang_ts::SourceId {
                    repo: &source.repo_key,
                    commit: "workdir",
                },
            )
            .unwrap();
            state
                .primary_sources
                .persist(&input, &source, &receipts)
                .unwrap();
            let bindings = crate::primary_source::matching_bindings(&extraction, &receipts);
            assert!(!bindings.is_empty());
            crate::load_into_graph_with_bindings(
                &mut state.graph.lock().unwrap(),
                &extraction,
                &source.repo_key,
                source.root(),
                "workdir",
                &bindings,
            )
            .unwrap();
            drop(input);
            // Every later read must use retained parser bytes, not this checkout.
            std::fs::remove_dir_all(target).unwrap();
            let graph_before =
                serde_json::to_string(&state.graph.lock().unwrap().read_snapshot().unwrap())
                    .unwrap();
            let request = StartInvestigationRequest {
                schema_version: 1,
                request_nonce: "worker-fixture".into(),
                specialist_id: SpecialistId::DomainAnalyst,
                question: "Inspect the available guard evidence and its limits.".into(),
                scope: InvestigationScope::All,
                provider_mode: InvestigationProviderMode::Local,
                limit_profile: "investigation-v1".into(),
                expected_graph_revision: None,
                conversation_id: None,
                parent_id: None,
            };
            let started = state
                .jobs
                .lock()
                .unwrap()
                .start_investigation(
                    &request,
                    Some(&descriptor(provider, InvestigationProviderMode::Local).unwrap()),
                )
                .unwrap();
            let id = started.detail.summary.investigation_id;
            let plan = state
                .jobs
                .lock()
                .unwrap()
                .claim_plan(
                    started.detail.summary.job_id,
                    crate::jobs::ClaimMode::StartQueued,
                )
                .unwrap();
            let reservation = state
                .job_execution_locks
                .try_reserve(plan.lock_target())
                .unwrap();
            let (_, execution) = state
                .jobs
                .lock()
                .unwrap()
                .claim_execution(&plan, reservation)
                .unwrap();
            let live = state.investigations.insert(&id).unwrap();
            let app = tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap();
            app.manage(state);
            Self {
                _directory: directory,
                app_data,
                app,
                request,
                id,
                execution,
                live,
                graph_before,
            }
        }

        fn run(&self, provider: Arc<dyn LlmProvider>) {
            run(
                self.app.handle().clone(),
                self.id.clone(),
                self.request.clone(),
                self.execution.clone(),
                provider,
                self.live.clone(),
                Instant::now(),
            );
        }

        fn cancel_inside_call(&self, provider: &Scripted, clear_job: bool) {
            let app = self.app.handle().clone();
            let id = self.id.clone();
            *provider.cancel.lock().unwrap() = Some(Box::new(move || {
                let state = app.state::<AppState>();
                let mut jobs = state.jobs.lock().unwrap();
                assert!(jobs.investigation(&id).unwrap().summary.invocation_pending);
                jobs.cancel_investigation(&id).unwrap();
                if clear_job {
                    jobs.clear_finished().unwrap();
                }
            }));
        }
    }

    #[test]
    fn investigation_worker_queries_reads_captured_definition_and_persists_cited_finish() {
        // AC-0182/0183/0184/0188/0191: actual parser receipts, graph selection,
        // source retention, provider input and durable coordinator loop together.
        let provider = Arc::new(Scripted::new(Mode::Finish));
        let fixture = Fixture::new(provider.as_ref());
        fixture.run(provider.clone());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
        assert_eq!(*provider.copied.lock().unwrap(), vec!["ok === false"]);
        let state = fixture.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&fixture.id).unwrap();
        assert_eq!(detail.summary.status, InvestigationStatus::Completed);
        assert_eq!(detail.usage.model_invocations, 3);
        assert_eq!(detail.usage.tool_actions, 2);
        assert_eq!(detail.usage.evidence_requests, 1);
        assert_eq!(
            detail.usage.captured_validation_bytes,
            ORIGINAL.len() as u64
        );
        assert_eq!(detail.usage.reported_input_tokens, Some(300));
        let result = jobs.investigation_result(&fixture.id).unwrap().unwrap();
        assert_eq!(result.findings[0].tier, Tier::Agentic);
        assert_eq!(
            result.findings[0].confidence_tier,
            ConfidenceTier::InferredWeak
        );
        let ledger = jobs.investigation_ledger(&fixture.id).unwrap().unwrap();
        let citation = ledger
            .citations
            .iter()
            .find(|citation| citation.citation_id == result.findings[0].citation_ids[0])
            .unwrap();
        assert_eq!(
            citation.role,
            Some(agents::TaskRangeRole::DefinitionInitializer)
        );
        assert!(matches!(
            citation.origin,
            InvestigationEvidenceOrigin::CapturedPrimarySource { .. }
        ));
        let events = jobs.investigation_events(&fixture.id, 0).unwrap().items;
        let reserve = events
            .iter()
            .position(|event| event.kind == InvestigationEventKind::EvidenceValidationReserved)
            .unwrap();
        let read_done = events
            .iter()
            .position(|event| {
                event.kind == InvestigationEventKind::ToolCompleted
                    && event.tool == Some(InvestigationTool::ReadEvidence)
            })
            .unwrap();
        assert!(reserve < read_done);
        assert!(
            events
                .windows(2)
                .all(|pair| pair[0].sequence + 1 == pair[1].sequence)
        );
        drop(jobs);
        assert_eq!(
            serde_json::to_string(&state.graph.lock().unwrap().read_snapshot().unwrap()).unwrap(),
            fixture.graph_before
        );
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("ok === false")
        );
        let reopened = crate::jobs::JobStore::open(fixture.app_data.join("state.db")).unwrap();
        assert_eq!(
            reopened.investigation_result(&fixture.id).unwrap(),
            Some(result)
        );
        assert!(state.investigations.get(&fixture.id).unwrap().is_none());
    }

    #[test]
    fn investigation_worker_retains_late_finish_after_cancellation_and_job_cleanup() {
        // AC-0187/0191: the actual call returns after cancellation and deletion;
        // only its finish is admitted, against its unchanged original ledger.
        let provider = Arc::new(Scripted::new(Mode::CancelFinish));
        let fixture = Fixture::new(provider.as_ref());
        fixture.cancel_inside_call(provider.as_ref(), true);
        fixture.run(provider.clone());
        let state = fixture.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&fixture.id).unwrap();
        assert!(detail.summary.cancel_requested);
        assert!(detail.summary.has_result);
        assert!(!detail.summary.invocation_pending);
        assert!(jobs.get(detail.summary.job_id).is_err());
        assert!(jobs.investigation_result(&fixture.id).unwrap().is_some());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
        assert_eq!(detail.usage.tool_actions, 2);
    }

    #[test]
    fn investigation_worker_never_executes_a_late_tool_after_cancellation() {
        // AC-0187: a tool returned by an already-issued call is not new permission.
        let provider = Arc::new(Scripted::new(Mode::CancelTool));
        let fixture = Fixture::new(provider.as_ref());
        fixture.cancel_inside_call(provider.as_ref(), false);
        fixture.run(provider.clone());
        let state = fixture.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&fixture.id).unwrap();
        assert_eq!(detail.summary.status, InvestigationStatus::Cancelled);
        assert_eq!(detail.usage.tool_actions, 1);
        assert_eq!(detail.usage.evidence_requests, 0);
        assert_eq!(detail.usage.evidence_items, 0);
        assert!(!detail.summary.has_result);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        assert!(provider.copied.lock().unwrap().is_empty());
    }

    #[test]
    fn investigation_worker_rejects_malformed_actions_without_model_repair() {
        // AC-0184/0185: invalid complete output does not buy another invocation.
        let provider = Arc::new(Scripted::new(Mode::Malformed));
        let fixture = Fixture::new(provider.as_ref());
        fixture.run(provider.clone());
        let state = fixture.app.state::<AppState>();
        let detail = state
            .jobs
            .lock()
            .unwrap()
            .investigation(&fixture.id)
            .unwrap();
        assert_eq!(detail.summary.status, InvestigationStatus::Failed);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(detail.usage.tool_actions, 0);
        assert!(!detail.summary.has_result);
    }

    #[test]
    fn investigation_worker_records_unknown_transport_outcome_without_replay() {
        // AC-0185/0187: reservations survive unknown transport, with absent usage.
        let provider = Arc::new(Scripted::new(Mode::Unknown));
        let fixture = Fixture::new(provider.as_ref());
        fixture.run(provider.clone());
        let state = fixture.app.state::<AppState>();
        let detail = state
            .jobs
            .lock()
            .unwrap()
            .investigation(&fixture.id)
            .unwrap();
        assert_eq!(detail.summary.status, InvestigationStatus::OutcomeUnknown);
        assert_eq!(detail.usage.model_invocations, 1);
        assert_eq!(detail.usage.generated_token_reservations, 2048);
        assert_eq!(detail.usage.reported_input_tokens, None);
        assert!(!detail.summary.has_result);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }
}
