use super::*;
use crate::jobs::{ClaimMode, ExecutionCheck, JobStore};
use std::io::{BufRead, Write};

fn fixture() -> (tempfile::TempDir, JobStore, JobExecutionLocks) {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open(dir.path().join("state.db")).unwrap();
    let locks = JobExecutionLocks::open(dir.path(), store.execution_namespace()).unwrap();
    (dir, store, locks)
}

fn start(store: &mut JobStore, locks: &JobExecutionLocks) -> JobExecution {
    let id = store.enqueue("noop").unwrap().id;
    let plan = store.claim_plan(id, ClaimMode::StartQueued).unwrap();
    let reservation = locks.try_reserve(plan.lock_target()).unwrap();
    store.claim_execution(&plan, reservation).unwrap().1
}

fn assert_unclaimed(connection: &rusqlite::Connection, id: i64) {
    let state: (String, i64, Option<String>) = connection
        .query_row(
            "SELECT j.status, a.generation, a.owner FROM jobs j JOIN job_attempts a ON a.job_id = j.id WHERE j.id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(state, ("queued".into(), 0, None));
}

#[test]
fn execution_reservations_sync_storage_before_claim() {
    // AC-0157: lock inode and all new ancestor entries are synced before SQL
    // ownership; a previously-created unclaimed file takes the same durable path.
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("state.db");
    let mut store = JobStore::open(&database).unwrap();
    assert!(!dir.path().join("job-executions").exists());
    let locks = JobExecutionLocks::open(dir.path(), store.execution_namespace()).unwrap();
    let id = store.enqueue("noop").unwrap().id;
    let plan = store.claim_plan(id, ClaimMode::StartQueued).unwrap();
    let observer = rusqlite::Connection::open(&database).unwrap();
    let steps = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = steps.clone();
    *locks.storage.before_sync.lock().unwrap() = Some(Box::new(move |step| {
        assert_unclaimed(&observer, id);
        observed.lock().unwrap().push(step);
        Ok(())
    }));
    let expected = [
        SyncStep::File,
        SyncStep::Namespace,
        SyncStep::Root,
        SyncStep::AppData,
    ];
    let reservation = locks.try_reserve(plan.lock_target()).unwrap();
    assert_eq!(*steps.lock().unwrap(), expected);
    drop(reservation);
    steps.lock().unwrap().clear();
    let reservation = locks.try_reserve(plan.lock_target()).unwrap();
    assert_eq!(*steps.lock().unwrap(), expected);
    let (_, execution) = store.claim_execution(&plan, reservation).unwrap();
    assert_eq!(execution.inner.generation, 1);
    let reopened = JobStore::open(&database).unwrap();
    assert_eq!(reopened.get(id).unwrap().status, "running");
}

#[test]
fn execution_sync_failures_leave_jobs_unclaimed_and_release_reservations() {
    // AC-0157: each failing sync boundary fails closed, preserves exact job
    // history/attempt metadata and releases the same inode for a durable retry.
    let expected = [
        SyncStep::File,
        SyncStep::Namespace,
        SyncStep::Root,
        SyncStep::AppData,
    ];
    for (failure_index, failure) in expected.into_iter().enumerate() {
        let (dir, mut store, locks) = fixture();
        let id = store.enqueue("noop").unwrap().id;
        let before = serde_json::to_value(store.get(id).unwrap()).unwrap();
        let plan = store.claim_plan(id, ClaimMode::StartQueued).unwrap();
        let observer = rusqlite::Connection::open(dir.path().join("state.db")).unwrap();
        let steps = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = steps.clone();
        *locks.storage.before_sync.lock().unwrap() = Some(Box::new(move |step| {
            assert_unclaimed(&observer, id);
            observed.lock().unwrap().push(step);
            if step == failure {
                Err(std::io::Error::other("injected storage sync failure"))
            } else {
                Ok(())
            }
        }));
        assert!(matches!(
            locks.try_reserve(plan.lock_target()),
            Err(JobTransitionError::LockUnavailable)
        ));
        assert_eq!(*steps.lock().unwrap(), expected[..=failure_index]);
        assert_eq!(
            serde_json::to_value(store.get(id).unwrap()).unwrap(),
            before
        );
        let observer = rusqlite::Connection::open(dir.path().join("state.db")).unwrap();
        assert_unclaimed(&observer, id);
        let entry = locks
            .storage
            .locks
            .symlink_metadata(id.to_string())
            .unwrap();
        *locks.storage.before_sync.lock().unwrap() = None;
        let reservation = locks.try_reserve(plan.lock_target()).unwrap();
        assert!(same_entry(
            &entry,
            &locks
                .storage
                .locks
                .symlink_metadata(id.to_string())
                .unwrap()
        ));
        let (_, execution) = store.claim_execution(&plan, reservation).unwrap();
        assert_eq!(execution.inner.generation, 1);
        assert_eq!(store.get(id).unwrap().status, "running");
    }
}

#[cfg(windows)]
#[test]
fn windows_execution_storage_requires_ntfs_and_real_flush() {
    // AC-0157: exercise the real handle query and Windows directory flushes,
    // including a fresh namespace and a retained existing lock file.
    let (dir, mut store, locks) = fixture();
    for directory in [
        &locks.storage.app,
        &locks.storage.root,
        &locks.storage.locks,
    ] {
        let name = windows::file_system_name(directory).unwrap();
        windows::require_ntfs(&name).unwrap();
    }
    let id = store.enqueue("noop").unwrap().id;
    let plan = store.claim_plan(id, ClaimMode::StartQueued).unwrap();
    let reservation = locks.try_reserve(plan.lock_target()).unwrap();
    let first = cap_std::fs::Metadata::from_file(&reservation.file).unwrap();
    assert_eq!(first.nlink(), 1);
    windows::require_ntfs(&windows::file_system_name(&reservation.file).unwrap()).unwrap();
    drop(reservation);
    let reservation = locks.try_reserve(plan.lock_target()).unwrap();
    let second = cap_std::fs::Metadata::from_file(&reservation.file).unwrap();
    assert!(same_entry(&first, &second));
    let (_, execution) = store.claim_execution(&plan, reservation).unwrap();
    assert_eq!(execution.inner.generation, 1);
    assert_eq!(
        JobStore::open(dir.path().join("state.db"))
            .unwrap()
            .get(id)
            .unwrap()
            .status,
        "running"
    );
}

#[cfg(windows)]
#[test]
fn windows_execution_storage_probe_failures_preserve_unclaimed_jobs() {
    // AC-0157: every exact handle must pass the capability query before its
    // flush and SQL claim. Query errors and successful non-NTFS replies both
    // release the reservation without rewriting history or replacing its file.
    fn name(value: &str) -> windows::FileSystemName {
        let mut buffer = [0; 261];
        for (slot, unit) in buffer.iter_mut().zip(value.encode_utf16()) {
            *slot = unit;
        }
        buffer
    }
    for rejected in [None, Some("FAT32"), Some("ReFS"), Some("unknown")] {
        for failure_index in 0..4 {
            let (dir, mut store, locks) = fixture();
            let id = store.enqueue("noop").unwrap().id;
            let before = serde_json::to_value(store.get(id).unwrap()).unwrap();
            let plan = store.claim_plan(id, ClaimMode::StartQueued).unwrap();
            let observer = rusqlite::Connection::open(dir.path().join("state.db")).unwrap();
            let probes = Arc::new(std::sync::Mutex::new(0));
            let observed = probes.clone();
            *locks.storage.volume_probe.lock().unwrap() = Some(Box::new(move || {
                assert_unclaimed(&observer, id);
                let mut count = observed.lock().unwrap();
                let index = *count;
                *count += 1;
                if index == failure_index {
                    rejected
                        .map(name)
                        .ok_or_else(|| std::io::Error::other("injected volume query failure"))
                } else {
                    Ok(name("NTFS"))
                }
            }));
            assert!(matches!(
                locks.try_reserve(plan.lock_target()),
                Err(JobTransitionError::LockUnavailable)
            ));
            assert_eq!(*probes.lock().unwrap(), failure_index + 1);
            assert_eq!(
                serde_json::to_value(store.get(id).unwrap()).unwrap(),
                before
            );
            let entry = locks
                .storage
                .locks
                .symlink_metadata(id.to_string())
                .unwrap();
            *locks.storage.volume_probe.lock().unwrap() = None;
            // This retry uses the real filesystem API and flush implementation.
            let reservation = locks.try_reserve(plan.lock_target()).unwrap();
            assert!(same_entry(
                &entry,
                &cap_std::fs::Metadata::from_file(&reservation.file).unwrap()
            ));
            let (_, execution) = store.claim_execution(&plan, reservation).unwrap();
            assert_eq!(execution.inner.generation, 1);
        }
    }
    // A malformed or unterminated name cannot pass an NTFS prefix comparison.
    for invalid in [name(""), name("NTFS-extra"), [b'N' as u16; 261]] {
        assert!(matches!(
            windows::require_ntfs(&invalid),
            Err(JobTransitionError::LockUnavailable)
        ));
    }
}

#[test]
fn execution_clones_hold_ownership_until_last_worker_exits() {
    // AC-0157: a detached blocking worker retains the original ownership guard.
    let (_dir, mut store, locks) = fixture();
    let execution = start(&mut store, &locks);
    let candidate = store.recovery_candidates().unwrap().pop().unwrap();
    let worker_execution = execution.clone();
    let (ready_send, ready_recv) = std::sync::mpsc::channel();
    let (exit_send, exit_recv) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        ready_send.send(()).unwrap();
        exit_recv.recv().unwrap();
        drop(worker_execution);
    });
    ready_recv.recv().unwrap();
    drop(execution);
    assert!(matches!(
        locks.try_reserve(candidate.lock_target()),
        Err(JobTransitionError::Busy)
    ));
    exit_send.send(()).unwrap();
    worker.join().unwrap();
    let reservation = locks.try_reserve(candidate.lock_target()).unwrap();
    assert!(
        store
            .recover_reserved(&candidate, reservation)
            .unwrap()
            .is_some()
    );
}

