use super::*;
use std::path::Path;

fn source() -> SourceId {
    SourceId::new("host-repository-1").unwrap()
}

fn capture(root: &Path, paths: &[&str]) -> Capture {
    capture_working_tree(
        root,
        &source(),
        &paths
            .iter()
            .map(|path| path.to_string())
            .collect::<Vec<_>>(),
        CaptureLimits::default(),
    )
    .unwrap()
}

// AC-0132: identity binds selected bytes and host identity, not ordering/location.
#[test]
fn canonical_capture_binds_raw_bytes_membership_and_source() {
    let one = tempfile::tempdir().unwrap();
    let two = tempfile::tempdir().unwrap();
    for root in [one.path(), two.path()] {
        std::fs::write(root.join("a.ts"), b"const a = 1; // one").unwrap();
        std::fs::write(root.join("b.ts"), b"const b = 2;").unwrap();
    }
    let first = capture(one.path(), &["b.ts", "a.ts"]);
    assert_eq!(first.id(), capture(one.path(), &["a.ts", "b.ts"]).id());
    assert_eq!(first.id(), capture(two.path(), &["a.ts", "b.ts"]).id());
    assert_ne!(first.id(), capture(one.path(), &["a.ts"]).id());
    let different_source = capture_working_tree(
        one.path(),
        &SourceId::new("host-repository-2").unwrap(),
        &["a.ts".into(), "b.ts".into()],
        CaptureLimits::default(),
    )
    .unwrap();
    assert_ne!(first.id(), different_source.id());
    std::fs::write(one.path().join("a.ts"), b"const a = 1; // two").unwrap();
    assert_ne!(first.id(), capture(one.path(), &["a.ts", "b.ts"]).id());
    assert_eq!(first.file("a.ts").unwrap().bytes(), b"const a = 1; // one");
    assert_eq!(first.manifest.files[0].path, "a.ts");
}

// AC-0132: diagnostics and serializable metadata must not archive raw source.
#[test]
fn capture_debug_and_manifest_omit_source_content() {
    let dir = tempfile::tempdir().unwrap();
    let canary = "captured-private-source-canary-932701";
    std::fs::write(dir.path().join("a.ts"), canary).unwrap();
    let result = capture(dir.path(), &["a.ts"]);
    for output in [
        format!("{result:?}"),
        format!("{:?}", result.file("a.ts").unwrap()),
        serde_json::to_string(result.manifest()).unwrap(),
    ] {
        assert!(!output.contains(canary));
        assert!(!output.contains(&format!("{:?}", canary.as_bytes())));
    }
    assert!(SourceId::new("").is_err());
    assert!(SourceId::new("x".repeat(MAX_SOURCE_ID_BYTES + 1)).is_err());
    assert!(SourceId::new("a\0b").is_err());
}

// AC-0132/AC-0133: empty selection is explicit; empty files have no nonempty span.
#[test]
fn empty_capture_and_empty_file_remain_distinct() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("empty"), []).unwrap();
    let empty = capture(dir.path(), &[]);
    let file = capture(dir.path(), &["empty"]);
    assert!(empty.manifest.files.is_empty());
    assert_ne!(empty.id(), file.id());
    assert!(empty.file("empty").is_err());
    assert_eq!(file.file("empty").unwrap().bytes(), b"");
    assert!(file.file("empty").unwrap().span(0, 0).is_err());
}

// AC-0133: canonical membership and all supplied limit dimensions are enforced.
#[test]
fn working_capture_rejects_paths_duplicates_and_limits() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a"), b"1234").unwrap();
    std::fs::write(dir.path().join("b"), b"5678").unwrap();
    for path in [
        "", "/a", "a/", "a//b", "./a", "a/../b", "../a", "a\\b", "C:/a", "a\0b",
    ] {
        assert!(
            capture_working_tree(
                dir.path(),
                &source(),
                &[path.into()],
                CaptureLimits::default()
            )
            .is_err(),
            "{path:?}"
        );
    }
    assert!(
        capture_working_tree(
            dir.path(),
            &source(),
            &["a".into(), "a".into()],
            CaptureLimits::default()
        )
        .is_err()
    );
    for limits in [
        CaptureLimits {
            max_files: 1,
            ..CaptureLimits::default()
        },
        CaptureLimits {
            max_file_bytes: 3,
            ..CaptureLimits::default()
        },
        CaptureLimits {
            max_total_bytes: 7,
            ..CaptureLimits::default()
        },
        CaptureLimits {
            max_manifest_bytes: 8,
            ..CaptureLimits::default()
        },
        CaptureLimits {
            max_path_bytes: 0,
            ..CaptureLimits::default()
        },
        CaptureLimits {
            max_source_id_bytes: 1,
            ..CaptureLimits::default()
        },
        CaptureLimits {
            max_files: MAX_FILES + 1,
            ..CaptureLimits::default()
        },
    ] {
        assert!(
            capture_working_tree(dir.path(), &source(), &["a".into(), "b".into()], limits).is_err()
        );
    }
    let exact = capture_working_tree(
        dir.path(),
        &source(),
        &["a".into(), "b".into()],
        CaptureLimits {
            max_file_bytes: 4,
            max_total_bytes: 8,
            ..CaptureLimits::default()
        },
    )
    .unwrap();
    assert_eq!(exact.file("a").unwrap().bytes(), b"1234");
}

