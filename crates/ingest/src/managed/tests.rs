use super::*;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

const REGISTRY: &str = "reg_11111111111111111111111111111111";
const FIRST: &str = "src_22222222222222222222222222222222";
const SECOND: &str = "src_33333333333333333333333333333333";

/// Lock lifetime assertions need control of every process that can inherit an
/// open handle. Parallel libtest fixtures can spawn/fork while another fixture
/// holds a lock: CLOEXEC closes that inherited handle only when exec occurs.
/// Open this fixture's locks after exec in an otherwise isolated test process.
fn run_in_isolated_process(test_name: &str) -> bool {
    const ISOLATED_CASE: &str = "CARTOGRAPH_MANAGED_ISOLATED_TEST_CASE";
    let exact = format!("managed::tests::{test_name}");
    if std::env::var(ISOLATED_CASE).is_ok_and(|value| value == exact) {
        return false;
    }
    let mut child = ChildCleanup(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &exact, "--nocapture", "--test-threads=1"])
            .env(ISOLATED_CASE, &exact)
            .env_remove("CARTOGRAPH_MANAGED_LOCK_TEST_CHILD_PATH")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    // Drain both pipes concurrently so diagnostic volume cannot block a child.
    // The owning guard kills/waits on any parent-side panic or I/O failure.
    let stdout = child.0.stdout.take().unwrap();
    let stderr = child.0.stderr.take().unwrap();
    let stdout = std::thread::spawn(move || collect_output(stdout));
    let stderr = std::thread::spawn(move || collect_output(stderr));
    let status = child.0.wait().unwrap();
    let stdout = stdout.join().unwrap().unwrap();
    let stderr = stderr.join().unwrap().unwrap();
    assert!(
        status.success(),
        "isolated {exact} failed:\n{}\n{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr),
    );
    true
}

