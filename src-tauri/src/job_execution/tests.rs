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
    std::fs::rename(namespace_path, &renamed).unwrap();
    std::fs::create_dir(namespace_path).unwrap();
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
