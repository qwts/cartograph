//! Ingest tests (US-0001). Clones run offline against `file://` bare-repo
//! fixtures; the auth-failure path is unit-tested at the error-mapping
//! layer (a live 401 requires the network — MT-covered).

use super::*;

/// Build a bare repo with one commit containing `app.ts`; returns its
/// `file://` URL. Shells out to `git` (present in CI and dev machines).
fn bare_fixture(dir: &Path) -> String {
    let src = dir.join("src-repo");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("app.ts"), "export function run() {}\n").unwrap();
    let run = |args: &[&str], cwd: &Path| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {:?}", out);
    };
    run(&["init", "-q", "-b", "main"], &src);
    run(&["add", "."], &src);
    run(&["commit", "-q", "-m", "init"], &src);
    let bare = dir.join("fixture.git");
    run(
        &[
            "clone",
            "-q",
            "--bare",
            src.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
        dir,
    );
    format!("file://{}", bare.display())
}

// AC-0001: a valid repo URL clones read-only and is listed with its
// commit SHA. (T-0001)
#[test]
fn clone_lists_repo_with_commit_sha() {
    let dir = tempfile::tempdir().unwrap();
    let url = bare_fixture(dir.path());
    let dest = dir.path().join("clones");

    let cloned = clone_repo(&url, &dest, None).unwrap();
    assert_eq!(cloned.repo, "local/fixture");
    assert_eq!(cloned.commit_sha.len(), 40, "full SHA");
    assert!(cloned.path.join("app.ts").exists());
    // Re-adding replaces, same identity/SHA (v1 one-shot ingest).
    let again = clone_repo(&url, &dest, None).unwrap();
    assert_eq!(again.commit_sha, cloned.commit_sha);
}

// AC-0003: a failed clone leaves no partial clone behind. (T-0003)
#[test]
fn failed_clone_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("clones");
    let url = format!("file://{}/does-not-exist.git", dir.path().display());

    let err = clone_repo(&url, &dest, None).unwrap_err();
    assert!(!matches!(err, IngestError::InvalidUrl(_)));
    let leftovers: Vec<_> = std::fs::read_dir(&dest)
        .map(|it| it.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "no partial clone: {leftovers:?}");
}

// AC-0003: auth-shaped git errors map to a typed failure with
// remediation the UI can show verbatim. (T-0003)
#[test]
fn auth_errors_carry_remediation() {
    let e = git2::Error::new(
        git2::ErrorCode::Auth,
        git2::ErrorClass::Http,
        "too many redirects or authentication replays",
    );
    let mapped = classify_git_error("https://github.com/acme/private", e);
    match mapped {
        IngestError::Auth { remediation, .. } => {
            assert!(remediation.contains("GH_TOKEN"));
            assert!(remediation.contains("gh auth login"));
        }
        other => panic!("expected Auth, got {other:?}"),
    }
    // Non-auth errors pass through untouched.
    let e = git2::Error::new(
        git2::ErrorCode::NotFound,
        git2::ErrorClass::Repository,
        "could not find repository",
    );
    assert!(matches!(classify_git_error("u", e), IngestError::Git(_)));
}

// URL forms: https, ssh, shorthand, file — one identity scheme.
#[test]
fn repo_urls_parse_to_identities() {
    let cases = [
        ("https://github.com/acme/shop", "acme/shop"),
        ("https://github.com/acme/shop.git", "acme/shop"),
        ("git@github.com:acme/shop.git", "acme/shop"),
        ("acme/shop", "acme/shop"),
        ("file:///tmp/x/mirror.git", "local/mirror"),
    ];
    for (url, want) in cases {
        let (id, _) = parse_repo_url(url).unwrap();
        assert_eq!(id, want, "{url}");
    }
    for bad in [
        "https://gitlab.com/a/b",
        "not a url",
        "https://github.com/onlyowner",
    ] {
        assert!(parse_repo_url(bad).is_err(), "{bad}");
    }
}

// AC-0002: the topology manifest parses repos, layer hints, and known
// identities; unknown layers fail loudly. (T-0002)
#[test]
fn manifest_parses_repos_layers_and_env() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(manifest::MANIFEST_NAME),
        r#"
[[repos]]
url = "acme/shop"
layers = ["server", "client"]

[[repos]]
url = "./infra"
layers = ["infra"]
pulumi_json = "stacks/infra.json"
otel_jsonl = ["traces/infra.jsonl"]

[env]
ORDERS_QUEUE = "https://sqs.example/orders"
"#,
    )
    .unwrap();
    // Loading by directory finds the canonical file name.
    let m = manifest::SystemManifest::load(dir.path()).unwrap();
    assert_eq!(m.repos.len(), 2);
    assert_eq!(m.repos[0].url, "acme/shop");
    assert_eq!(m.repos[0].layers, ["server", "client"]);
    assert_eq!(m.repos[1].layers, ["infra"]);
    assert_eq!(m.repos[1].pulumi_json.as_deref(), Some("stacks/infra.json"));
    assert_eq!(m.repos[1].otel_jsonl, ["traces/infra.jsonl"]);
    assert_eq!(
        m.env.get("ORDERS_QUEUE").map(String::as_str),
        Some("https://sqs.example/orders")
    );
}

#[test]
fn manifest_rejects_unknown_layers() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join(manifest::MANIFEST_NAME);
    std::fs::write(&f, "[[repos]]\nurl = \"a/b\"\nlayers = [\"backend\"]\n").unwrap();
    let err = manifest::SystemManifest::load(&f).unwrap_err();
    assert!(err.to_string().contains("unknown layer \"backend\""));
}
