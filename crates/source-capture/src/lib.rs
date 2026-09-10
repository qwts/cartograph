//! Immutable selected source bytes, independent of fact provenance (SPEC-04).
//!
//! A capture attests retained bytes, not whether an existing producer parsed
//! them. This crate has no app, graph, proposal, model, or network API.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;

mod acquisition;
mod store;

pub use acquisition::{capture_git, capture_working_tree};
pub use store::{CaptureInfo, CaptureStore, MAX_STORE_BYTES, MAX_STORED_CAPTURES, StoreLimits};

/// Canonical manifest interpretation version.
pub const CAPTURE_SCHEMA_VERSION: u32 = 1;
/// Maximum files explicitly selected for one capture.
pub const MAX_FILES: usize = 8_192;
/// Maximum UTF-8 bytes in a logical relative path.
pub const MAX_PATH_BYTES: usize = 1_024;
/// Maximum UTF-8 bytes in a host-assigned source identity.
pub const MAX_SOURCE_ID_BYTES: usize = 256;
/// Maximum raw bytes per captured file.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// Maximum raw bytes per capture, counting every selected file.
pub const MAX_CAPTURE_BYTES: u64 = 128 * 1024 * 1024;
/// Maximum serialized canonical manifest bytes.
pub const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
/// Maximum nonempty evidence span bytes.
pub const MAX_SPAN_BYTES: u64 = 256 * 1024;

