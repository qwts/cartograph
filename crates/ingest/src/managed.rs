//! Host-owned managed checkouts and nonblocking source guards (SPEC-05).
//!
//! The registry supplies trusted installation/source IDs and controls availability.
//! These guards coordinate participating processes; they do not attest source
//! bytes or exclude arbitrary filesystem writers. Keep a write guard alive through
//! the host's complete parse/enrichment/publication pass after cloning returns.

use crate::{ClonedRepo, IngestError, clone_into_new};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

const MAX_ORIGIN_BYTES: usize = 8 * 1024;
const MAX_OWNER_BYTES: u64 = 4096;
static ATTEMPT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A bounded normalized origin. This contains no authentication token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedOrigin {
    /// Stable registry locator: `github:owner/repo` or a canonical file URL.
    pub key: String,
    /// Canonical URL passed to the existing libgit2 clone mechanism.
    pub clone_url: String,
    /// Human-readable repository basename, separate from source identity.
    pub display_name: String,
    /// Canonical GitHub repository key, absent for a local mirror.
    pub github_repo: Option<String>,
}

impl ManagedOrigin {
    /// Validate stored metadata without reading or requiring the original source.
    /// This checks canonical serialization and field agreement, not availability.
    pub fn validate(&self) -> Result<(), IngestError> {
        if self.key.len() > MAX_ORIGIN_BYTES
            || self.clone_url.len() > MAX_ORIGIN_BYTES
            || self.display_name.len() > MAX_ORIGIN_BYTES
            || self
                .github_repo
                .as_ref()
                .is_some_and(|repo| repo.len() > 201)
        {
            return Err(IngestError::InvalidManagedOrigin);
        }
        let expected = if self.clone_url.starts_with("file:") {
            let parsed = local_file_url(&self.clone_url)?;
            let path = parsed
                .to_file_path()
                .map_err(|_| IngestError::InvalidManagedOrigin)?;
            if !absolute_normal_path(&path) || path.to_str().is_none() {
                return Err(IngestError::InvalidManagedOrigin);
            }
            file_origin(&path)?
        } else {
            github_origin(&self.clone_url)?
        };
        if &expected != self {
            return Err(IngestError::InvalidManagedOrigin);
        }
        Ok(())
    }
}

/// Normalize supported GitHub or local file origins before host registration.
/// File URLs are decoded and canonicalized exactly; absent mirrors fail here,
/// while [`ManagedOrigin::validate`] can validate their retained metadata later.
pub fn parse_managed_origin(input: &str) -> Result<ManagedOrigin, IngestError> {
    let input = input.trim();
    if input.is_empty() || input.len() > MAX_ORIGIN_BYTES || input.chars().any(char::is_control) {
        return Err(IngestError::InvalidManagedOrigin);
    }
    let origin = if input
        .get(..5)
        .is_some_and(|s| s.eq_ignore_ascii_case("file:"))
    {
        let parsed = local_file_url(input)?;
        let path = parsed
            .to_file_path()
            .map_err(|_| IngestError::InvalidManagedOrigin)?;
        let canonical = dunce::canonicalize(path)?;
        if canonical.to_str().is_none() || !canonical.is_dir() {
            return Err(IngestError::InvalidManagedOrigin);
        }
        file_origin(&canonical)?
    } else {
        github_origin(input)?
    };
    origin.validate()?;
    Ok(origin)
}

fn local_file_url(input: &str) -> Result<Url, IngestError> {
    let parsed = Url::parse(input).map_err(|_| IngestError::InvalidManagedOrigin)?;
    if parsed.scheme() != "file"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.port().is_some()
        || parsed.host_str().is_some_and(|host| host != "localhost")
    {
        return Err(IngestError::InvalidManagedOrigin);
    }
    Ok(parsed)
}

fn file_origin(path: &Path) -> Result<ManagedOrigin, IngestError> {
    let clone_url = Url::from_file_path(path)
        .map_err(|_| IngestError::InvalidManagedOrigin)?
        .to_string();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or(IngestError::InvalidManagedOrigin)?;
    let display_name = name.strip_suffix(".git").unwrap_or(name).to_string();
    if display_name.is_empty() {
        return Err(IngestError::InvalidManagedOrigin);
    }
    Ok(ManagedOrigin {
        key: clone_url.clone(),
        clone_url,
        display_name,
        github_repo: None,
    })
}

