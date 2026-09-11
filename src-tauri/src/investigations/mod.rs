//! Durable specialist investigations (SPEC-10). The coordinator owns execution;
//! graph/source acquisition is short-lived and model payloads remain transient.
pub(crate) mod commands;
mod context;
mod events;
mod evidence;
mod history;
mod recovery;
mod runtime;
mod worker;

pub(super) use events::emit_changed;
pub(crate) use recovery::recover;
pub(crate) use runtime::InvestigationRuntime;

use agents::investigation::{InvestigationInputLedger, InvestigationUsage};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum HostError {
    #[error("Investigation context changed during acquisition.")]
    InputChanged,
    #[error("Investigation evidence is unavailable; no source was substituted.")]
    SourceUnavailable,
    #[error("Investigation input or source metadata is invalid.")]
    InvalidInput,
    #[error("Investigation limit exhausted.")]
    LimitExceeded,
    #[error("Investigation storage or source ownership could not be verified.")]
    Operational,
}

impl From<agents::investigation::InvestigationError> for HostError {
    fn from(error: agents::investigation::InvestigationError) -> Self {
        match error {
            agents::investigation::InvestigationError::LimitExceeded => Self::LimitExceeded,
            _ => Self::InvalidInput,
        }
    }
}

/// Implemented by the owning worker's durable transition adapter. Publication
/// happens while source guards still protect every newly supplied receipt.
trait AcquisitionSink {
    fn usage(&self) -> &InvestigationUsage;
    fn reserve_validation(&mut self, bytes: u64) -> Result<(), HostError>;
    fn publish_input(
        &mut self,
        ledger: &InvestigationInputLedger,
        usage: InvestigationUsage,
    ) -> Result<(), HostError>;
}