/// Errors contain metadata and diagnostics, never captured source bytes.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    /// A supplied identity, path, range, or configuration is invalid.
    #[error("invalid capture input: {0}")]
    Invalid(&'static str),
    /// A declared or hard bound was exceeded.
    #[error("capture limit exceeded: {0}")]
    Limit(&'static str),
    /// Selected content or a retained capture is unavailable.
    #[error("capture content unavailable: {0}")]
    Missing(&'static str),
    /// Stored metadata or bytes do not match their immutable binding.
    #[error("capture integrity failure: {0}")]
    Corrupt(&'static str),
    /// Filesystem acquisition failed.
    #[error("capture filesystem error: {0}")]
    Io(#[from] std::io::Error),
    /// Local Git object access failed; no network fallback is attempted.
    #[error("capture Git object error: {0}")]
    Git(#[from] git2::Error),
    /// Metadata encoding or decoding failed.
    #[error("capture metadata error: {0}")]
    Json(#[from] serde_json::Error),
    /// Local transactional storage failed.
    #[error("capture storage error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// The selected span is not an exact UTF-8 string.
    #[error("capture span is not valid UTF-8")]
    Encoding,
}

/// Result of a capture operation.
pub type Result<T> = std::result::Result<T, CaptureError>;

/// Opaque trusted-host repository/worktree identity, never inferred from a path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SourceId(String);

impl SourceId {
    /// Validate a nonempty bounded identity supplied by the trusted host.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_SOURCE_ID_BYTES || value.contains('\0') {
            return Err(CaptureError::Invalid("source identity"));
        }
        Ok(Self(value))
    }

    /// Return the opaque identity without interpreting it as a filesystem path.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SourceId {
    type Error = CaptureError;

    fn try_from(value: String) -> Result<Self> {
        Self::new(value)
    }
}

impl From<SourceId> for String {
    fn from(value: SourceId) -> Self {
        value.0
    }
}

/// Acquisition and read bounds; callers may lower but never raise hard maxima.
#[derive(Debug, Clone, Copy)]
pub struct CaptureLimits {
    /// Selected-file count bound; zero permits an explicit empty capture only.
    pub max_files: usize,
    /// Relative-path UTF-8 byte bound.
    pub max_path_bytes: usize,
    /// Opaque source-identity UTF-8 byte bound.
    pub max_source_id_bytes: usize,
    /// Raw byte bound for one file.
    pub max_file_bytes: u64,
    /// Sum of selected raw byte lengths.
    pub max_total_bytes: u64,
    /// Serialized metadata byte bound.
    pub max_manifest_bytes: u64,
    /// Nonempty span byte bound.
    pub max_span_bytes: u64,
}

impl Default for CaptureLimits {
    fn default() -> Self {
        Self {
            max_files: MAX_FILES,
            max_path_bytes: MAX_PATH_BYTES,
            max_source_id_bytes: MAX_SOURCE_ID_BYTES,
            max_file_bytes: MAX_FILE_BYTES,
            max_total_bytes: MAX_CAPTURE_BYTES,
            max_manifest_bytes: MAX_MANIFEST_BYTES,
            max_span_bytes: MAX_SPAN_BYTES,
        }
    }
}

impl CaptureLimits {
    /// Reject any requested bound greater than the core's hard maximum.
    pub fn validate(self) -> Result<()> {
        if self.max_files > MAX_FILES
            || self.max_path_bytes > MAX_PATH_BYTES
            || self.max_source_id_bytes > MAX_SOURCE_ID_BYTES
            || self.max_file_bytes > MAX_FILE_BYTES
            || self.max_total_bytes > MAX_CAPTURE_BYTES
            || self.max_manifest_bytes > MAX_MANIFEST_BYTES
            || self.max_span_bytes > MAX_SPAN_BYTES
        {
            return Err(CaptureError::Invalid("limits exceed hard maxima"));
        }
        Ok(())
    }
}

/// How the retained raw bytes were acquired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CaptureKind {
    /// Selected regular working-tree files; not an atomic filesystem snapshot.
    WorkingTree,
    /// Exact local Git objects, independent of checkout conversion.
    Git {
        /// Full SHA-1 commit OID.
        commit: String,
        /// Full SHA-1 root tree OID.
        tree: String,
    },
}

/// Git tree entry bound alongside a file's raw digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitFile {
    /// Full regular blob OID.
    pub oid: String,
    /// Exact regular-file mode (100644 or 100755 in octal).
    pub mode: u32,
}

/// Metadata for one selected file; never includes raw source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    /// Canonical relative UTF-8 path.
    pub path: String,
    /// Lowercase BLAKE3 digest of the unmodified acquired bytes.
    pub digest: String,
    /// Actual acquired raw byte length.
    pub byte_len: u64,
    /// Exact Git tree metadata, present only for Git-object captures.
    pub git: Option<GitFile>,
}

/// Canonical, metadata-only manifest; entries are sorted by exact path bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureManifest {
    /// Interpretation version, currently one.
    pub schema_version: u32,
    /// Host-owned source identity.
    pub source_id: SourceId,
    /// Working-tree or exact Git acquisition.
    pub kind: CaptureKind,
    /// Exact selected membership, including empty files.
    pub files: Vec<FileEntry>,
}

/// Integrity binding to one retained file, separate from legacy EvidenceRef.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureFileRef {
    /// Host-owned source identity.
    pub source_id: SourceId,
    /// Versioned canonical manifest digest.
    pub capture_id: String,
    /// Exact selected relative path.
    pub path: String,
    /// Digest of the full raw file, including bytes outside cited spans.
    pub digest: String,
    /// Full raw byte length.
    pub byte_len: u64,
}

/// A nonempty byte range within one fully bound file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureSpanRef {
    /// Complete file binding, not a filesystem authority token.
    pub file: CaptureFileRef,
    /// Inclusive raw-byte offset.
    pub byte_start: u64,
    /// Exclusive raw-byte offset.
    pub byte_end: u64,
}

/// Immutable bytes and their binding. Debug intentionally prints metadata only.
#[derive(Clone)]
pub struct CapturedFile {
    reference: CaptureFileRef,
    bytes: Vec<u8>,
    max_span_bytes: u64,
}

impl fmt::Debug for CapturedFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CapturedFile")
            .field("reference", &self.reference)
            .finish_non_exhaustive()
    }
}

impl CapturedFile {
    /// Access the same immutable raw buffer retained at acquisition or load.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Return this file's complete metadata binding.
    pub fn reference(&self) -> &CaptureFileRef {
        &self.reference
    }

