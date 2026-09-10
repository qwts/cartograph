//! Private, persistent execution locks. A reservation never outlives its last
//! execution holder, and terminal database state does not release a live worker.

use cap_fs_ext::{
    DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt, OpenOptionsMaybeDirExt,
    OpenOptionsSyncExt,
};
use cap_std::fs::{Dir, DirBuilder, OpenOptions};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::jobs::JobTransitionError;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ExecutionNamespace {
    pub(super) value: String,
    pub(super) store_path: PathBuf,
}

#[derive(Clone)]
pub(crate) struct JobLockTarget {
    pub(super) namespace: ExecutionNamespace,
    pub(super) id: i64,
    pub(super) existing: bool,
}

/// Guard fields and owner identity are intentionally not serializable or Debug.
#[derive(Clone)]
pub(crate) struct JobExecution {
    pub(super) inner: Arc<ExecutionIdentity>,
}

pub(super) struct ExecutionIdentity {
    pub(super) namespace: ExecutionNamespace,
    pub(super) id: i64,
    pub(super) generation: i64,
    pub(super) owner: String,
    pub(super) reservation: ExecutionReservation,
}

impl JobExecution {
    pub(crate) fn id(&self) -> i64 {
        self.inner.id
    }

    pub(super) fn verify(&self) -> Result<(), JobTransitionError> {
        self.inner.reservation.verify()
    }
}

/// Owns one fresh OS handle. Never unlink the file, even after job cleanup.
pub(crate) struct ExecutionReservation {
    pub(super) target: JobLockTarget,
    file: File,
    storage: Arc<LockStorage>,
}

impl ExecutionReservation {
    pub(super) fn verify(&self) -> Result<(), JobTransitionError> {
        self.storage.verify()?;
        let current = self
            .storage
            .locks
            .symlink_metadata(self.target.id.to_string())
            .map_err(lock_error)?;
        let opened = self.file.metadata().map_err(lock_error)?;
        if !current.is_file()
            || current.nlink() != 1
            || current.dev() != opened.dev()
            || current.ino() != opened.ino()
        {
            return Err(JobTransitionError::LockUnavailable);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if opened.permissions().mode() & 0o077 != 0 {
                return Err(JobTransitionError::LockUnavailable);
            }
        }
        Ok(())
    }
}

impl Drop for ExecutionReservation {
    fn drop(&mut self) {
        // No descendant intentionally owns this handle. Explicit unlock also
        // handles an unrelated fork briefly inheriting its open-file description.
        // Arc keeps this Drop from running until the last real worker releases it.
        let _ = self.file.unlock();
    }
}

pub(crate) struct JobExecutionLocks {
    namespace: ExecutionNamespace,
    storage: Arc<LockStorage>,
}

impl JobExecutionLocks {
    pub(crate) fn open(
        app_data: &Path,
        namespace: ExecutionNamespace,
    ) -> Result<Self, JobTransitionError> {
        if namespace.value.len() != 32
            || !namespace
                .value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(JobTransitionError::InvalidMetadata);
        }
        let app_path = dunce::canonicalize(app_data).map_err(lock_error)?;
        // One state store cannot select a second lock root to evade its owners.
        if namespace.store_path.parent() != Some(app_path.as_path()) {
            return Err(JobTransitionError::ForeignStore);
        }
        let app =
            Dir::open_ambient_dir(&app_path, cap_std::ambient_authority()).map_err(lock_error)?;
        let root = private_dir(&app, "job-executions")?;
        let locks = private_dir(&root, &namespace.value)?;
        let storage = Arc::new(LockStorage {
            app_path,
            app,
            root,
            locks,
            name: namespace.value.clone(),
            #[cfg(test)]
            before_sync: std::sync::Mutex::new(None),
        });
        storage.verify()?;
        Ok(Self { namespace, storage })
    }

    /// Try once outside every application/database mutex. An earlier attempt
    /// must already have a lock inode; missing storage is not proof of death.
    pub(crate) fn try_reserve(
        &self,
        target: &JobLockTarget,
    ) -> Result<ExecutionReservation, JobTransitionError> {
        if target.namespace != self.namespace || target.id <= 0 {
            return Err(JobTransitionError::ForeignStore);
        }
        self.storage.verify()?;
        let file = private_file(
            &self.storage.locks,
            &target.id.to_string(),
            !target.existing,
        )?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(JobTransitionError::Busy),
            Err(std::fs::TryLockError::Error(_)) => {
                return Err(JobTransitionError::LockUnavailable);
            }
        }
        let reservation = ExecutionReservation {
            target: target.clone(),
            file,
            storage: self.storage.clone(),
        };
        reservation.verify()?;
        reservation.storage.sync_reservation(&reservation.file)?;
        reservation.verify()?;
        Ok(reservation)
    }
}

/// Rooted handles detect substitutions between operations. This is private
/// application storage, not protection from arbitrary same-user concurrent edits.
struct LockStorage {
    app_path: PathBuf,
    app: Dir,
    root: Dir,
    locks: Dir,
    name: String,
    #[cfg(test)]
    before_sync: std::sync::Mutex<Option<SyncHook>>,
}

#[cfg(test)]
type SyncHook = Box<dyn FnMut(SyncStep) -> std::io::Result<()> + Send>;

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SyncStep {
    File,
    Namespace,
    Root,
    AppData,
}

