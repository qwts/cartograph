use super::*;
use std::sync::{Arc, Barrier};

fn directory(parent: &Path, relative: &str) -> PathBuf {
    let path = parent.join(relative);
    std::fs::create_dir_all(&path).unwrap();
    crate::paths::canonicalize(path).unwrap()
}

fn file_url(path: &Path) -> String {
    let normalized = path.to_str().unwrap().replace('\\', "/");
    let mut url = if normalized.starts_with('/') {
        "file://".to_string()
    } else {
        "file:///".to_string()
    };
    for byte in normalized.bytes() {
        if byte.is_ascii_alphanumeric() || b"/:-._~".contains(&byte) {
            url.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(&mut url, "%{byte:02X}").unwrap();
        }
    }
    url
}

// AC0140: same-basename roots and worktree-like independent directories retain
// separate namespaces; canonical aliases and restart retain one registration.
#[test]
fn source_registration_is_durable_and_distinguishes_roots() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let state = app_data.join("state.db");
    let first_root = directory(dir.path(), "one/project");
    let second_root = directory(dir.path(), "two/project");
    std::fs::write(first_root.join("same.ts"), "export const value = 1;").unwrap();
    std::fs::write(second_root.join("same.ts"), "export const value = 1;").unwrap();
    let mut registry = SourceRegistry::open(&state, &app_data).unwrap();
    let first = registry.register_local(&first_root).unwrap();
    let second = registry.register_local(&second_root).unwrap();
    assert_ne!(first.source_id, second.source_id);
    assert_ne!(first.repo_key, second.repo_key);
    assert_eq!(first.display_name, second.display_name);
    assert_eq!(first.repo_key, format!("local/{}", first.source_id));
    assert!(first.is_ready());
    assert!(!first.is_managed());
    assert!(first.clone_url().is_none());
    assert!(first.managed().unwrap().is_none());
    assert_eq!(
        registry
            .register_local(&first_root.join("."))
            .unwrap()
            .source_id,
        first.source_id
    );
    #[cfg(unix)]
    {
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&first_root, &alias).unwrap();
        assert_eq!(
            registry.register_local(&alias).unwrap().source_id,
            first.source_id
        );
    }
    let installation = registry.registry_id.clone();
    drop(registry);
    let mut reopened = SourceRegistry::open(&state, &app_data).unwrap();
    assert_eq!(reopened.registry_id, installation);
    assert_eq!(
        reopened.register_local(&first_root).unwrap().source_id,
        first.source_id
    );
    assert_eq!(
        reopened
            .get_by_repo(&second.repo_key)
            .unwrap()
            .unwrap()
            .root(),
        second_root
    );
    let rows = reopened.list().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.windows(2)
            .all(|pair| pair[0].source_id < pair[1].source_id)
    );
    // No target metadata, ID markers or Git metadata were created.
    assert_eq!(std::fs::read_dir(first_root).unwrap().count(), 1);
    assert_eq!(std::fs::read_dir(second_root).unwrap().count(), 1);
}

// AC0140: two independently opened connections race on the same canonical root.
#[test]
fn concurrent_registration_converges_without_merging_distinct_roots() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "source");
    let state = app_data.join("state.db");
    let mut first = SourceRegistry::open(&state, &app_data).unwrap();
    let mut second = SourceRegistry::open(&state, &app_data).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let other_barrier = Arc::clone(&barrier);
    let other_root = root.clone();
    let thread = std::thread::spawn(move || {
        other_barrier.wait();
        second.register_local(&other_root).unwrap()
    });
    barrier.wait();
    let one = first.register_local(&root).unwrap();
    let two = thread.join().unwrap();
    assert_eq!(one.source_id, two.source_id);
    assert_eq!(one.repo_key, two.repo_key);
    assert_eq!(first.list().unwrap().len(), 1);
}

