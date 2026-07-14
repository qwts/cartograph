//! Read-only source access for evidence jump-to-source (US-0002/AC-0006,
//! AC-0028 groundwork). Never writes to target code (NG1).

use std::io::{self, Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::Path;

/// Largest window returned to the webview; evidence views need context around
/// a span, not multi-megabyte payloads.
pub const MAX_EVIDENCE_BYTES: u64 = 256 * 1024;

/// Resolve `rel_path` under `root`, refusing any escape (`..`, absolute
/// paths, symlinks out of the tree).
fn confined_path(root: &Path, rel_path: &str) -> io::Result<std::path::PathBuf> {
    let root = root.canonicalize()?;
    let candidate = root.join(rel_path);
    let file_path = candidate.canonicalize()?;
    if !file_path.starts_with(&root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("evidence path escapes ingest root: {rel_path}"),
        ));
    }
    Ok(file_path)
}

/// Read exactly the span's bytes and nothing else (lossy UTF-8 applied to
/// the span alone). This is the citation contract for escalation payloads:
/// byte offsets index the file, so slicing must happen *before* any lossy
/// conversion — a window that starts mid-character would otherwise shift
/// the offsets and leak unrelated source into the payload.
pub fn read_span_exact(root: &Path, rel_path: &str, span: &Range<u64>) -> io::Result<String> {
    if span.end <= span.start || span.end - span.start > MAX_EVIDENCE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "evidence span {}..{} is empty or oversized",
                span.start, span.end
            ),
        ));
    }
    let file_path = confined_path(root, rel_path)?;
    let mut file = std::fs::File::open(&file_path)?;
    let len = file.metadata()?.len();
    if span.end > len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!(
                "evidence span ends at {} but the file has {len} bytes",
                span.end
            ),
        ));
    }
    file.seek(SeekFrom::Start(span.start))?;
    let mut bytes = vec![0u8; (span.end - span.start) as usize];
    file.read_exact(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// A window of a source file guaranteed to contain the evidence span.
pub struct SourceWindow {
    /// Window content (lossy UTF-8).
    pub text: String,
    /// Byte offset of the window within the file — subtract from span offsets
    /// to highlight within `text`.
    pub window_start: u64,
    /// 1-based line number of the window's first line, so a windowed view
    /// can show true file line numbers instead of pretending it starts at 1.
    pub window_start_line: u64,
    /// True when the file was larger than the window.
    pub truncated: bool,
}

/// Read a window of the file at `rel_path` under `root` that contains
/// `span` (with leading/trailing context up to [`MAX_EVIDENCE_BYTES`]),
/// refusing any path that escapes `root` (`..`, absolute paths, symlinks out
/// of the tree). Only the window is read from disk, never the whole file.
pub fn read_source(root: &Path, rel_path: &str, span: &Range<u64>) -> io::Result<SourceWindow> {
    let file_path = confined_path(root, rel_path)?;
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

    // True line number of the window's first line: count the newlines
    // before it, streaming so a deep window never loads the whole prefix.
    let window_start_line = 1 + {
        let mut newlines = 0u64;
        if window_start > 0 {
            file.seek(SeekFrom::Start(0))?;
            let mut remaining = window_start;
            let mut chunk = vec![0u8; 64 * 1024];
            while remaining > 0 {
                let take = chunk.len().min(remaining as usize);
                file.read_exact(&mut chunk[..take])?;
                newlines += chunk[..take].iter().filter(|byte| **byte == b'\n').count() as u64;
                remaining -= take as u64;
            }
        }
        newlines
    };

    file.seek(SeekFrom::Start(window_start))?;
    let mut bytes = vec![0u8; window_len as usize];
    file.read_exact(&mut bytes)?;
    Ok(SourceWindow {
        text: String::from_utf8_lossy(&bytes).into_owned(),
        window_start,
        window_start_line,
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

    #[test]
    fn exact_span_reads_slice_bytes_before_lossy_conversion() {
        // Multibyte content before the span: byte offsets index the FILE,
        // and the exact reader must return precisely the cited bytes —
        // never more (#141 review: a mid-character window start must not
        // leak unrelated source into an escalation payload).
        let dir = tempfile::tempdir().unwrap();
        let content = "// naïve café — 🚀\nconst handler = capture;\n";
        std::fs::write(dir.path().join("src.ts"), content).unwrap();
        let needle = "const handler = capture;";
        let start = content.find(needle).unwrap() as u64;
        let span = start..start + needle.len() as u64;

        let text = read_span_exact(dir.path(), "src.ts", &span).unwrap();
        assert_eq!(text, needle);

        // Empty, oversized, and beyond-EOF spans are explicit errors.
        assert!(read_span_exact(dir.path(), "src.ts", &(5..5)).is_err());
        assert!(read_span_exact(dir.path(), "src.ts", &(0..10_000)).is_err());
        // Escapes are refused exactly like the windowed reader.
        assert!(read_span_exact(dir.path(), "../src.ts", &(0..4)).is_err());
    }

    #[test]
    fn windowed_reads_report_true_starting_line() {
        // A deep window must know the real file line it starts on, so the
        // drawer's gutter never pretends the window starts at line 1.
        let dir = tempfile::tempdir().unwrap();
        let line = "y".repeat(63); // 64 bytes with the newline
        let line_count = (3 * MAX_EVIDENCE_BYTES as usize) / 64;
        let content: String = (0..line_count).map(|_| format!("{line}\n")).collect();
        let span_start = content.len() as u64 - 100;
        std::fs::write(dir.path().join("long.ts"), &content).unwrap();

        let w = read_source(dir.path(), "long.ts", &(span_start..span_start + 10)).unwrap();
        assert!(w.window_start > 0);
        // Every line is exactly 64 bytes (newline at 64k-1), so the number
        // of newlines before window_start is floor(window_start / 64) —
        // the streamed count must match the closed form.
        let expected = 1 + w.window_start / 64;
        assert_eq!(w.window_start_line, expected);

        // A small file starts at line 1, no prefix scan involved.
        std::fs::write(dir.path().join("small.ts"), "a\nb\nc\n").unwrap();
        let small = read_source(dir.path(), "small.ts", &(2..3)).unwrap();
        assert_eq!(small.window_start_line, 1);
    }
}