fn collect_output(mut pipe: impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn mirror(path: &Path, source: &[u8]) -> ManagedOrigin {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let repo = git2::Repository::init_bare(path).unwrap();
    let blob = repo.blob(source).unwrap();
    let mut builder = repo.treebuilder(None).unwrap();
    builder.insert("app.ts", blob, 0o100644).unwrap();
    let tree_id = builder.write().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let signature =
        git2::Signature::new("fixture", "fixture@example.invalid", &git2::Time::new(1, 0)).unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
        .unwrap();
    parse_managed_origin(Url::from_file_path(path).unwrap().as_str()).unwrap()
}

fn app_dir(root: &Path) -> PathBuf {
    let path = root.join("application");
    fs::create_dir(&path).unwrap();
    dunce::canonicalize(path).unwrap()
}

#[test]
fn managed_clone_slots_isolate_same_named_mirrors() {
    if run_in_isolated_process("managed_clone_slots_isolate_same_named_mirrors") {
        return;
    }
    // AC-0141: reserve destination identity before cloning; equal display names
    // cannot replace one another or become the recovered repository namespace.
    let dir = tempfile::tempdir().unwrap();
    let first_origin = mirror(
        &dir.path().join("one/mirror.git"),
        b"export const one = 1;\n",
    );
    let second_origin = mirror(
        &dir.path().join("two/mirror.git"),
        b"export const two = 2;\n",
    );
    assert_eq!(first_origin.display_name, second_origin.display_name);
    assert_ne!(first_origin.key, second_origin.key);
    let app = app_dir(dir.path());
    let first = ManagedCheckout::new(&app, REGISTRY, FIRST).unwrap();
    let second = ManagedCheckout::new(&app, REGISTRY, SECOND).unwrap();
    assert!(!app.join("sources").exists(), "derivation is metadata only");
    let mut first_use = first.try_write().unwrap();
    let a = first_use.clone_from(&first_origin, None).unwrap();
    let mut second_use = second.try_write().unwrap();
    let b = second_use.clone_from(&second_origin, None).unwrap();
    assert_ne!(a.path, b.path);
    assert_ne!(a.repo, b.repo);
    assert_eq!(a.repo, format!("local/{FIRST}"));
    assert_eq!(b.repo, format!("local/{SECOND}"));
    assert_eq!(
        fs::read(a.path.join("app.ts")).unwrap(),
        b"export const one = 1;\n"
    );
    assert_eq!(
        fs::read(b.path.join("app.ts")).unwrap(),
        b"export const two = 2;\n"
    );
    let repeated = first_use.clone_from(&first_origin, None).unwrap();
    assert_eq!(repeated.commit_sha, a.commit_sha);
    assert_eq!(
        fs::read(b.path.join("app.ts")).unwrap(),
        b"export const two = 2;\n"
    );
    assert!(!a.path.join("owner.json").exists());
    assert!(
        !a.path.join(".cartograph").exists(),
        "no target identity marker"
    );
    assert!(a.path.parent().unwrap().join("owner.json").is_file());
}

#[test]
fn managed_source_guards_exclude_replacement_until_all_readers_release() {
    if run_in_isolated_process(
        "managed_source_guards_exclude_replacement_until_all_readers_release",
    ) {
        return;
    }
    // AC-0147: clone completion does not drop exclusive use; later parser and
    // enrichment reads stay protected. Independent shared readers coexist.
    // This process opens every lock after exec and spawns no children, so each
    // explicit drop below closes the last handle for that guard's file description.
    let dir = tempfile::tempdir().unwrap();
    let origin = mirror(&dir.path().join("mirror.git"), b"export const value = 1;\n");
    let app = app_dir(dir.path());
    let checkout = ManagedCheckout::new(&app, REGISTRY, FIRST).unwrap();
    let mut writer = checkout.try_write().unwrap();
    writer.clone_from(&origin, None).unwrap();
    assert_eq!(writer.source_id(), FIRST);
    assert!(matches!(checkout.try_read(), Err(IngestError::SourceBusy)));
    assert!(matches!(checkout.try_write(), Err(IngestError::SourceBusy)));
    assert_eq!(
        fs::read(writer.root().join("app.ts")).unwrap(),
        b"export const value = 1;\n"
    );
    drop(writer);
    let one = checkout.try_read().unwrap();
    let two = ManagedCheckout::new(&app, REGISTRY, FIRST)
        .unwrap()
        .try_read()
        .unwrap();
    assert_eq!(one.source_id(), FIRST);
    assert_eq!(one.root(), two.root());
    assert!(matches!(checkout.try_write(), Err(IngestError::SourceBusy)));
    drop(one);
    assert!(matches!(checkout.try_write(), Err(IngestError::SourceBusy)));
    drop(two);
    let reacquired = checkout.try_write().unwrap();
    assert_eq!(reacquired.root(), checkout.root());
    assert!(
        app.join("source-locks")
            .join(format!("{FIRST}.lock"))
            .is_file()
    );
}

struct ChildCleanup(Child);

impl Drop for ChildCleanup {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn managed_source_lock_process_exit_releases_ownership() {
    if run_in_isolated_process("managed_source_lock_process_exit_releases_ownership") {
        return;
    }
    // AC-0147: a real independent process holds the lock until an explicit
    // handshake permits exit. process::exit skips Rust Drop and tests OS release.
    const CHILD_PATH: &str = "CARTOGRAPH_MANAGED_LOCK_TEST_CHILD_PATH";
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        let checkout = ManagedCheckout::new(Path::new(&path), REGISTRY, FIRST).unwrap();
        let _guard = checkout.try_write().unwrap();
        println!("MANAGED_LOCK_READY");
        std::io::stdout().flush().unwrap();
        let mut instruction = [0];
        std::io::stdin().read_exact(&mut instruction).unwrap();
        assert_eq!(instruction, [b'x']);
        std::process::exit(0);
    }
    let dir = tempfile::tempdir().unwrap();
    let origin = mirror(&dir.path().join("mirror.git"), b"export const value = 1;\n");
    let app = app_dir(dir.path());
    let checkout = ManagedCheckout::new(&app, REGISTRY, FIRST).unwrap();
    checkout
        .try_write()
        .unwrap()
        .clone_from(&origin, None)
        .unwrap();
    // The isolated controller has released its setup guard before spawning the
    // worker. The worker then opens its own lock after exec; no other fixture can
    // inherit either process's source lock while these assertions run.
    let mut child = ChildCleanup(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "managed::tests::managed_source_lock_process_exit_releases_ownership",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_PATH, &app)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let mut output = BufReader::new(child.0.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        assert_ne!(
            output.read_line(&mut line).unwrap(),
            0,
            "child exited before acquiring lock"
        );
        if line.contains("MANAGED_LOCK_READY") {
            break;
        }
    }
    assert!(matches!(checkout.try_read(), Err(IngestError::SourceBusy)));
    assert!(matches!(checkout.try_write(), Err(IngestError::SourceBusy)));
    child.0.stdin.take().unwrap().write_all(b"x").unwrap();
    assert!(child.0.wait().unwrap().success());
    let read = checkout.try_read().unwrap();
    assert_eq!(
        fs::read(read.root().join("app.ts")).unwrap(),
        b"export const value = 1;\n"
    );
    // This layer deliberately has no API that can mark registry availability ready.
}

#[test]
fn managed_origins_normalize_urls_and_validate_missing_stored_mirrors() {
    // AC-0141: all supported aliases share a key; encoded local paths preserve
    // exact identity, while restoring metadata does not require a present mirror.
    let expected = parse_managed_origin("acme/shop").unwrap();
    for value in [
        "https://GitHub.com/Acme/Shop.git",
        "git@GitHub.com:ACME/SHOP.GIT",
        "Acme/Shop/",
    ] {
        assert_eq!(parse_managed_origin(value).unwrap(), expected);
    }
    assert_eq!(expected.key, "github:acme/shop");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("space # percent% ü.git");
    fs::create_dir(&path).unwrap();
    let file_url = Url::from_file_path(&path).unwrap();
    let origin = parse_managed_origin(file_url.as_str()).unwrap();
    assert_eq!(origin.display_name, "space # percent% ü");
    assert!(origin.clone_url.contains("%23"));
    assert!(origin.clone_url.contains("%25"));
    assert_eq!(origin.key, origin.clone_url);
    fs::remove_dir(&path).unwrap();
    origin.validate().unwrap();
    assert!(parse_managed_origin(&origin.clone_url).is_err());
    let mut corrupt = origin;
    corrupt.key.push_str("-changed");
    assert!(matches!(
        corrupt.validate(),
        Err(IngestError::InvalidManagedOrigin)
    ));
    for invalid in [
        "https://token@github.com/acme/shop",
        "https://github.com/acme/shop?token=secret",
        "https://github.com/acme/shop#fragment",
        "https://gitlab.com/acme/shop",
        "file://remote-host/tmp/repo.git",
        "file:///tmp/repo.git?token=secret",
        "file:///tmp/repo.git#fragment",
        "file://user:password@localhost/tmp/repo.git",
        "acme/shop/extra",
        "https://github.com/acme/sh\nop",
    ] {
        let error = parse_managed_origin(invalid).unwrap_err();
        assert!(
            matches!(error, IngestError::InvalidManagedOrigin),
            "{invalid}"
        );
        assert_eq!(error.to_string(), "invalid managed source origin");
    }
}

#[test]
fn managed_failed_clone_preserves_previous_checkout_and_other_attempts() {
    if run_in_isolated_process(
        "managed_failed_clone_preserves_previous_checkout_and_other_attempts",
    ) {
        return;
    }
    // AC-0141 / AC-0003: failure cleans only this attempt and never removes the
    // prior successful checkout or another attempt's retained recovery data.
    let dir = tempfile::tempdir().unwrap();
    let origin = mirror(
        &dir.path().join("mirror.git"),
        b"export const previous = 1;\n",
    );
    let not_git = dir.path().join("not-git");
    fs::create_dir(&not_git).unwrap();
    let invalid = parse_managed_origin(Url::from_file_path(&not_git).unwrap().as_str()).unwrap();
    let app = app_dir(dir.path());
    let checkout = ManagedCheckout::new(&app, REGISTRY, FIRST).unwrap();
    let mut guard = checkout.try_write().unwrap();
    let previous = guard.clone_from(&origin, None).unwrap();
    let orphan = checkout.slot.join(".attempt-retained");
    create_private_directory(&orphan).unwrap();
    fs::write(orphan.join("keep"), "retained after another process exited").unwrap();
    assert!(guard.clone_from(&invalid, None).is_err());
    assert_eq!(
        fs::read(previous.path.join("app.ts")).unwrap(),
        b"export const previous = 1;\n"
    );
    assert!(orphan.join("keep").is_file());
    let entries: Vec<_> = fs::read_dir(&checkout.slot)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(
        entries.len(),
        3,
        "owner, checkout and untouched orphan only"
    );
    assert!(matches!(checkout.try_write(), Err(IngestError::SourceBusy)));
}

#[test]
fn managed_slots_reject_unowned_existing_destinations_and_wrong_registry() {
    if run_in_isolated_process(
        "managed_slots_reject_unowned_existing_destinations_and_wrong_registry",
    ) {
        return;
    }
    // AC-0141: an existing directory is not authorization to delete its contents.
    let dir = tempfile::tempdir().unwrap();
    let app = app_dir(dir.path());
    let checkout = ManagedCheckout::new(&app, REGISTRY, FIRST).unwrap();
    ensure_private_directory(&app.join("sources")).unwrap();
    create_private_directory(&checkout.slot).unwrap();
    fs::write(checkout.slot.join("keep"), "unowned content").unwrap();
    assert!(matches!(
        checkout.try_write(),
        Err(IngestError::SourceOwnership)
    ));
    assert_eq!(
        fs::read_to_string(checkout.slot.join("keep")).unwrap(),
        "unowned content"
    );

    let owned = ManagedCheckout::new(&app, REGISTRY, SECOND).unwrap();
    drop(owned.try_write().unwrap());
    let other_registry =
        ManagedCheckout::new(&app, "reg_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", SECOND).unwrap();
    assert!(matches!(
        other_registry.try_write(),
        Err(IngestError::SourceOwnership)
    ));
    create_private_directory(owned.root()).unwrap();
    fs::write(owned.root().join("keep"), "not a managed checkout").unwrap();
    assert!(matches!(
        owned.try_write(),
        Err(IngestError::SourceOwnership)
    ));
    assert!(owned.root().join("keep").is_file());
    for id in [
        "../escape",
        "src_short",
        "src_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        assert!(matches!(
            ManagedCheckout::new(&app, REGISTRY, id),
            Err(IngestError::InvalidManagedIdentity)
        ));
    }
}