// AC0140, AC0141: managed file origins retain full-path identity, GitHub forms
// share canonical identity, and direct local registration stays a separate lane.
#[test]
fn managed_origins_and_checkout_bindings_survive_missing_mirror_restart() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let state = app_data.join("state.db");
    let first_origin = directory(dir.path(), "one/project.git");
    let second_origin = directory(dir.path(), "two/project.git");
    let mut registry = SourceRegistry::open(&state, &app_data).unwrap();
    let first = registry.reserve_managed(&file_url(&first_origin)).unwrap();
    let second = registry.reserve_managed(&file_url(&second_origin)).unwrap();
    let direct = registry.register_local(&first_origin).unwrap();
    assert_ne!(first.source_id, second.source_id);
    assert_ne!(first.source_id, direct.source_id);
    assert_ne!(first.root(), first_origin);
    assert_eq!(first.repo_key, format!("local/{}", first.source_id));
    assert_eq!(
        first.root(),
        app_data
            .join("sources")
            .join(&first.source_id)
            .join("checkout")
    );
    assert!(!first.is_ready());
    assert!(!first.root().exists());
    assert_eq!(first.managed().unwrap().unwrap().root(), first.root());
    assert_eq!(
        registry
            .reserve_managed(&file_url(&first_origin))
            .unwrap()
            .source_id,
        first.source_id
    );
    let github = registry
        .reserve_managed("https://github.com/Owner/Project.git")
        .unwrap();
    let ssh = registry
        .reserve_managed("git@github.com:owner/project.git")
        .unwrap();
    assert_eq!(github.source_id, ssh.source_id);
    assert_eq!(github.repo_key, "owner/project");
    assert_eq!(
        github.clone_url(),
        Some("https://github.com/owner/project.git")
    );
    // This fixture models a caller publishing validated bytes. is_ready is only
    // the operational check; actual source readers additionally hold owner guards.
    std::fs::create_dir_all(first.root()).unwrap();
    registry.set_ready(&first.source_id, true).unwrap();
    let by_local = registry.register_local(first.root()).unwrap();
    assert_eq!(by_local.source_id, first.source_id);
    assert!(by_local.is_managed());
    assert!(by_local.is_ready());
    registry.set_ready(&first.source_id, false).unwrap();
    std::fs::remove_dir_all(&first_origin).unwrap();
    drop(registry);
    let reopened = SourceRegistry::open(&state, &app_data).unwrap();
    let retained = reopened.get_by_id(&first.source_id).unwrap().unwrap();
    assert_eq!(retained.clone_url(), first.clone_url());
    assert_eq!(retained.root(), first.root());
    assert!(!retained.is_ready()); // Existing checkout alone cannot revive it.
    assert!(
        !reopened
            .get_by_id(&direct.source_id)
            .unwrap()
            .unwrap()
            .is_ready()
    );
    assert_eq!(
        reopened
            .get_by_root(first.root())
            .unwrap()
            .unwrap()
            .source_id,
        first.source_id
    );
}

// AC0140, AC0143: missing bindings remain durable, moves get a new identity,
// and a symlink substitution never silently redirects a direct source read.
#[test]
fn missing_moved_and_substituted_roots_never_change_stored_identity() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let state = app_data.join("state.db");
    let root = directory(dir.path(), "source");
    let moved = dir.path().join("moved");
    let mut registry = SourceRegistry::open(&state, &app_data).unwrap();
    let source = registry.register_local(&root).unwrap();
    std::fs::rename(&root, &moved).unwrap();
    let unavailable = registry.get_by_id(&source.source_id).unwrap().unwrap();
    assert_eq!(unavailable.root(), root);
    assert!(!unavailable.is_ready());
    assert!(registry.register_local(&root).is_err());
    assert_ne!(
        registry.register_local(&moved).unwrap().source_id,
        source.source_id
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&moved, &root).unwrap();
        assert!(
            !registry
                .get_by_id(&source.source_id)
                .unwrap()
                .unwrap()
                .is_ready()
        );
        std::fs::remove_file(&root).unwrap();
    }
    // Same-path replacement intentionally retains logical identity, with no
    // claim that it is the same physical directory or contains the same bytes.
    std::fs::create_dir(&root).unwrap();
    assert_eq!(
        registry.register_local(&root).unwrap().source_id,
        source.source_id
    );
    assert!(
        registry
            .get_by_id(&source.source_id)
            .unwrap()
            .unwrap()
            .is_ready()
    );
    assert!(registry.get_by_repo("local/unknown").unwrap().is_none());
    assert!(
        registry
            .get_by_id("src_00000000000000000000000000000000")
            .unwrap()
            .is_none()
    );
}

