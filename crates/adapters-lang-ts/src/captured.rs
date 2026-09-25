//! Producer-bound primary source receipts (SPEC-06).
//!
//! The captured factory invokes the actual parser on the retained byte slice.
//! Only its private direct-output sink admits facts; public receipt validation
//! checks content integrity and does not establish who produced supplied JSON.
//! Directory enrichment still reads live configuration and is outside closure.

use super::{ExtractError, Extraction, IncrementalStats, SourceId};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, OpenOptions};
use core_graph::{Edge, Node};
use source_capture::{Capture, CapturedFile};
use source_walk::IgnoreRules;
use std::collections::BTreeMap;
use std::path::Path;

mod receipt;
pub use receipt::{
    Grammar, MAX_RECEIPT_BYTES, MAX_RECEIPT_RANGES, RangeRole, Receipt, ReceiptRange,
};

/// Maximum directory entries examined, including excluded entries.
pub const MAX_VISITED_ENTRIES: usize = 100_000;
/// Maximum nested directory depth below the host-selected root.
pub const MAX_DIRECTORY_DEPTH: usize = 64;
/// Maximum metadata retained in one adapter result, before host persistence.
pub const MAX_EXTRACTION_RECEIPT_BYTES: usize = 64 * 1024 * 1024;
/// Maximum receipts returned by one captured directory extraction.
pub const MAX_EXTRACTION_RECEIPTS: usize = 100_000;

