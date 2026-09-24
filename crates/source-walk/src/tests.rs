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

mod parallel_merge {
    //! AC-0208 / T-0208: parallel extraction merges in walk order.
    use crate::parallel::{
        MAX_WORKERS, Parallelism, UNKNOWN_MEMORY_AUTO_CAP, map_ordered, resolve_auto, with_workers,
        workers,
    };

    fn items(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("f{i:04}")).collect()
    }

    /// Uneven per-item cost so completion order differs from item order.
    fn slow_echo(item: &str) -> Result<String, String> {
        let n: u64 = item[1..].parse().unwrap();
        std::thread::sleep(std::time::Duration::from_micros((n * 7919) % 500));
        Ok(item.to_uppercase())
    }

    #[test]
    fn results_merge_in_item_order_for_every_worker_count() {
        let items = items(200);
        let serial = with_workers(1, || {
            let mut seen = Vec::new();
            map_ordered(&items, slow_echo, |item, value| {
                seen.push((item.to_string(), value));
                Ok::<_, String>(())
            })
            .unwrap();
            seen
        });
        for n in [2, 3, 8] {
            let parallel = with_workers(n, || {
                assert_eq!(workers(), n);
                let mut seen = Vec::new();
                map_ordered(&items, slow_echo, |item, value| {
                    seen.push((item.to_string(), value));
                    Ok::<_, String>(())
                })
                .unwrap();
                seen
            });
            assert_eq!(parallel, serial);
        }
    }

    #[test]
    fn the_first_error_in_item_order_wins_and_nothing_after_it_merges() {
        let items = items(64);
        let failing = |item: &str| {
            let n: usize = item[1..].parse().unwrap();
            if n == 40 || n == 50 {
                return Err(format!("bad {item}"));
            }
            slow_echo(item)
        };
        with_workers(6, || {
            let mut merged = 0;
            let error = map_ordered(&items, failing, |_, _| {
                merged += 1;
                Ok(())
            })
            .unwrap_err();
            assert_eq!(error, "bad f0040");
            assert_eq!(merged, 40);
        });
    }

    #[test]
    fn a_worker_panic_resumes_on_the_calling_thread() {
        let items = items(16);
        let outcome = std::panic::catch_unwind(|| {
            with_workers(4, || {
                map_ordered(
                    &items,
                    |item| {
                        assert_ne!(item, "f0009", "boom");
                        Ok::<_, String>(())
                    },
                    |_, ()| Ok(()),
                )
            })
        });
        assert!(outcome.is_err());
    }

    #[test]
    fn a_merge_panic_stops_the_workers_and_resumes() {
        // Many items and 2 workers so the window fills while the merge is
        // unwinding; a regression hangs, so run it under a timeout.
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let items = items(256);
            let outcome = std::panic::catch_unwind(|| {
                with_workers(2, || {
                    map_ordered(
                        &items,
                        |_| Ok::<_, String>(()),
                        |item, ()| {
                            assert_ne!(item, "f0001", "merge boom");
                            Ok(())
                        },
                    )
                })
            });
            done.send(outcome.is_err()).unwrap();
        });
        let panicked = finished
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("map_ordered hung after a panic in merge");
        assert!(panicked);
    }

    #[test]
    fn settings_resolve_to_a_bounded_worker_count() {
        assert_eq!(Parallelism::Fixed(0).workers(), 1);
        assert_eq!(Parallelism::Fixed(10_000).workers(), MAX_WORKERS);
        let auto = Parallelism::Auto.workers();
        assert!((1..=MAX_WORKERS).contains(&auto));
    }

    #[test]
    fn auto_is_capped_when_physical_memory_is_unknown() {
        // AC-0208 (#472): where the platform does not report physical memory
        // (Windows), Auto is conservatively capped instead of running one
        // worker per core — 16 threads with unknown memory get 4, not 15.
        const GIB: u64 = 1 << 30;
        assert_eq!(resolve_auto(16, None), UNKNOWN_MEMORY_AUTO_CAP);
        assert_eq!(resolve_auto(3, None), 2, "fewer cores than the cap");
        assert_eq!(resolve_auto(1, None), 1);
        // Known memory keeps the 2 GiB-per-worker cap.
        assert_eq!(resolve_auto(16, Some(8 * GIB)), 4);
        assert_eq!(resolve_auto(16, Some(64 * GIB)), 15);
        assert_eq!(resolve_auto(16, Some(GIB)), 1, "never below one worker");
        assert_eq!(resolve_auto(200, Some(1024 * GIB)), MAX_WORKERS);
    }
}

#[test]
fn the_directory_hook_reports_each_directory_and_stops_without_a_partial_list() {
    // AC-0197/AC-0198 (#453): the walk announces each directory before
    // reading it, with the files found so far, and a break stops it there —
    // no further directory is read and no partial list is returned.
    let dir = fixture();
    let root = dir.path();
    let mut seen = Vec::new();
    let all = files_until(
        root,
        &|name| name == "node_modules",
        Gitignores::Honor,
        &mut |step| {
            seen.push((step.dir.to_string(), step.found));
            ControlFlow::Continue(())
        },
    )
    .unwrap()
    .expect("completes");
    assert_eq!(
        all,
        files(root, &|name| name == "node_modules", Gitignores::Honor).unwrap()
    );
    // Ignored and skipped directories are never entered, so never announced.
    assert_eq!(
        seen,
        vec![
            (String::new(), 0),
            ("pkg".to_string(), 1), // the root `.gitignore` sorts first
            ("pkg/out".to_string(), 1),
            ("src".to_string(), 2),
            ("vendor".to_string(), 3),
        ]
    );

    let mut announced = Vec::new();
    let stopped = files_until(
        root,
        &|name| name == "node_modules",
        Gitignores::Honor,
        &mut |step| {
            announced.push(step.dir.to_string());
            if step.dir == "pkg" {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        },
    )
    .unwrap();
    assert!(stopped.is_none());
    assert_eq!(announced, vec![String::new(), "pkg".to_string()]);
}