// AC0146: first initialization retires only known generated preflight findings;
// its marker and removal commit together, never deleting fresh rows on reopen.
#[test]
fn findings_retirement_is_selective_atomic_and_runs_only_once() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let state = app_data.join("state.db");
    let mut findings = crate::findings::FindingStore::open(&state).unwrap();
    let fixture = |detector: &'static str| crate::findings::NewFinding {
        kind: "unsupported",
        detector,
        path: "a.ts",
        line: 1,
        message: "fixture",
    };
    findings
        .replace_for(
            "local/legacy",
            ingest::preflight::DETECTOR_ID,
            &[fixture(ingest::preflight::DETECTOR_ID)],
        )
        .unwrap();
    findings
        .replace_for("local/legacy", "custom@1", &[fixture("custom@1")])
        .unwrap();
    let state_conn = Connection::open(&state).unwrap();
    state_conn.execute_batch("PRAGMA user_version=73; CREATE TABLE unrelated(payload TEXT); INSERT INTO unrelated VALUES ('preserve');").unwrap();
    // A trigger could alter unrelated tables during deletion. Refusing it must
    // roll back the private registry tables as well as the retirement attempt.
    state_conn.execute_batch("CREATE TRIGGER legacy_side_effect AFTER DELETE ON findings BEGIN DELETE FROM unrelated; END;").unwrap();
    assert!(SourceRegistry::open(&state, &app_data).is_err());
    assert_eq!(findings.list().unwrap().len(), 2);
    assert_eq!(
        state_conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='source_registry_meta'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    state_conn
        .execute_batch("DROP TRIGGER legacy_side_effect")
        .unwrap();
    let registry = SourceRegistry::open(&state, &app_data).unwrap();
    let rows = findings.list().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].detector, "custom@1");
    findings
        .replace_for(
            "local/new",
            ingest::preflight::DETECTOR_ID,
            &[fixture(ingest::preflight::DETECTOR_ID)],
        )
        .unwrap();
    drop(registry);
    SourceRegistry::open(&state, &app_data).unwrap();
    assert_eq!(findings.list().unwrap().len(), 2);
    assert_eq!(
        state_conn
            .query_row("SELECT payload FROM unrelated", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "preserve"
    );
    assert_eq!(
        state_conn
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        73
    );
}

// AC0140, AC0146: unsupported versions, malformed associations and modified
// private schema fail closed on an existing handle as well as across restart.
#[test]
fn corrupt_registry_versions_bindings_and_schema_fail_closed() {
    for mutation in [
        "PRAGMA ignore_check_constraints=ON; UPDATE source_registry_meta SET schema_version=2",
        "UPDATE source_registry_meta SET registry_id='reg_00000000000000000000000000000000'",
        "UPDATE registered_sources SET source_id='src_NOT_A_VALID_IDENTIFIER'",
        "UPDATE registered_sources SET repo_key='local/wrong'",
        "UPDATE registered_sources SET root='../outside'",
        "UPDATE registered_sources SET display_name=CAST(zeroblob(5000) AS TEXT)",
        "ALTER TABLE registered_sources ADD COLUMN hidden TEXT GENERATED ALWAYS AS ('hidden') VIRTUAL",
        "CREATE TRIGGER altered_publication AFTER UPDATE ON registered_sources BEGIN UPDATE registered_sources SET ready=0; END",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let app_data = directory(dir.path(), "private");
        let state = app_data.join("state.db");
        let root = directory(dir.path(), "source");
        let mut registry = SourceRegistry::open(&state, &app_data).unwrap();
        registry.register_local(&root).unwrap();
        let conn = Connection::open(&state).unwrap();
        conn.execute_batch(mutation).unwrap();
        let error = registry
            .list()
            .expect_err("existing handle must reject corruption");
        assert!(!error.contains("outside"));
        assert!(!error.contains("NOT_A_VALID"));
        // A well-formed changed installation ID is only detectable against the
        // pinned live handle; it is not a cryptographic tamper-proof signature.
        if !mutation.contains("registry_id='reg_") {
            assert!(
                SourceRegistry::open(&state, &app_data).is_err(),
                "{mutation}"
            );
        }
    }
}

