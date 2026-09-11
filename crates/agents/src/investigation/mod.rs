//! Closed, read-only investigation protocol (SPEC-10, ADR-0028).
//!
//! The host owns graph access, receipts, execution and durable transitions. This
//! module accepts bounded input data and admits model actions; it has no graph
//! store, filesystem, network dispatch or target-code mutation authority.

mod admission;
mod input;
mod privacy;
mod protocol;
mod records;
mod specialists;
mod strict;

pub use input::*;
pub use privacy::{redacted_history_text, reject_prose_replay};
pub use protocol::*;
pub use records::*;
pub use specialists::*;

#[cfg(test)]
mod tests;

/// Hard protocol and storage boundaries. Host counters may narrow these limits.
pub const MAX_ACTION_BYTES: usize = 32 * 1024;
pub const MAX_INPUT_BYTES: usize = 128 * 1024;
pub const MAX_MANIFEST_BYTES: usize = 128 * 1024;
pub const MAX_RESULT_BYTES: usize = 64 * 1024;
pub const MAX_RECORD_BYTES: usize = 256 * 1024;
pub const MAX_QUERY_BYTES: usize = 32 * 1024;
pub const MAX_QUERY_FACTS: usize = 32;
pub const MAX_SELECTED_FACTS: usize = 64;
pub const MAX_QUESTION_BYTES: usize = 2048;
pub const MAX_MODEL_CALLS: u32 = 8;
pub const MAX_TOOL_ACTIONS: u32 = 8;

/// Fixed diagnostics never expose source, prompts or unadmitted model output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InvestigationError {
    #[error("invalid investigation request")]
    InvalidRequest,
    #[error("invalid investigation action")]
    InvalidAction,
    #[error("investigation bound exceeded")]
    LimitExceeded,
    #[error("invalid investigation input")]
    InvalidInput,
    #[error("investigation output cites unavailable input")]
    InvalidCitation,
    #[error("investigation output repeats supplied text")]
    SourceReplay,
    #[error("investigation output contains sensitive text")]
    SensitiveOutput,
}

pub(crate) fn bounded_bytes(
    value: &impl serde::Serialize,
    limit: usize,
) -> Result<Vec<u8>, InvestigationError> {
    crate::source_basis::bounded_json(value, limit).map_err(|_| InvestigationError::LimitExceeded)
}

pub(crate) fn text(value: &str, maximum: usize) -> bool {
    crate::source_basis::text(value, maximum)
}

/// Versioned, bounded decoding for durable typed investigation bodies. SQL
/// callers must additionally preflight storage type and UTF-8 byte length before
/// retrieving a body; this method cannot retroactively bound that allocation.
pub fn decode_record<T: serde::de::DeserializeOwned + serde::Serialize>(
    body: &str,
) -> Result<T, InvestigationError> {
    strict::preflight(body, MAX_RECORD_BYTES, 32, 16384, 128)?;
    let value: T = serde_json::from_str(body).map_err(|_| InvestigationError::InvalidInput)?;
    // New investigation contracts have no legacy permissive fields. Typed
    // round-trip equality also closes nested DTOs reused from older contracts.
    let original: serde_json::Value =
        serde_json::from_str(body).map_err(|_| InvestigationError::InvalidInput)?;
    if serde_json::to_value(&value).map_err(|_| InvestigationError::InvalidInput)? != original {
        return Err(InvestigationError::InvalidInput);
    }
    Ok(value)
}