impl LockStorage {
    fn sync_reservation(&self, file: &File) -> Result<(), JobTransitionError> {
        // A later SQLite claim may survive power loss. Persist the same locked
        // inode and every newly-created directory entry before allowing it.
        // Repeat the whole chain even for an existing file: another process or
        // a failed earlier reservation may have created it without finishing sync.
        #[cfg(test)]
        self.before_sync(SyncStep::File)?;
        file.sync_all().map_err(lock_error)?;
        #[cfg(test)]
        self.before_sync(SyncStep::Namespace)?;
        sync_directory(&self.locks)?;
        #[cfg(test)]
        self.before_sync(SyncStep::Root)?;
        sync_directory(&self.root)?;
        #[cfg(test)]
        self.before_sync(SyncStep::AppData)?;
        sync_directory(&self.app)?;
        Ok(())
    }

    #[cfg(test)]
    fn before_sync(&self, step: SyncStep) -> Result<(), JobTransitionError> {
        if let Some(hook) = self.before_sync.lock().map_err(lock_error)?.as_mut() {
            hook(step).map_err(lock_error)?;
        }
        Ok(())
    }

    fn verify(&self) -> Result<(), JobTransitionError> {
        if dunce::canonicalize(&self.app_path).map_err(lock_error)? != self.app_path {
            return Err(JobTransitionError::LockUnavailable);
        }
        let app = Dir::open_ambient_dir(&self.app_path, cap_std::ambient_authority())
            .map_err(lock_error)?;
        let root = self
            .app
            .open_dir_nofollow("job-executions")
            .map_err(lock_error)?;
        let locks = self
            .root
            .open_dir_nofollow(&self.name)
            .map_err(lock_error)?;
        for (expected, current, private) in [
            (&self.app, &app, false),
            (&self.root, &root, true),
            (&self.locks, &locks, true),
        ] {
            if !same_entry(
                &expected.dir_metadata().map_err(lock_error)?,
                &current.dir_metadata().map_err(lock_error)?,
            ) {
                return Err(JobTransitionError::LockUnavailable);
            }
            #[cfg(unix)]
            {
                use cap_std::fs::PermissionsExt;
                if private
                    && current
                        .dir_metadata()
                        .map_err(lock_error)?
                        .permissions()
                        .mode()
                        & 0o077
                        != 0
                {
                    return Err(JobTransitionError::LockUnavailable);
                }
            }
            #[cfg(not(unix))]
            let _ = private;
        }
        Ok(())
    }
}

fn sync_directory(directory: &Dir) -> Result<(), JobTransitionError> {
    // Rooted Dir handles may use O_PATH on Linux, which cannot be synced.
    // Reopen the retained directory itself for reading, never its ambient path.
    let mut options = OpenOptions::new();
    options
        .read(true)
        .maybe_dir(true)
        .follow(FollowSymlinks::No)
        .nonblock(true);
    let file = directory.open_with(".", &options).map_err(lock_error)?;
    let opened = file.metadata().map_err(lock_error)?;
    if !opened.is_dir() || !same_entry(&opened, &directory.dir_metadata().map_err(lock_error)?) {
        return Err(JobTransitionError::LockUnavailable);
    }
    file.sync_all().map_err(lock_error)
}

fn lock_error(_: impl std::fmt::Display) -> JobTransitionError {
    JobTransitionError::LockUnavailable
}

fn same_entry(a: &cap_std::fs::Metadata, b: &cap_std::fs::Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

fn private_dir(parent: &Dir, name: &str) -> Result<Dir, JobTransitionError> {
    let mut builder = DirBuilder::new();
    #[cfg(unix)]
    {
        use cap_std::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match parent.create_dir_with(name, &builder) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(lock_error(error)),
    }
    let directory = parent.open_dir_nofollow(name).map_err(lock_error)?;
    let named = parent.symlink_metadata(name).map_err(lock_error)?;
    if !named.is_dir() || !same_entry(&named, &directory.dir_metadata().map_err(lock_error)?) {
        return Err(JobTransitionError::LockUnavailable);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Linux O_PATH directory handles require the rooted permission API.
        directory
            .set_permissions(
                ".",
                cap_std::fs::Permissions::from_std(std::fs::Permissions::from_mode(0o700)),
            )
            .map_err(lock_error)?;
    }
    Ok(directory)
}

fn private_file(parent: &Dir, name: &str, create: bool) -> Result<File, JobTransitionError> {
    let prior = match parent.symlink_metadata(name) {
        Ok(metadata) if metadata.is_file() && metadata.nlink() == 1 => Some(metadata),
        Ok(_) => return Err(JobTransitionError::LockUnavailable),
        Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(lock_error(error)),
    };
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .follow(FollowSymlinks::No)
        .nonblock(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = if prior.is_none() {
        options.create_new(true);
        match parent.open_with(name, &options) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                options.create_new(false);
                parent.open_with(name, &options).map_err(lock_error)?
            }
            Err(error) => return Err(lock_error(error)),
        }
    } else {
        parent.open_with(name, &options).map_err(lock_error)?
    };
    let opened = file.metadata().map_err(lock_error)?;
    let named = parent.symlink_metadata(name).map_err(lock_error)?;
    if !opened.is_file()
        || opened.nlink() != 1
        || !named.is_file()
        || !same_entry(&opened, &named)
        || prior.is_some_and(|prior| !same_entry(&prior, &opened))
    {
        return Err(JobTransitionError::LockUnavailable);
    }
    let file = file.into_std();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(lock_error)?;
    }
    Ok(file)
}

#[cfg(test)]
mod tests;