#[test]
fn managed_slot_initialization_failure_leaves_final_slot_unpublished_and_retryable() {
    if run_in_isolated_process(
        "managed_slot_initialization_failure_leaves_final_slot_unpublished_and_retryable",
    ) {
        return;
    }
    // AC-0141: a failed ownership publication cannot poison the reserved slot.
    // Retry initializes it normally, without deleting another attempt's data or
    // treating a newly appeared empty destination as permission to overwrite it.
    let dir = tempfile::tempdir().unwrap();
    let app = app_dir(dir.path());
    let checkout = ManagedCheckout::new(&app, REGISTRY, FIRST).unwrap();
    let parent = app.join("sources");
    ensure_private_directory(&parent).unwrap();
    let orphan = parent.join(".attempt-retained-from-another-process");
    create_private_directory(&orphan).unwrap();
    fs::write(orphan.join("keep"), "retained initialization data").unwrap();
    let guard = checkout.acquire_lock(true).unwrap();
    let failed = checkout.initialize_slot_before_publish(|attempt| {
        assert!(!checkout.slot.exists());
        let owner: SlotOwner =
            serde_json::from_slice(&fs::read(attempt.join("owner.json")).unwrap()).unwrap();
        assert_eq!(
            owner,
            checkout.owner(),
            "stage is complete before publication"
        );
        Err(std::io::Error::other("injected initialization failure").into())
    });
    assert!(matches!(failed, Err(IngestError::Io(_))));
    assert!(!checkout.slot.exists());
    assert_eq!(
        fs::read_dir(&parent).unwrap().count(),
        1,
        "only earlier orphan remains"
    );
    drop(guard);
    drop(checkout.try_write().unwrap());
    checkout.validate_slot().unwrap();
    assert_eq!(
        fs::read_to_string(orphan.join("keep")).unwrap(),
        "retained initialization data"
    );

    let raced = ManagedCheckout::new(&app, REGISTRY, SECOND).unwrap();
    let _guard = raced.acquire_lock(true).unwrap();
    let result = raced.initialize_slot_before_publish(|_| {
        // A destination appearing before publication is not one we created.
        create_private_directory(&raced.slot)?;
        Ok(())
    });
    assert!(matches!(result, Err(IngestError::SourceOwnership)));
    assert!(raced.slot.is_dir());
    assert_eq!(fs::read_dir(&raced.slot).unwrap().count(), 0);
    assert!(!raced.slot.join("owner.json").exists());
    assert_eq!(
        fs::read_dir(&parent).unwrap().count(),
        3,
        "no failed attempt remains"
    );
}