fn github_origin(input: &str) -> Result<ManagedOrigin, IngestError> {
    let candidate = if let Some((host, path)) = input.split_once(':')
        && host.eq_ignore_ascii_case("git@github.com")
    {
        format!("https://github.com/{path}")
    } else if !input.contains(':') {
        format!("https://github.com/{input}")
    } else {
        input.to_string()
    };
    let parsed = Url::parse(&candidate).map_err(|_| IngestError::InvalidManagedOrigin)?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("github.com")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.port().is_some()
        || input
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return Err(IngestError::InvalidManagedOrigin);
    }
    let path = parsed.path().trim_end_matches('/').to_ascii_lowercase();
    let mut parts = path.strip_prefix('/').unwrap_or(&path).split('/');
    let owner = parts.next().ok_or(IngestError::InvalidManagedOrigin)?;
    let name = parts.next().ok_or(IngestError::InvalidManagedOrigin)?;
    let name = name.strip_suffix(".git").unwrap_or(name);
    if parts.next().is_some()
        || owner.is_empty()
        || owner.len() > 100
        || !owner
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        || name.is_empty()
        || name.len() > 100
        || matches!(name, "." | "..")
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
    {
        return Err(IngestError::InvalidManagedOrigin);
    }
    let repo = format!("{owner}/{name}");
    Ok(ManagedOrigin {
        key: format!("github:{repo}"),
        clone_url: format!("https://github.com/{repo}.git"),
        display_name: name.to_string(),
        github_repo: Some(repo),
    })
}

/// Metadata-only derivation of a registry-owned checkout and its stable lock path.
#[derive(Debug, Clone)]
pub struct ManagedCheckout {
    app_data_dir: PathBuf,
    slot: PathBuf,
    root: PathBuf,
    registry_id: String,
    source_id: String,
}

impl ManagedCheckout {
    /// Derive a host-owned slot without creating or modifying any filesystem entry.
    /// `app_data_dir` must be the trusted absolute application storage directory.
    pub fn new(
        app_data_dir: &Path,
        registry_id: &str,
        source_id: &str,
    ) -> Result<Self, IngestError> {
        if !valid_identity(registry_id, "reg_")
            || !valid_identity(source_id, "src_")
            || !absolute_normal_path(app_data_dir)
            || app_data_dir.to_str().is_none()
        {
            return Err(IngestError::InvalidManagedIdentity);
        }
        let slot = app_data_dir.join("sources").join(source_id);
        Ok(Self {
            app_data_dir: app_data_dir.to_path_buf(),
            root: slot.join("checkout"),
            slot,
            registry_id: registry_id.into(),
            source_id: source_id.into(),
        })
    }

    /// Fixed checkout root; deriving it alone establishes no availability.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Acquire a nonblocking shared guard over an existing owned checkout.
    pub fn try_read(&self) -> Result<ManagedReadGuard, IngestError> {
        let lock = self.acquire_lock(false)?;
        self.validate_slot()?;
        validate_checkout(&self.root)?;
        Ok(ManagedReadGuard {
            checkout: self.clone(),
            _lock: lock,
        })
    }

    /// Acquire exclusive use, initializing only an absent owned slot. The host
    /// separately marks availability false/true around its complete operation.
    pub fn try_write(&self) -> Result<ManagedWriteGuard, IngestError> {
        let lock = self.acquire_lock(true)?;
        self.initialize_slot()?;
        self.validate_slot()?;
        if entry_exists(&self.root)? {
            validate_checkout(&self.root)?;
        }
        Ok(ManagedWriteGuard {
            checkout: self.clone(),
            _lock: lock,
        })
    }