    /// Bind a bounded nonempty range to this captured file.
    pub fn span(&self, byte_start: u64, byte_end: u64) -> Result<CaptureSpanRef> {
        validate_range(
            byte_start,
            byte_end,
            self.reference.byte_len,
            self.max_span_bytes,
        )?;
        Ok(CaptureSpanRef {
            file: self.reference.clone(),
            byte_start,
            byte_end,
        })
    }
}

/// An immutable, bounded selected byte set and canonical manifest.
#[derive(Clone)]
pub struct Capture {
    id: String,
    manifest: CaptureManifest,
    files: BTreeMap<String, CapturedFile>,
    max_span_bytes: u64,
}

impl fmt::Debug for Capture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Capture")
            .field("id", &self.id)
            .field("manifest", &self.manifest)
            .field("max_span_bytes", &self.max_span_bytes)
            .finish_non_exhaustive()
    }
}

impl Capture {
    /// Versioned BLAKE3 identity of the canonical complete manifest.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Metadata only; serializing this value never includes source bytes.
    pub fn manifest(&self) -> &CaptureManifest {
        &self.manifest
    }

    /// Effective span-read bound, separate from canonical byte identity.
    /// Store persistence can tighten future loads without revoking this buffer.
    pub fn max_span_bytes(&self) -> u64 {
        self.max_span_bytes
    }

    /// Look up an exact selected path; absence makes no repository-wide claim.
    pub fn file(&self, path: &str) -> Result<&CapturedFile> {
        validate_path(path, MAX_PATH_BYTES)?;
        self.files
            .get(path)
            .ok_or(CaptureError::Missing("path outside capture"))
    }

    /// Check the complete binding and range, then borrow the retained buffer.
    pub fn read_span(&self, reference: &CaptureSpanRef) -> Result<&[u8]> {
        let file = self.file(&reference.file.path)?;
        if reference.file != file.reference {
            return Err(CaptureError::Corrupt("file reference mismatch"));
        }
        validate_range(
            reference.byte_start,
            reference.byte_end,
            file.reference.byte_len,
            file.max_span_bytes,
        )?;
        Ok(&file.bytes[reference.byte_start as usize..reference.byte_end as usize])
    }

    /// Strictly decode only the requested verified byte span.
    pub fn read_text_span(&self, reference: &CaptureSpanRef) -> Result<&str> {
        std::str::from_utf8(self.read_span(reference)?).map_err(|_| CaptureError::Encoding)
    }
}

fn validate_range(start: u64, end: u64, len: u64, max: u64) -> Result<()> {
    if end <= start || end > len {
        return Err(CaptureError::Invalid(
            "span must be nonempty and within the file",
        ));
    }
    if end - start > max {
        return Err(CaptureError::Limit("span bytes"));
    }
    Ok(())
}

fn validate_path(path: &str, limit: usize) -> Result<()> {
    if path.len() > limit {
        return Err(CaptureError::Limit("path bytes"));
    }
    if path.is_empty()
        || path.contains(['\\', ':', '\0'])
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(CaptureError::Invalid("canonical relative path required"));
    }
    Ok(())
}

fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn validate_manifest(manifest: &CaptureManifest, limits: CaptureLimits) -> Result<()> {
    limits.validate()?;
    if manifest.schema_version != CAPTURE_SCHEMA_VERSION {
        return Err(CaptureError::Corrupt("unsupported manifest version"));
    }
    if manifest.source_id.as_str().len() > limits.max_source_id_bytes {
        return Err(CaptureError::Limit("source identity bytes"));
    }
    if manifest.files.len() > limits.max_files {
        return Err(CaptureError::Limit("selected files"));
    }
    if let CaptureKind::Git { commit, tree } = &manifest.kind
        && (!is_hex(commit, 40) || !is_hex(tree, 40))
    {
        return Err(CaptureError::Corrupt("invalid Git identity"));
    }
    let mut prior: Option<&str> = None;
    let mut total = 0u64;
    for entry in &manifest.files {
        validate_path(&entry.path, limits.max_path_bytes)?;
        if prior.is_some_and(|path| path >= entry.path.as_str()) {
            return Err(CaptureError::Corrupt("duplicate or unsorted manifest path"));
        }
        prior = Some(&entry.path);
        if !is_hex(&entry.digest, 64) {
            return Err(CaptureError::Corrupt("invalid raw digest"));
        }
        if entry.byte_len > limits.max_file_bytes {
            return Err(CaptureError::Limit("file bytes"));
        }
        total = total
            .checked_add(entry.byte_len)
            .ok_or(CaptureError::Limit("capture bytes"))?;
        if total > limits.max_total_bytes {
            return Err(CaptureError::Limit("capture bytes"));
        }
        match (&manifest.kind, &entry.git) {
            (CaptureKind::WorkingTree, None) => {}
            (CaptureKind::Git { .. }, Some(git))
                if is_hex(&git.oid, 40) && matches!(git.mode, 0o100644 | 0o100755) => {}
            _ => return Err(CaptureError::Corrupt("capture kind and Git entry disagree")),
        }
    }
    Ok(())
}

fn canonical_manifest(manifest: &CaptureManifest, limit: u64) -> Result<Vec<u8>> {
    struct Counter {
        count: u64,
        limit: u64,
    }
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.count = self.count.saturating_add(bytes.len() as u64);
            if self.count > self.limit {
                return Err(std::io::Error::other("manifest byte limit"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Counter { count: 0, limit };
    let result = serde_json::to_writer(&mut count, manifest);
    if count.count > limit {
        return Err(CaptureError::Limit("manifest bytes"));
    }
    result?;
    Ok(serde_json::to_vec(manifest)?)
}

fn manifest_id(bytes: &[u8]) -> String {
    format!("capture-v1:{}", blake3::hash(bytes).to_hex())
}

fn validate_capture_id(id: &str) -> Result<()> {
    if !id
        .strip_prefix("capture-v1:")
        .is_some_and(|hash| is_hex(hash, 64))
    {
        return Err(CaptureError::Invalid("capture identity"));
    }
    Ok(())
}

fn assemble_capture(
    manifest: CaptureManifest,
    mut bytes: BTreeMap<String, Vec<u8>>,
    limits: CaptureLimits,
) -> Result<Capture> {
    validate_manifest(&manifest, limits)?;
    let id = manifest_id(&canonical_manifest(&manifest, limits.max_manifest_bytes)?);
    let mut files = BTreeMap::new();
    for entry in &manifest.files {
        let raw = bytes
            .remove(&entry.path)
            .ok_or(CaptureError::Corrupt("missing captured bytes"))?;
        if raw.len() as u64 != entry.byte_len
            || blake3::hash(&raw).to_hex().as_str() != entry.digest
        {
            return Err(CaptureError::Corrupt("raw bytes disagree with manifest"));
        }
        if let Some(git) = &entry.git {
            let oid = git2::Oid::hash_object(git2::ObjectType::Blob, &raw)?;
            if oid.to_string() != git.oid {
                return Err(CaptureError::Corrupt("Git blob identity mismatch"));
            }
        }
        files.insert(
            entry.path.clone(),
            CapturedFile {
                reference: CaptureFileRef {
                    source_id: manifest.source_id.clone(),
                    capture_id: id.clone(),
                    path: entry.path.clone(),
                    digest: entry.digest.clone(),
                    byte_len: entry.byte_len,
                },
                bytes: raw,
                max_span_bytes: limits.max_span_bytes,
            },
        );
    }
    if !bytes.is_empty() {
        return Err(CaptureError::Corrupt("extra captured bytes"));
    }
    Ok(Capture {
        id,
        manifest,
        files,
        max_span_bytes: limits.max_span_bytes,
    })
}

#[cfg(test)]
mod tests;