#[cfg(unix)]
#[test]
fn managed_slots_reject_symlink_substitutions_without_touching_targets() {
    if run_in_isolated_process(
        "managed_slots_reject_symlink_substitutions_without_touching_targets",
    ) {
        return;
    }
    // AC-0141: lock parents, ownership metadata and checkout substitution fail
    // before any cleanup or overwrite can follow an external filesystem target.
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let app = app_dir(dir.path());
    let external = dir.path().join("external");
    fs::create_dir(&external).unwrap();
    fs::write(external.join("keep"), "external content").unwrap();
    symlink(&external, app.join("source-locks")).unwrap();
    let checkout = ManagedCheckout::new(&app, REGISTRY, FIRST).unwrap();
    assert!(matches!(
        checkout.try_write(),
        Err(IngestError::SourceOwnership)
    ));
    fs::remove_file(app.join("source-locks")).unwrap();
    drop(checkout.try_write().unwrap());
    symlink(&external, checkout.root()).unwrap();
    assert!(matches!(
        checkout.try_write(),
        Err(IngestError::SourceOwnership)
    ));
    fs::remove_file(checkout.root()).unwrap();
    fs::remove_file(checkout.slot.join("owner.json")).unwrap();
    symlink(external.join("keep"), checkout.slot.join("owner.json")).unwrap();
    assert!(matches!(
        checkout.try_write(),
        Err(IngestError::SourceOwnership)
    ));
    assert_eq!(
        fs::read_to_string(external.join("keep")).unwrap(),
        "external content"
    );
}