#[test]
fn foreign_missing_or_substituted_locks_never_grant_ownership() {
    // AC-0157 / AC-0159: missing previous-attempt storage is not death proof.
    let (dir, mut store, locks) = fixture();
    let execution = start(&mut store, &locks);
    let candidate = store.recovery_candidates().unwrap().pop().unwrap();
    let namespace = store.execution_namespace();
    let path = dir
        .path()
        .join("job-executions")
        .join(&namespace.value)
        .join(execution.id().to_string());
    let another_dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        JobExecutionLocks::open(another_dir.path(), namespace.clone()),
        Err(JobTransitionError::ForeignStore)
    ));
    let other = JobStore::open(dir.path().join("other.db")).unwrap();
    let other_locks = JobExecutionLocks::open(dir.path(), other.execution_namespace()).unwrap();
    assert!(matches!(
        other_locks.try_reserve(candidate.lock_target()),
        Err(JobTransitionError::ForeignStore)
    ));
    // Simulate external storage damage only after the real worker exits.
    drop(execution);
    std::fs::remove_file(&path).unwrap();
    assert!(matches!(
        locks.try_reserve(candidate.lock_target()),
        Err(JobTransitionError::LockUnavailable)
    ));
    assert!(!path.exists());
    assert_eq!(store.get(candidate.id()).unwrap().status, "running");
    // Replacing an anchored directory cannot relocate this manager's locks.
    let namespace_path = path.parent().unwrap();
    let renamed = namespace_path.with_extension("removed");
    #[cfg(not(windows))]
    {
        std::fs::rename(namespace_path, &renamed).unwrap();
        std::fs::create_dir(namespace_path).unwrap();
    }
    #[cfg(windows)]
    {
        // cap retains directory handles without FILE_SHARE_DELETE on Windows.
        // The OS prevents this substitution instead of allowing us to detect
        // it afterward; keep that stronger guarantee in the fixture.
        assert!(std::fs::rename(namespace_path, &renamed).is_err());
        assert!(!renamed.exists());
        locks.storage.verify().unwrap();
        let fresh = start(&mut store, &locks);
        assert!(matches!(
            store.check_execution(&fresh).unwrap(),
            ExecutionCheck::Running
        ));
    }
    assert!(matches!(
        locks.try_reserve(candidate.lock_target()),
        Err(JobTransitionError::LockUnavailable)
    ));
}

