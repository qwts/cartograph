//! AC-0205 / T-0205: the shared walk honors the tree's own `.gitignore`.

use super::*;

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn rels(root: &Path, gitignores: Gitignores) -> Vec<String> {
    files(root, &|name| name == "node_modules", gitignores)
        .unwrap()
        .into_iter()
        .map(|file| file.rel)
        .collect()
}

/// A tree with a root `.gitignore`, a nested one that re-includes a file,
/// and a directory skipped by the caller's fallback set.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, ".gitignore", "build/\n/out\n*.log\n# comment\n");
    write(root, "src/app.ts", "");
    write(root, "src/build/keep.ts", ""); // `build/` matches at any depth
    write(root, "build/gen.ts", "");
    write(root, "out/gen.ts", "");
    write(root, "pkg/out/kept.ts", ""); // `/out` is anchored to the root
    write(root, "debug.log", "");
    write(root, "node_modules/dep/index.ts", "");
    write(root, "vendor/.gitignore", "*.ts\n!own.ts\n");
    write(root, "vendor/lib.ts", "");
    write(root, "vendor/own.ts", "");
    dir
}

#[test]
fn gitignored_trees_are_excluded_and_the_rest_stays_sorted() {
    let dir = fixture();
    assert_eq!(
        rels(dir.path(), Gitignores::Honor),
        [
            ".gitignore",
            "pkg/out/kept.ts",
            "src/app.ts",
            "vendor/.gitignore",
            "vendor/own.ts",
        ]
    );
}

#[test]
fn disregarding_gitignores_applies_only_the_caller_skips() {
    let dir = fixture();
    let all = rels(dir.path(), Gitignores::Disregard);
    assert!(all.contains(&"build/gen.ts".to_string()));
    assert!(all.contains(&"vendor/lib.ts".to_string()));
    assert!(!all.iter().any(|rel| rel.starts_with("node_modules/")));
    let mut sorted = all.clone();
    sorted.sort();
    assert_eq!(all, sorted);
}

#[test]
fn rules_match_relative_to_the_directory_that_declares_them() {
    let rules = IgnoreRules::default()
        .descend("", Some(b"/top.ts\n"))
        .descend("a/b", Some(b"/gen\n!keep.log\n"))
        .descend("a/b/c", None);
    assert!(rules.is_ignored("top.ts", false));
    assert!(!rules.is_ignored("a/top.ts", false));
    assert!(rules.is_ignored("a/b/gen", true));
    assert!(!rules.is_ignored("a/b/c/gen", true));
    // An inner negation outranks an outer ignore; the outer rule still
    // decides whatever the inner layer is silent about.
    let outer = IgnoreRules::default()
        .descend("", Some(b"*.log\n"))
        .descend("a", Some(b"!keep.log\n"));
    assert!(outer.is_ignored("a/other.log", false));
    assert!(!outer.is_ignored("a/keep.log", false));
}

#[cfg(unix)]
#[test]
fn symlinks_are_never_followed() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "secret.ts", "");
    write(dir.path(), "src/a.ts", "");
    std::os::unix::fs::symlink(outside.path(), dir.path().join("linked")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("src/a.ts"), dir.path().join("b.ts")).unwrap();
    assert_eq!(rels(dir.path(), Gitignores::Honor), ["src/a.ts"]);
}

#[test]
fn an_oversized_gitignore_fails_instead_of_truncating() {
    let big = vec![b'a'; usize::try_from(MAX_GITIGNORE_BYTES).unwrap() + 1];
    assert!(read_gitignore(big.as_slice()).is_err());
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(GITIGNORE), &big).unwrap();
    assert!(files(dir.path(), &|_| false, Gitignores::Honor).is_err());
    assert!(files(dir.path(), &|_| false, Gitignores::Disregard).is_ok());
}