// AC-0133: final and intermediate symlinks, directories, sockets and FIFOs fail.
#[cfg(unix)]
#[test]
fn working_capture_refuses_symlinks_and_special_files() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("real")).unwrap();
    std::fs::write(dir.path().join("real/file"), "inside").unwrap();
    std::fs::write(outside.path().join("file"), "outside").unwrap();
    std::os::unix::fs::symlink("real/file", dir.path().join("file-link")).unwrap();
    std::os::unix::fs::symlink("real", dir.path().join("dir-link")).unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(dir.path().join("socket")).unwrap();
    // Trusted OS utility used only to construct a disposable test fixture.
    // No repository-controlled command or production shell path is involved.
    assert!(
        std::process::Command::new("mkfifo")
            .arg(dir.path().join("fifo"))
            .status()
            .unwrap()
            .success()
    );
    for path in [
        "file-link",
        "dir-link/file",
        "escape/file",
        "real",
        "socket",
        "fifo",
    ] {
        assert!(
            capture_working_tree(
                dir.path(),
                &source(),
                &[path.into()],
                CaptureLimits::default()
            )
            .is_err(),
            "{path}"
        );
    }
    assert_eq!(
        capture(dir.path(), &["real/file"])
            .file("real/file")
            .unwrap()
            .bytes(),
        b"inside"
    );
}

// AC-0135: every binding component and exact range is checked before slicing.
#[test]
fn captured_spans_validate_full_reference_and_strict_encoding() {
    let dir = tempfile::tempdir().unwrap();
    let raw = b"ok\r\n\xf0\x9f\x9a\x80\xffend";
    std::fs::write(dir.path().join("a"), raw).unwrap();
    std::fs::write(dir.path().join("b"), raw).unwrap();
    let result = capture(dir.path(), &["a", "b"]);
    let file = result.file("a").unwrap();
    let good = file.span(0, 4).unwrap();
    assert_eq!(result.read_text_span(&good).unwrap(), "ok\r\n");
    assert_eq!(
        result.read_text_span(&file.span(4, 8).unwrap()).unwrap(),
        "🚀"
    );
    assert!(result.read_text_span(&file.span(5, 8).unwrap()).is_err());
    assert!(result.read_text_span(&file.span(8, 9).unwrap()).is_err());
    assert_eq!(result.read_span(&file.span(8, 9).unwrap()).unwrap(), &[255]);
    for range in [(0, 0), (8, 7), (0, 99), (u64::MAX, u64::MAX)] {
        assert!(file.span(range.0, range.1).is_err());
    }
    let mut wrong = good.clone();
    wrong.file.source_id = SourceId::new("other").unwrap();
    assert!(result.read_span(&wrong).is_err());
    wrong = good.clone();
    wrong.file.capture_id.push('0');
    assert!(result.read_span(&wrong).is_err());
    wrong = good.clone();
    wrong.file.digest = "0".repeat(64);
    assert!(result.read_span(&wrong).is_err());
    wrong = good.clone();
    wrong.file.byte_len += 1;
    assert!(result.read_span(&wrong).is_err());
    wrong = good.clone();
    wrong.file.path = "missing".into();
    assert!(result.read_span(&wrong).is_err());
    wrong = good.clone();
    wrong.byte_end = 99;
    assert!(result.read_span(&wrong).is_err());
    let restricted = capture_working_tree(
        dir.path(),
        &source(),
        &["a".into()],
        CaptureLimits {
            max_span_bytes: 2,
            ..CaptureLimits::default()
        },
    )
    .unwrap();
    assert!(restricted.file("a").unwrap().span(0, 3).is_err());
}

