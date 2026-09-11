use super::{HostError, context::bounded_json};
use agents::investigation::*;
use llm::LlmProvider;
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// App-lifetime clients and transient worker payloads. Durable task state and
/// deduplication belong exclusively to JobStore's coordinator tables.
#[derive(Default)]
pub(crate) struct InvestigationRuntime {
    providers: Mutex<BTreeMap<InvestigationProviderMode, Arc<dyn LlmProvider>>>,
    live: Mutex<BTreeMap<String, Arc<LiveTask>>>,
}

impl InvestigationRuntime {
    pub(super) fn provider(
        &self,
        mode: InvestigationProviderMode,
    ) -> Result<Arc<dyn LlmProvider>, HostError> {
        let mut providers = self.providers.lock().map_err(|_| HostError::Operational)?;
        if let Some(provider) = providers.get(&mode) {
            return Ok(provider.clone());
        }
        // Constructors make no HTTP/model calls. Reuse their blocking runtimes
        // for the application lifetime, including after a bounded request ends.
        let provider: Arc<dyn LlmProvider> = Arc::from(
            crate::escalation_provider(match mode {
                InvestigationProviderMode::Local => "local",
                InvestigationProviderMode::Cloud => "cloud",
            })
            .map_err(|_| HostError::Operational)?,
        );
        provider
            .bounded_profile()
            .map_err(|_| HostError::InvalidInput)?;
        providers.insert(mode, provider.clone());
        Ok(provider)
    }

    pub(super) fn descriptor(&self, mode: InvestigationProviderMode) -> InvestigationProvider {
        self.provider(mode)
            .and_then(|p| descriptor(p.as_ref(), mode))
            .unwrap_or_else(|_| InvestigationProvider {
                mode,
                provider_id: match mode {
                    InvestigationProviderMode::Local => "ollama",
                    InvestigationProviderMode::Cloud => "anthropic",
                }
                .into(),
                model: String::new(),
                endpoint: String::new(),
                deployment: None,
                available: false,
                unavailable_reason: Some(
                    "A compatible provider is not configured. No model request has been made."
                        .into(),
                ),
            })
    }

    pub(super) fn insert(&self, id: &str) -> Result<Arc<LiveTask>, HostError> {
        let mut live = self.live.lock().map_err(|_| HostError::Operational)?;
        if live.contains_key(id) || live.len() >= 2 {
            return Err(HostError::Operational);
        }
        let task = Arc::new(LiveTask::default());
        live.insert(id.into(), task.clone());
        Ok(task)
    }

    pub(super) fn get(&self, id: &str) -> Result<Option<Arc<LiveTask>>, HostError> {
        Ok(self
            .live
            .lock()
            .map_err(|_| HostError::Operational)?
            .get(id)
            .cloned())
    }

    pub(super) fn remove(&self, id: &str) {
        if let Ok(mut live) = self.live.lock() {
            live.remove(id);
        }
    }

    pub(super) fn wake(&self, id: &str) {
        if let Ok(Some(task)) = self.get(id) {
            task.wake();
        }
    }
}

pub(super) fn descriptor(
    provider: &dyn LlmProvider,
    mode: InvestigationProviderMode,
) -> Result<InvestigationProvider, HostError> {
    let profile = provider
        .bounded_profile()
        .map_err(|_| HostError::InvalidInput)?;
    if (profile.locality == llm::Locality::Local) != (mode == InvestigationProviderMode::Local) {
        return Err(HostError::InvalidInput);
    }
    let descriptor = InvestigationProvider {
        mode,
        provider_id: profile.provider_id,
        model: profile.requested_model,
        endpoint: profile.endpoint_id,
        deployment: None,
        available: true,
        unavailable_reason: None,
    };
    bounded_json(&descriptor, 8192)?;
    Ok(descriptor)
}

#[derive(Default)]
pub(super) struct LiveTask {
    pub pending: Mutex<Option<InvestigationConsent>>,
    generation: Mutex<u64>,
    changed: Condvar,
}

impl LiveTask {
    pub(super) fn wake(&self) {
        if let Ok(mut generation) = self.generation.lock() {
            *generation = generation.wrapping_add(1);
            self.changed.notify_all();
        }
    }
    /// The durable state is rechecked at most one second later even if a
    /// notification races entry or originated in another app process.
    pub(super) fn wait(&self, remaining: Duration) -> Result<(), HostError> {
        let generation = self.generation.lock().map_err(|_| HostError::Operational)?;
        let _guard = self
            .changed
            .wait_timeout(generation, remaining.min(Duration::from_secs(1)))
            .map_err(|_| HostError::Operational)?;
        Ok(())
    }
}

pub(super) struct RunClock {
    started: Instant,
    consent_waiting: Duration,
}

impl RunClock {
    pub(super) fn new(started: Instant) -> Self {
        Self {
            started,
            consent_waiting: Duration::ZERO,
        }
    }
    pub(super) fn active_milliseconds(&self) -> u64 {
        self.started
            .elapsed()
            .saturating_sub(self.consent_waiting)
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
    pub(super) fn record_wait(&mut self, elapsed: Duration) {
        self.consent_waiting = self.consent_waiting.saturating_add(elapsed);
    }
    pub(super) fn remaining_wall(&self) -> Duration {
        Duration::from_secs(3600).saturating_sub(self.started.elapsed())
    }
    pub(super) fn deadline(&self) -> Result<Instant, HostError> {
        let active = Duration::from_millis(self.active_milliseconds());
        let remaining = Duration::from_secs(600)
            .saturating_sub(active)
            .min(self.remaining_wall());
        if remaining.is_zero() {
            return Err(HostError::LimitExceeded);
        }
        Instant::now()
            .checked_add(remaining)
            .ok_or(HostError::LimitExceeded)
    }
}