    fn acquire_lock(&self, exclusive: bool) -> Result<File, IngestError> {
        validate_directory(&self.app_data_dir, false)?;
        let parent = self.app_data_dir.join("source-locks");
        ensure_private_directory(&parent)?;
        let path = parent.join(format!("{}.lock", self.source_id));
        if entry_exists(&path)? {
            validate_regular_file(&path)?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        if !file.metadata()?.is_file() {
            return Err(IngestError::SourceOwnership);
        }
        validate_regular_file(&path)?;
        let result = if exclusive {
            file.try_lock()
        } else {
            file.try_lock_shared()
        };
        match result {
            Ok(()) => Ok(file),
            Err(TryLockError::WouldBlock) => Err(IngestError::SourceBusy),
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }

    fn initialize_slot(&self) -> Result<(), IngestError> {
        self.initialize_slot_before_publish(|_| Ok(()))
    }

    fn initialize_slot_before_publish(
        &self,
        before_publish: impl FnOnce(&Path) -> Result<(), IngestError>,
    ) -> Result<(), IngestError> {
        validate_directory(&self.app_data_dir, false)?;
        let parent = self.app_data_dir.join("sources");
        ensure_private_directory(&parent)?;
        if entry_exists(&self.slot)? {
            return self.validate_slot();
        }
        // The stable source lock is already held. Publish the complete owner
        // directory, never an empty final slot that a crash makes unrecognizable.
        let mut attempt = Attempt::new(&parent)?;
        let owner = serde_json::to_vec(&self.owner()).map_err(|_| IngestError::SourceOwnership)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(attempt.path.join("owner.json"))?;
        file.write_all(&owner)?;
        file.sync_all()?;
        drop(file);
        sync_directory(&attempt.path)?;
        before_publish(&attempt.path)?;
        validate_directory(&self.app_data_dir, false)?;
        validate_directory(&parent, true)?;
        // All cooperating publishers hold this source's exclusive lock. Even an
        // unexpected empty destination is a conflict, never overwrite authority.
        if entry_exists(&self.slot)? {
            return Err(IngestError::SourceOwnership);
        }
        fs::rename(&attempt.path, &self.slot)?;
        attempt.cleanup = false;
        sync_directory(&parent)?;
        Ok(())
    }

    fn owner(&self) -> SlotOwner {
        SlotOwner {
            schema_version: 1,
            registry_id: self.registry_id.clone(),
            source_id: self.source_id.clone(),
        }
    }

    fn validate_slot(&self) -> Result<(), IngestError> {
        validate_directory(&self.app_data_dir, false)?;
        validate_directory(&self.app_data_dir.join("sources"), true)?;
        validate_directory(&self.slot, true)?;
        let path = self.slot.join("owner.json");
        validate_regular_file(&path)?;
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_OWNER_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_OWNER_BYTES {
            return Err(IngestError::SourceOwnership);
        }
        let owner: SlotOwner =
            serde_json::from_slice(&bytes).map_err(|_| IngestError::SourceOwnership)?;
        if owner != self.owner() {
            return Err(IngestError::SourceOwnership);
        }
        Ok(())
    }
}

/// A shared managed-source read lease. Drop it after the bounded root-dependent
/// read pass; do not retain it while awaiting a model or human response.
#[derive(Debug)]
pub struct ManagedReadGuard {
    checkout: ManagedCheckout,
    _lock: File,
}

impl ManagedReadGuard {
    /// Owned checkout whose replacement this guard excludes.
    pub fn root(&self) -> &Path {
        self.checkout.root()
    }

    /// Registry source identifier held by this guard.
    pub fn source_id(&self) -> &str {
        &self.checkout.source_id
    }
}

/// Exclusive managed-source use. This is deliberately not cloneable and remains
/// locked after [`Self::clone_from`] returns, for host parse/enrichment/publication.
#[derive(Debug)]
pub struct ManagedWriteGuard {
    checkout: ManagedCheckout,
    _lock: File,
}

impl ManagedWriteGuard {
    /// Fixed owned checkout root, including before its first successful clone.
    pub fn root(&self) -> &Path {
        self.checkout.root()
    }

    /// Registry source identifier held by this guard.
    pub fn source_id(&self) -> &str {
        &self.checkout.source_id
    }

    /// Clone into a fresh owned attempt and publish without first deleting the
    /// prior checkout. The caller must retain this guard throughout later use and
    /// control durable availability; this function never marks the source ready.
    pub fn clone_from(
        &mut self,
        origin: &ManagedOrigin,
        token: Option<&str>,
    ) -> Result<ClonedRepo, IngestError> {
        origin.validate()?;
        self.checkout.validate_slot()?;
        if entry_exists(self.root())? {
            validate_checkout(self.root())?;
        }
        let mut attempt = Attempt::new(&self.checkout.slot)?;
        let staged = attempt.path.join("checkout");
        let commit_sha = clone_into_new(&origin.clone_url, &staged, token)?;
        validate_checkout(&staged)?;
        self.checkout.validate_slot()?;
        let previous = attempt.path.join("previous");
        let had_previous = entry_exists(self.root())?;
        if had_previous {
            validate_checkout(self.root())?;
            // Once prior content moves, failed restoration must retain the attempt
            // rather than allowing cleanup to destroy the only previous checkout.
            attempt.cleanup = false;
            fs::rename(self.root(), &previous)?;
        }
        if fs::rename(&staged, self.root()).is_err() {
            if had_previous {
                if entry_exists(self.root())? || fs::rename(&previous, self.root()).is_err() {
                    return Err(IngestError::SourcePublication);
                }
                attempt.cleanup = true;
            }
            return Err(IngestError::SourcePublication);
        }
        attempt.cleanup = true;
        Ok(ClonedRepo {
            repo: origin
                .github_repo
                .clone()
                .unwrap_or_else(|| format!("local/{}", self.source_id())),
            commit_sha,
            path: self.root().to_path_buf(),
        })
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SlotOwner {
    schema_version: u32,
    registry_id: String,
    source_id: String,
}

struct Attempt {
    path: PathBuf,
    cleanup: bool,
}

impl Attempt {
    fn new(slot: &Path) -> Result<Self, IngestError> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for _ in 0..64 {
            let sequence = ATTEMPT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = slot.join(format!(
                ".attempt-{}-{timestamp}-{sequence}",
                std::process::id()
            ));
            match create_private_directory(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        cleanup: true,
                    });
                }
                Err(IngestError::Io(error))
                    if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(IngestError::SourceOwnership)
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        if self.cleanup {
            // Only the directory successfully created by this attempt is removed.
            // Never enumerate/delete another process's or a crash-stale attempt.
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn valid_identity(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
    })
}

fn absolute_normal_path(path: &Path) -> bool {
    path.is_absolute()
        && path.to_str().is_some_and(|text| !text.contains('\0'))
        && !path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
}

fn entry_exists(path: &Path) -> Result<bool, IngestError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn validate_directory(path: &Path, private: bool) -> Result<(), IngestError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| IngestError::SourceOwnership)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(IngestError::SourceOwnership);
    }
    if dunce::canonicalize(path).map_err(|_| IngestError::SourceOwnership)? != path {
        return Err(IngestError::SourceOwnership);
    }
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(IngestError::SourceOwnership);
        }
    }
    #[cfg(not(unix))]
    let _ = private;
    Ok(())
}

fn validate_regular_file(path: &Path) -> Result<(), IngestError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| IngestError::SourceOwnership)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(IngestError::SourceOwnership);
    }
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<(), IngestError> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}

fn ensure_private_directory(path: &Path) -> Result<(), IngestError> {
    match create_private_directory(path) {
        Ok(()) => Ok(()),
        Err(IngestError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            validate_directory(path, true)
        }
        Err(error) => Err(error),
    }
}

fn sync_directory(path: &Path) -> Result<(), IngestError> {
    // std cannot portably open a directory for synchronization on Windows.
    // The ownership file is synced on every target before namespace publication.
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn validate_checkout(root: &Path) -> Result<(), IngestError> {
    validate_directory(root, false)?;
    validate_directory(&root.join(".git"), false)?;
    let repo = git2::Repository::open(root).map_err(|_| IngestError::SourceOwnership)?;
    let workdir = repo.workdir().ok_or(IngestError::SourceOwnership)?;
    if repo.is_bare()
        || dunce::canonicalize(workdir)? != dunce::canonicalize(root)?
        || dunce::canonicalize(repo.path())? != dunce::canonicalize(root.join(".git"))?
    {
        return Err(IngestError::SourceOwnership);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
