//! Read-only source access for evidence jump-to-source (US-0002/AC-0006,
//! AC-0028 groundwork). Never writes to target code (NG1).

use std::io;
use std::path::Path;

/// Largest file returned to the webview; evidence views need context around a
/// span, not multi-megabyte payloads.
pub const MAX_EVIDENCE_BYTES: usize = 256 * 1024;

/// Read the file at `rel_path` under `root`, refusing any path that escapes
/// `root` (`..`, absolute paths, symlinks out of the tree). Returns the file
/// text (lossy UTF-8, capped at [`MAX_EVIDENCE_BYTES`]) and whether it was cut.
pub fn read_source(root: &Path, rel_path: &str) -> io::Result<(String, bool)> {
    let root = root.canonicalize()?;
    let candidate = root.join(rel_path);
    let file = candidate.canonicalize()?;
    if !file.starts_with(&root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("evidence path escapes ingest root: {rel_path}"),
        ));
    }
    let bytes = std::fs::read(&file)?;
    let truncated = bytes.len() > MAX_EVIDENCE_BYTES;
    let slice = if truncated {
        &bytes[..MAX_EVIDENCE_BYTES]
    } else {
        &bytes[..]
    };
    Ok((String::from_utf8_lossy(slice).into_owned(), truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_files_under_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/app.ts"), "const x = 1;").unwrap();
        let (text, truncated) = read_source(dir.path(), "src/app.ts").unwrap();
        assert_eq!(text, "const x = 1;");
        assert!(!truncated);
    }

    #[test]
    fn rejects_paths_escaping_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let sibling = tempfile::tempdir().unwrap();
        std::fs::write(sibling.path().join("secret.txt"), "nope").unwrap();
        // Relative traversal out of root must be refused, not resolved.
        let escape = format!(
            "../{}/secret.txt",
            sibling.path().file_name().unwrap().to_string_lossy()
        );
        let err = read_source(dir.path(), &escape);
        assert!(err.is_err());
    }
}