#[cfg(unix)]
#[test]
fn symlink_special_and_widened_lock_entries_fail_closed() {
    // AC-0157: no symlink/special-file lock fallback, including post-open changes.
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (dir, mut store, locks) = fixture();
    let id = store.enqueue("noop").unwrap().id;
    let plan = store.claim_plan(id, ClaimMode::StartQueued).unwrap();
    let namespace_dir = dir
        .path()
        .join("job-executions")
        .join(&store.execution_namespace().value);
    let lock_path = namespace_dir.join(id.to_string());
    let unrelated = dir.path().join("unrelated");
    std::fs::write(&unrelated, "untouched").unwrap();
    symlink(&unrelated, &lock_path).unwrap();
    assert!(matches!(
        locks.try_reserve(plan.lock_target()),
        Err(JobTransitionError::LockUnavailable)
    ));
    assert_eq!(std::fs::read_to_string(&unrelated).unwrap(), "untouched");
    std::fs::remove_file(&lock_path).unwrap();
    let socket_path = dir.path().join("socket");
    let socket = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    std::fs::rename(&socket_path, &lock_path).unwrap();
    assert!(matches!(
        locks.try_reserve(plan.lock_target()),
        Err(JobTransitionError::LockUnavailable)
    ));
    drop(socket);
    std::fs::remove_file(&lock_path).unwrap();
    let reservation = locks.try_reserve(plan.lock_target()).unwrap();
    let (_, execution) = store.claim_execution(&plan, reservation).unwrap();
    std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        store.check_execution(&execution),
        Err(JobTransitionError::LockUnavailable)
    ));
    std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        store.check_execution(&execution).unwrap(),
        ExecutionCheck::Running
    );
    std::fs::set_permissions(&namespace_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        store.check_execution(&execution),
        Err(JobTransitionError::LockUnavailable)
    ));
    std::fs::set_permissions(&namespace_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        store.check_execution(&execution).unwrap(),
        ExecutionCheck::Running
    );
}