/// Fixed diagnostics never include parsed source or untrusted receipt values.
#[derive(Debug, thiserror::Error)]
pub enum CapturedError {
    /// Invalid metadata, unsupported syntax binding, or malformed receipt.
    #[error("invalid primary source input: {0}")]
    Invalid(&'static str),
    /// A declared hard bound was exceeded.
    #[error("primary source limit exceeded: {0}")]
    Limit(&'static str),
    /// Source enumeration failed.
    #[error("primary source enumeration failed: {0}")]
    Io(#[from] std::io::Error),
    /// The retained capture could not supply a selected file or range.
    #[error("primary source capture failed: {0}")]
    Capture(#[from] source_capture::CaptureError),
    /// Ordinary parser or directory enrichment failure.
    #[error("primary source extraction failed: {0}")]
    Extraction(#[from] ExtractError),
}

/// Private producer knowledge recorded at direct emission, never inferred from
/// arbitrary public Extraction metadata. Recursive eval receives no such sink.
#[derive(Default)]
pub(super) struct DirectFacts {
    pub(super) owners: BTreeMap<String, Node>,
    pub(super) rules: Vec<(Node, Edge)>,
}

fn supported(path: &str) -> bool {
    super::SOURCE_EXTENSIONS
        .iter()
        .any(|extension| path.ends_with(extension))
        && !path.ends_with(".d.ts")
}

fn excluded_directory(name: &str) -> bool {
    super::skipped_directory(name)
}

/// This directory's own `.gitignore`, read through the confined handle
/// without following a symlink. Anything but a regular file is not a rule
/// source, exactly as for the ordinary walk (`source_walk::files`).
fn own_gitignore(directory: &Dir) -> Result<Option<Vec<u8>>, CapturedError> {
    match directory.symlink_metadata(source_walk::GITIGNORE) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = directory.open_with(source_walk::GITIGNORE, &options)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(CapturedError::Invalid("ignore file is not a regular file"));
    }
    match source_walk::read_gitignore(file) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            Err(CapturedError::Limit("ignore file bytes"))
        }
        Err(error) => Err(error.into()),
    }
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= source_capture::MAX_PATH_BYTES
        && !path.contains(['\\', ':', '\0'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Enumerate the ordinary TS/JS selection without following directory symlinks.
/// The selection honors the tree's own `.gitignore` files through the same
/// matcher as the ordinary walk (#248), so both lanes select identical files.
/// Each descent uses a rooted no-follow handle, including after a concurrent
/// entry replacement. The subsequent capture independently confines acquisition.
/// Unsupported encodings, nonexcluded symlinks and selected special files fail;
/// no partial selection is returned. This is not a filesystem snapshot.
pub fn enumerate_paths(root: &Path) -> Result<Vec<String>, CapturedError> {
    let directory = Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
    enumerate_with_limits(
        &directory,
        MAX_VISITED_ENTRIES,
        MAX_DIRECTORY_DEPTH,
        source_capture::MAX_FILES,
    )
}

fn enumerate_with_limits(
    directory: &Dir,
    max_entries: usize,
    max_depth: usize,
    max_files: usize,
) -> Result<Vec<String>, CapturedError> {
    struct Walk {
        files: Vec<String>,
        visited: usize,
        max_entries: usize,
        max_depth: usize,
        max_files: usize,
    }
    impl Walk {
        fn visit(
            &mut self,
            directory: &Dir,
            prefix: &str,
            parent: &IgnoreRules,
            depth: usize,
        ) -> Result<(), CapturedError> {
            let rules = parent.descend(prefix, own_gitignore(directory)?.as_deref());
            for entry in directory.entries()? {
                let entry = entry?;
                self.visited += 1;
                if self.visited > self.max_entries {
                    return Err(CapturedError::Limit("visited entries"));
                }
                let os_name = entry.file_name();
                let name = os_name
                    .to_str()
                    .ok_or(CapturedError::Invalid("non-UTF-8 path"))?;
                let kind = entry.file_type()?;
                if (kind.is_dir() || kind.is_symlink())
                    && excluded_directory(name)
                    && !supported(name)
                {
                    continue;
                }
                let path = if prefix.is_empty() {
                    name.to_string()
                } else {
                    format!("{prefix}/{name}")
                };
                // Ignored entries are excluded like skipped directories, so
                // an ignored symlink is not part of the selection either.
                if rules.is_ignored(&path, kind.is_dir()) {
                    continue;
                }
                if kind.is_symlink() {
                    return Err(CapturedError::Invalid("symlink in selected tree"));
                }
                if kind.is_dir() && excluded_directory(name) {
                    continue;
                }
                if kind.is_dir() {
                    if depth == self.max_depth {
                        return Err(CapturedError::Limit("directory depth"));
                    }
                    if !valid_path(&path) {
                        return Err(CapturedError::Invalid("directory path"));
                    }
                    let child = directory.open_dir_nofollow(name)?;
                    self.visit(&child, &path, &rules, depth + 1)?;
                } else if supported(name) {
                    if !kind.is_file() || !valid_path(&path) {
                        return Err(CapturedError::Invalid("selected source path or type"));
                    }
                    if self.files.len() == self.max_files {
                        return Err(CapturedError::Limit("selected files"));
                    }
                    self.files.push(path);
                }
            }
            Ok(())
        }
    }
    let mut walk = Walk {
        files: Vec::new(),
        visited: 0,
        max_entries,
        max_depth,
        max_files,
    };
    walk.visit(directory, "", &IgnoreRules::default(), 0)?;
    walk.files.sort();
    Ok(walk.files)
}

/// Parse precisely this immutable retained buffer, recording only direct lexical
/// rules, their actual emitted owners, and lexical GOVERNS. No source read or
/// retrospective attestation of a caller-provided Extraction occurs here.
/// Unsupported/oversized receipt inventories omit that whole receipt; they do
/// not truncate ranges or change ordinary recovered facts or interpretation.
pub fn extract_file(
    file: &CapturedFile,
    id: &SourceId,
) -> Result<(Extraction, Vec<Receipt>), CapturedError> {
    if !supported(&file.reference().path) || !valid_path(&file.reference().path) {
        return Err(CapturedError::Invalid("unsupported captured path"));
    }
    receipt::validate_identity(file.reference(), id.repo)?;
    let mut direct = DirectFacts::default();
    let extraction = super::extract_source_recording(
        file.bytes(),
        &file.reference().path,
        id,
        Some(&mut direct),
    )?;
    let mut receipts = BTreeMap::new();
    for (rule, edge) in &direct.rules {
        for node in [Some(rule), direct.owners.get(&edge.dst)]
            .into_iter()
            .flatten()
        {
            if let Ok(receipt) = Receipt::for_node(file, id, node) {
                receipts.insert(receipt.fact_key().clone(), receipt);
            }
        }
        if let Ok(receipt) = Receipt::for_edge(file, id, edge) {
            receipts.insert(receipt.fact_key().clone(), receipt);
        }
    }
    let mut receipts: Vec<_> = receipts.into_values().collect();
    retain_matching(&mut receipts, &extraction);
    Ok((extraction, receipts))
}

/// Parse the selected immutable membership, bypassing the ordinary parse cache,
/// then run the exact same directory completion as ordinary extraction. `root`
/// is used by that separate live configuration pass, not to read primary files.
/// The host persists this capture and receipts before graph publication, and
/// must repeat complete-fact matching after its own cross-layer enrichments.
///
/// Per-file parsing runs on the same ordered worker pool as the uncaptured
/// lane (#236, #482): files parse concurrently on up to [`workers`] threads,
/// but every result is merged back on the calling thread strictly in
/// manifest order (the manifest is itself sorted by path, #248), so receipts
/// and facts are byte-identical to a serial run for any worker count
/// (AC-0217) — parallelism changes only wall-clock time.
///
/// [`workers`]: source_walk::parallel::workers
pub fn extract_captured_dir(
    root: &Path,
    id: &SourceId,
    capture: &Capture,
    on_file: &mut dyn FnMut(&str),
) -> Result<(Extraction, Vec<Receipt>, IncrementalStats), CapturedError> {
    receipt::validate_source_repo(&capture.manifest().source_id, id.repo)?;
    let paths: Vec<String> = capture
        .manifest()
        .files
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    let mut extraction = Extraction::default();
    let mut receipts = Vec::new();
    let mut receipt_bytes = 0;
    let mut stats = IncrementalStats::default();
    source_walk::parallel::map_ordered(
        &paths,
        |path| extract_file(capture.file(path)?, id),
        |path, (facts, file_receipts)| {
            on_file(path);
            for receipt in &file_receipts {
                receipt_bytes += receipt.to_json()?.len();
            }
            if receipt_bytes > MAX_EXTRACTION_RECEIPT_BYTES
                || receipts.len() + file_receipts.len() > MAX_EXTRACTION_RECEIPTS
            {
                return Err(CapturedError::Limit("extraction receipt metadata"));
            }
            super::append_extraction(&mut extraction, facts);
            receipts.extend(file_receipts);
            stats.recomputed_files += 1;
            Ok(())
        },
    )?;
    super::complete_directory(&mut extraction, root, id)?;
    retain_matching(&mut receipts, &extraction);
    Ok((extraction, receipts, stats))
}

fn retain_matching(receipts: &mut Vec<Receipt>, extraction: &Extraction) {
    let nodes: BTreeMap<_, _> = extraction
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node))
        .collect();
    let edges: BTreeMap<_, _> = extraction
        .edges
        .iter()
        .map(|edge| {
            (
                (edge.src.clone(), edge.label.clone(), edge.dst.clone()),
                edge,
            )
        })
        .collect();
    receipts.retain(|receipt| match receipt.fact_key() {
        core_graph::source::FactKey::Node { id } => {
            nodes.get(id).is_some_and(|node| receipt.matches_node(node))
        }
        core_graph::source::FactKey::Edge {
            source,
            label,
            destination,
        } => edges
            .get(&(source.clone(), label.clone(), destination.clone()))
            .is_some_and(|edge| receipt.matches_edge(edge)),
    });
}

#[cfg(test)]
mod tests;
