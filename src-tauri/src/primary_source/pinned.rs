//! Historical receipt reads; current graph association is deliberately absent.

use super::*;
use source_capture::CaptureError;

/// Fixed categories keep missing content separate from storage/lock failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PinnedReadError {
    Unavailable,
    Invalid,
    Operational,
}

impl std::fmt::Display for PinnedReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "Retained task evidence is unavailable; no source was substituted.",
            Self::Invalid => "Retained task evidence is invalid; no source was substituted.",
            Self::Operational => "Retained task evidence could not be checked; retry after the storage or source operation completes.",
        })
    }
}

impl std::error::Error for PinnedReadError {}

pub(super) fn stored_receipt(conn: &Connection, id: &str) -> Result<Receipt, PinnedReadError> {
    let row: Option<(String, String, String)> = conn
        .query_row(
            "SELECT source_id,repo_key,payload FROM receipts WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|_| PinnedReadError::Operational)?;
    let (source, repo, json) = row.ok_or(PinnedReadError::Unavailable)?;
    let receipt = Receipt::from_json(&json).map_err(|_| PinnedReadError::Invalid)?;
    if receipt.id() != id
        || receipt.source_id().as_str() != source
        || receipt.repo_key() != repo
        || receipt.to_json().map_err(|_| PinnedReadError::Invalid)? != json
    {
        return Err(PinnedReadError::Invalid);
    }
    Ok(receipt)
}

impl PrimarySourceStore {
    pub(crate) fn task_guard(&self, source: &str) -> Result<RetentionGuard, PinnedReadError> {
        self.guard(source, false)
            .map_err(|_| PinnedReadError::Operational)
    }

    fn validate_task_guard(
        &self,
        guard: &RetentionGuard,
        source: &str,
    ) -> Result<(), PinnedReadError> {
        if guard.source_id != source || guard.app_path != self.storage.app_path {
            return Err(PinnedReadError::Invalid);
        }
        self.storage
            .verify()
            .map_err(|_| PinnedReadError::Operational)
    }

    pub(crate) fn task_receipt(
        &self,
        guard: &RetentionGuard,
        fact: &FactKey,
        digest: &str,
        repo: &str,
        receipt_id: &str,
    ) -> Result<Receipt, PinnedReadError> {
        self.validate_task_guard(guard, &guard.source_id)?;
        if receipt_id.is_empty() || receipt_id.len() > 256 {
            return Err(PinnedReadError::Invalid);
        }
        let store = self
            .receipts
            .lock()
            .map_err(|_| PinnedReadError::Operational)?;
        let tx = store
            .conn
            .unchecked_transaction()
            .map_err(|_| PinnedReadError::Operational)?;
        receipt_storage(&tx).map_err(|_| PinnedReadError::Operational)?;
        let receipt = stored_receipt(&tx, receipt_id)?;
        if receipt.source_id().as_str() != guard.source_id
            || receipt.repo_key() != repo
            || receipt.fact_key() != fact
            || receipt.fact_digest() != digest
        {
            return Err(PinnedReadError::Invalid);
        }
        tx.commit().map_err(|_| PinnedReadError::Operational)?;
        Ok(receipt)
    }

    /// The caller charges complete file-validation work before invoking this.
    /// The receipt and lease are already pinned; no current graph lookup occurs.
    pub(crate) fn task_text(
        &self,
        guard: &RetentionGuard,
        receipt: &Receipt,
        range_index: usize,
    ) -> Result<String, PinnedReadError> {
        self.validate_task_guard(guard, receipt.source_id().as_str())?;
        let range = receipt
            .ranges()
            .get(range_index)
            .ok_or(PinnedReadError::Invalid)?;
        self.captures
            .lock()
            .map_err(|_| PinnedReadError::Operational)?
            .read_text_span(&range.captured)
            .map_err(|error| match error {
                CaptureError::Missing(_) => PinnedReadError::Unavailable,
                CaptureError::Invalid(_)
                | CaptureError::Limit(_)
                | CaptureError::Corrupt(_)
                | CaptureError::Encoding
                | CaptureError::Json(_) => PinnedReadError::Invalid,
                CaptureError::Sqlite(_) | CaptureError::Io(_) | CaptureError::Git(_) => {
                    PinnedReadError::Operational
                }
            })
    }
}