/// The subprocess uses this same test binary, with an explicit stdin/stdout
/// handshake. Normal suite invocation returns immediately without external work.
#[test]
fn execution_owner_child() {
    let Some(path) = std::env::var_os("CARTOGRAPH_JOB_OWNER_TEST_ROOT") else {
        return;
    };
    let path = PathBuf::from(path);
    let id: i64 = std::env::var("CARTOGRAPH_JOB_OWNER_TEST_ID")
        .unwrap()
        .parse()
        .unwrap();
    let mut store = JobStore::open(path.join("state.db")).unwrap();
    let locks = JobExecutionLocks::open(&path, store.execution_namespace()).unwrap();
    let plan = store.claim_plan(id, ClaimMode::StartQueued).unwrap();
    let reservation = locks.try_reserve(plan.lock_target()).unwrap();
    let (_, _execution) = store.claim_execution(&plan, reservation).unwrap();
    println!("CARTOGRAPH_JOB_OWNERSHIP_READY");
    std::io::stdout().flush().unwrap();
    let mut command = String::new();
    std::io::stdin().read_line(&mut command).unwrap();
    assert_eq!(command.trim(), "exit");
    // No Rust destructor runs: the OS, not cooperative guard Drop, releases it.
    std::process::exit(17);
}

struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn execution_locks_survive_process_exit_without_stealing_live_work() {
    // AC-0157 / AC-0159: a real second process is live until confirmed exit.
    let (dir, mut store, locks) = fixture();
    let id = store.enqueue("noop").unwrap().id;
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "job_execution::tests::execution_owner_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("CARTOGRAPH_JOB_OWNER_TEST_ROOT", dir.path())
        .env("CARTOGRAPH_JOB_OWNER_TEST_ID", id.to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    let mut child = ChildGuard(child);
    let mut stdout = std::io::BufReader::new(child.0.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        assert_ne!(
            stdout.read_line(&mut line).unwrap(),
            0,
            "owner child exited before claim"
        );
        if line.contains("CARTOGRAPH_JOB_OWNERSHIP_READY") {
            break;
        }
    }
    let mut startup = JobStore::open(dir.path().join("state.db")).unwrap();
    let candidate = startup.recovery_candidates().unwrap().pop().unwrap();
    assert_eq!(candidate.id(), id);
    assert!(matches!(
        locks.try_reserve(candidate.lock_target()),
        Err(JobTransitionError::Busy)
    ));
    assert_eq!(startup.get(id).unwrap().status, "running");
    child.0.stdin.take().unwrap().write_all(b"exit\n").unwrap();
    assert_eq!(child.0.wait().unwrap().code(), Some(17));
    let reservation = locks.try_reserve(candidate.lock_target()).unwrap();
    let recovered = startup
        .recover_reserved(&candidate, reservation)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.id, id);
    assert_eq!(recovered.status, "interrupted");
}