// AC0140: managed source bindings cannot be changed to another checkout or
// inconsistent origin through a corrupt row, even if the target exists.
#[test]
fn corrupt_managed_origin_and_root_are_rejected_without_raw_error_echo() {
    for field in ["root", "origin_key", "clone_url", "github_repo"] {
        let dir = tempfile::tempdir().unwrap();
        let app_data = directory(dir.path(), "private");
        let state = app_data.join("state.db");
        let other = directory(dir.path(), "other");
        let mut registry = SourceRegistry::open(&state, &app_data).unwrap();
        let source = registry
            .reserve_managed("https://github.com/owner/repo")
            .unwrap();
        let malicious = if field == "root" {
            other.to_str().unwrap()
        } else {
            "PRIVATE_CORRUPT_ORIGIN"
        };
        let conn = Connection::open(&state).unwrap();
        conn.execute(
            &format!("UPDATE registered_sources SET {field}=?1"),
            [malicious],
        )
        .unwrap();
        let error = registry.get_by_id(&source.source_id).err().unwrap();
        assert!(!error.contains(malicious));
        assert!(SourceRegistry::open(&state, &app_data).is_err());
    }
}

// AC0145: job kinds bind opaque source IDs and reject historical path retry
// without looking up, creating, or mutating any registration or job row.
#[test]
fn source_job_kinds_bind_ids_and_refuse_legacy_paths() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "source");
    let mut registry = SourceRegistry::open(app_data.join("state.db"), &app_data).unwrap();
    let source = registry.register_local(&root).unwrap();
    assert_eq!(
        source_id_from_ingest_job_kind(&source.ingest_job_kind()).unwrap(),
        source.source_id
    );
    let legacy = source_id_from_ingest_job_kind("ingest:/PRIVATE_PATH").unwrap_err();
    assert!(legacy.contains("re-run ingestion"));
    assert!(!legacy.contains("PRIVATE_PATH"));
    for bad in [
        "ingest-source-v1:",
        "ingest-source-v1:src_BAD",
        "ingest-source-v1:src_ABCDEF00000000000000000000000000",
        "add-repo:owner/repo",
        "add-system:system",
    ] {
        assert!(source_id_from_ingest_job_kind(bad).is_err());
    }
    assert_eq!(registry.list().unwrap().len(), 1);
}

// AC0140: unsupported/credential-bearing and oversized inputs produce fixed
// errors, and unsupported path representations cannot collapse through lossiness.
#[test]
fn registry_rejects_invalid_inputs_without_creating_registrations() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let mut registry = SourceRegistry::open(app_data.join("state.db"), &app_data).unwrap();
    for url in [
        "https://PRIVATE_TOKEN@github.com/owner/repo",
        "https://github.com/owner/repo?secret=PRIVATE_TOKEN",
        "https://example.com/owner/repo",
        "file://remotehost/PRIVATE_TOKEN",
    ] {
        let error = registry.reserve_managed(url).unwrap_err();
        assert!(!error.contains("PRIVATE_TOKEN"));
    }
    assert!(
        registry
            .reserve_managed(&"a".repeat(MAX_ORIGIN_BYTES + 1))
            .is_err()
    );
    assert!(registry.get_by_id("../outside").is_err());
    assert!(registry.get_by_root(Path::new("relative")).is_err());
    let file = dir.path().join("file");
    std::fs::write(&file, "not a directory").unwrap();
    assert!(registry.register_local(&file).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let non_utf8 = dir
            .path()
            .join(std::ffi::OsString::from_vec(vec![b'r', 0xff]));
        std::fs::create_dir(&non_utf8).unwrap();
        assert!(registry.register_local(&non_utf8).is_err());
    }
    assert!(registry.list().unwrap().is_empty());
}
