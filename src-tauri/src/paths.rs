//! Canonicalization boundary for every user-supplied path that reaches
//! storage (#225, refs ADR-0018). On Windows `std::fs::canonicalize` returns
//! `\\?\`-prefixed extended-length paths; those strings flow into stored
//! `project_roots` keys and the `Repo` node's hashed fact bytes, so the same
//! commit would hash differently per platform — breaking the M10 re-ingest
//! determinism invariant — and evidence readback would compare mixed-prefix
//! paths. `dunce::canonicalize` strips the verbatim prefix exactly when the
//! result stays a valid non-verbatim path (long paths, device names, and
//! trailing-dot components keep it, because Windows cannot open them
//! otherwise) and is a byte-identical passthrough to `std::fs::canonicalize`
//! everywhere else.

use std::io;
use std::path::{Path, PathBuf};

/// Canonicalize `path` without the Windows `\\?\` verbatim prefix. Every
/// ingest/evidence canonicalize call goes through here so future call sites
/// inherit the normalization instead of re-deciding it.
pub fn canonicalize(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    dunce::canonicalize(path)
}

#[cfg(test)]
mod tests {
    #[test]
    fn canonical_paths_carry_no_verbatim_prefix() {
        // The determinism contract (#225): what we store must never start
        // with the Windows extended-length prefix. On Unix this asserts the
        // passthrough; on Windows it asserts the actual strip.
        let dir = tempfile::tempdir().expect("tempdir");
        let canonical = super::canonicalize(dir.path()).expect("canonicalize");
        assert!(!canonical.to_string_lossy().starts_with(r"\\?\"));
        assert_eq!(
            canonical,
            dunce::simplified(&std::fs::canonicalize(dir.path()).expect("std canonicalize"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn stripped_path_still_opens() {
        // A naive unconditional strip would produce paths Windows cannot
        // open (>260 chars, reserved names); dunce keeps the prefix in those
        // cases. Round-trip a real file to prove the normal case opens.
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("probe.txt");
        std::fs::write(&file, b"evidence").expect("write");
        let canonical = super::canonicalize(&file).expect("canonicalize");
        assert!(!canonical.to_string_lossy().starts_with(r"\\?\"));
        assert_eq!(std::fs::read(&canonical).expect("read"), b"evidence");
    }
}
