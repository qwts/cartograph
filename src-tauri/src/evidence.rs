//! Read-only source access for evidence jump-to-source (US-0002/AC-0006,
//! AC-0028 groundwork). Never writes to target code (NG1).

use std::io::{self, Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::Path;

/// Largest window returned to the webview; evidence views need context around
/// a span, not multi-megabyte payloads.
pub const MAX_EVIDENCE_BYTES: u64 = 256 * 1024;

/// A window of a source file guaranteed to contain the evidence span.
pub struct SourceWindow {
    /// Window content (lossy UTF-8).
    pub text: String,
    /// Byte offset of the window within the file — subtract from span offsets
    /// to highlight within `text`.
    pub window_start: u64,
    /// True when the file was larger than the window.
    pub truncated: bool,
}

/// Read a window of the file at `rel_path` under `root` that contains
/// `span` (with leading/trailing context up to [`MAX_EVIDENCE_BYTES`]),
/// refusing any path that escapes `root` (`..`, absolute paths, symlinks out
/// of the tree). Only the window is read from disk, never the whole file.
pub fn read_source(root: &Path, rel_path: &str, span: &Range<u64>) -> io::Result<SourceWindow> {
    let root = root.canonicalize()?;
    let candidate = root.join(rel_path);
    let file_path = candidate.canonicalize()?;
    if !file_path.starts_with(&root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("evidence path escapes ingest root: {rel_path}"),
        ));
    }
    let mut file = std::fs::File::open(&file_path)?;
    let len = file.metadata()?.len();

    // Center the window on the span so the highlight always fits, however
    // deep in the file it sits (a head-only cap would cut late spans off).
    let window_start = if len <= MAX_EVIDENCE_BYTES {
        0
    } else {
        let context = MAX_EVIDENCE_BYTES.saturating_sub(span.end.saturating_sub(span.start)) / 2;
        span.start.saturating_sub(context).min(len)
    };
    let window_len = MAX_EVIDENCE_BYTES.min(len - window_start);

    file.seek(SeekFrom::Start(window_start))?;
    let mut bytes = vec![0u8; window_len as usize];
    file.read_exact(&mut bytes)?;
    Ok(SourceWindow {
        text: String::from_utf8_lossy(&bytes).into_owned(),
        window_start,
        truncated: window_start > 0 || window_start + window_len < len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_files_under_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/app.ts"), "const x = 1;").unwrap();
        let w = read_source(dir.path(), "src/app.ts", &(6..7)).unwrap();
        assert_eq!(w.text, "const x = 1;");
        assert_eq!(w.window_start, 0);
        assert!(!w.truncated);
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
        assert!(read_source(dir.path(), &escape, &(0..1)).is_err());
    }

    #[test]
    fn window_contains_spans_beyond_the_size_cap() {
        // A span deep in a large file must still arrive in the window —
        // a head-only truncation would highlight nothing.
        let dir = tempfile::tempdir().unwrap();
        let needle = "app.get('/deep', handler);";
        let mut content = "x".repeat(2 * MAX_EVIDENCE_BYTES as usize);
        let start = content.len() as u64;
        content.push_str(needle);
        std::fs::write(dir.path().join("big.ts"), &content).unwrap();

        let span = start..start + needle.len() as u64;
        let w = read_source(dir.path(), "big.ts", &span).unwrap();
        assert!(w.truncated);
        let local_start = (span.start - w.window_start) as usize;
        let local_end = (span.end - w.window_start) as usize;
        assert_eq!(&w.text[local_start..local_end], needle);
    }
}