fn commit_tree(repo: &git2::Repository, tree: git2::Oid) -> git2::Oid {
    let signature =
        git2::Signature::new("fixture", "fixture@example.invalid", &git2::Time::new(1, 0)).unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "fixture",
        &repo.find_tree(tree).unwrap(),
        &[],
    )
    .unwrap()
}

// AC-0134: Git bytes are exact objects; checkout/HEAD changes never substitute them.
#[test]
fn git_capture_uses_exact_objects_and_distinct_kind() {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    repo.config()
        .unwrap()
        .set_bool("core.autocrlf", true)
        .unwrap();
    let blob = repo.blob(b"first\nsecond\n").unwrap();
    let tree_id = {
        let mut builder = repo.treebuilder(None).unwrap();
        builder.insert("a.txt", blob, 0o100644).unwrap();
        builder.write().unwrap()
    };
    let commit = commit_tree(&repo, tree_id);
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("a.txt")).unwrap(),
        b"first\r\nsecond\r\n"
    );
    let result = capture_git(
        dir.path(),
        &source(),
        &commit.to_string(),
        &["a.txt".into()],
        CaptureLimits::default(),
    )
    .unwrap();
    assert_eq!(result.file("a.txt").unwrap().bytes(), b"first\nsecond\n");
    repo.set_head("refs/heads/unborn").unwrap();
    std::fs::write(dir.path().join("a.txt"), b"first\nsecond\n").unwrap();
    assert_eq!(
        capture_git(
            dir.path(),
            &source(),
            &commit.to_string().to_uppercase(),
            &["a.txt".into()],
            CaptureLimits::default()
        )
        .unwrap()
        .id(),
        result.id()
    );
    assert_ne!(capture(dir.path(), &["a.txt"]).id(), result.id());
    assert_eq!(
        result.manifest.files[0].git.as_ref().unwrap().oid,
        blob.to_string()
    );
    for revision in [
        "HEAD",
        "main",
        "abc123",
        "0000000000000000000000000000000000000000",
    ] {
        assert!(
            capture_git(
                dir.path(),
                &source(),
                revision,
                &["a.txt".into()],
                CaptureLimits::default()
            )
            .is_err()
        );
    }
    assert!(
        capture_git(
            dir.path(),
            &source(),
            &commit.to_string(),
            &["a.txt".into()],
            CaptureLimits {
                max_file_bytes: 2,
                ..CaptureLimits::default()
            }
        )
        .is_err()
    );
}

// AC-0134: symlink/gitlink tree entries and missing local blobs fail closed.
#[test]
fn git_capture_rejects_unsupported_entries_and_missing_objects() {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let blob = repo.blob(b"raw fixture").unwrap();
    let empty_tree = repo.treebuilder(None).unwrap().write().unwrap();
    let first = commit_tree(&repo, empty_tree);
    let tree_id = {
        let mut builder = repo.treebuilder(None).unwrap();
        builder.insert("regular", blob, 0o100644).unwrap();
        builder.insert("link", blob, 0o120000).unwrap();
        builder.insert("submodule", first, 0o160000).unwrap();
        builder.insert("directory", empty_tree, 0o040000).unwrap();
        builder.write().unwrap()
    };
    let signature =
        git2::Signature::new("fixture", "fixture@example.invalid", &git2::Time::new(2, 0)).unwrap();
    let next = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "next",
            &repo.find_tree(tree_id).unwrap(),
            &[&repo.find_commit(first).unwrap()],
        )
        .unwrap();
    for path in ["link", "link/file", "submodule", "directory", "absent"] {
        assert!(
            capture_git(
                dir.path(),
                &source(),
                &next.to_string(),
                &[path.into()],
                CaptureLimits::default()
            )
            .is_err()
        );
    }
    let blob_hex = blob.to_string();
    std::fs::remove_file(
        repo.path()
            .join("objects")
            .join(&blob_hex[..2])
            .join(&blob_hex[2..]),
    )
    .unwrap();
    drop(repo);
    assert!(
        capture_git(
            dir.path(),
            &source(),
            &next.to_string(),
            &["regular".into()],
            CaptureLimits::default()
        )
        .is_err()
    );
}
