use crate::{
    CAPTURE_SCHEMA_VERSION, Capture, CaptureError, CaptureKind, CaptureLimits, CaptureManifest,
    FileEntry, GitFile, MAX_MANIFEST_BYTES, Result, SourceId, assemble_capture, validate_path,
};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, OpenOptions};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

fn selected_paths<'a>(
    source: &SourceId,
    paths: &'a [String],
    limits: CaptureLimits,
) -> Result<Vec<&'a str>> {
    limits.validate()?;
    if source.as_str().len() > limits.max_source_id_bytes {
        return Err(CaptureError::Limit("source identity bytes"));
    }
    if paths.len() > limits.max_files {
        return Err(CaptureError::Limit("selected files"));
    }
    let mut selected = BTreeSet::new();
    for path in paths {
        validate_path(path, limits.max_path_bytes)?;
        if !selected.insert(path.as_str()) {
            return Err(CaptureError::Invalid("duplicate selected path"));
        }
    }
    Ok(selected.into_iter().collect())
}

fn remaining_budget(total: u64, limits: CaptureLimits) -> u64 {
    limits
        .max_file_bytes
        .min(limits.max_total_bytes.saturating_sub(total))
}

fn entry(path: &str, bytes: &[u8], git: Option<GitFile>) -> FileEntry {
    FileEntry {
        path: path.to_string(),
        digest: blake3::hash(bytes).to_hex().to_string(),
        byte_len: bytes.len() as u64,
        git,
    }
}

fn read_selected(root: &Dir, path: &str, max_bytes: u64) -> Result<Vec<u8>> {
    // Each directory is opened by a single component with no-follow. A final
    // no-follow option on a multi-component path would not reject its parents.
    let mut directory = root.try_clone()?;
    let mut components = path.split('/').peekable();
    while let Some(component) = components.next() {
        if components.peek().is_some() {
            directory = directory.open_dir_nofollow(component)?;
            continue;
        }
        // Reject known special entries before opening. The nonblocking open
        // and opened-handle type check also cover a regular→FIFO replacement
        // between this observation and the open; no data read precedes them.
        if !directory.symlink_metadata(component)?.file_type().is_file() {
            return Err(CaptureError::Invalid(
                "selected entry is not a regular file",
            ));
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let file = directory.open_with(component, &options)?;
        let metadata = file.metadata()?;
        if !metadata.file_type().is_file() {
            return Err(CaptureError::Invalid("opened entry is not a regular file"));
        }
        if metadata.len() > max_bytes {
            return Err(CaptureError::Limit("file or capture bytes"));
        }
        let mut bytes = Vec::new();
        // Bounds apply to bytes actually read even when a concurrent writer
        // grows the file after metadata inspection. The extra byte detects it.
        file.take(max_bytes + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > max_bytes {
            return Err(CaptureError::Limit("file or capture bytes"));
        }
        return Ok(bytes);
    }
    Err(CaptureError::Invalid("empty selected path"))
}

/// Acquire only explicitly selected regular files beneath a trusted host root.
///
/// Symlinks at every selected component are refused. Bytes are neither decoded
/// nor normalized; this captures the acquired sequence, not an atomic filesystem
/// snapshot. Any failed file aborts the entire capture.
pub fn capture_working_tree(
    root: &Path,
    source: &SourceId,
    paths: &[String],
    limits: CaptureLimits,
) -> Result<Capture> {
    let selected = selected_paths(source, paths, limits)?;
    let directory = Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
    let mut files = Vec::with_capacity(selected.len());
    let mut buffers = BTreeMap::new();
    let mut total = 0u64;
    for path in selected {
        let bytes = read_selected(&directory, path, remaining_budget(total, limits))?;
        total += bytes.len() as u64;
        files.push(entry(path, &bytes, None));
        buffers.insert(path.to_string(), bytes);
    }
    assemble_capture(
        CaptureManifest {
            schema_version: CAPTURE_SCHEMA_VERSION,
            source_id: source.clone(),
            kind: CaptureKind::WorkingTree,
            files,
        },
        buffers,
        limits,
    )
}

/// Acquire selected regular blobs from an exact full commit in a local Git repo.
///
/// Does not checkout, use shell Git, execute filters/hooks, discover credentials,
/// or fetch. Missing local objects fail without fallback. No HEAD lookup occurs.
pub fn capture_git(
    repo_path: &Path,
    source: &SourceId,
    commit: &str,
    paths: &[String],
    limits: CaptureLimits,
) -> Result<Capture> {
    let selected = selected_paths(source, paths, limits)?;
    if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CaptureError::Invalid("full SHA-1 commit required"));
    }
    let repo = git2::Repository::open_ext(
        repo_path,
        git2::RepositoryOpenFlags::NO_SEARCH,
        std::iter::empty::<&std::ffi::OsStr>(),
    )?;
    let oid = git2::Oid::from_str(commit)?;
    let odb = repo.odb()?;
    checked_header(&odb, oid, git2::ObjectType::Commit, MAX_MANIFEST_BYTES)?;
    let commit_object = repo.find_commit(oid)?;
    let tree_id = commit_object.tree_id();
    checked_header(&odb, tree_id, git2::ObjectType::Tree, MAX_MANIFEST_BYTES)?;
    let kind = CaptureKind::Git {
        commit: oid.to_string(),
        tree: tree_id.to_string(),
    };
    let mut files = Vec::with_capacity(selected.len());
    let mut buffers = BTreeMap::new();
    let mut total = 0u64;
    for path in selected {
        let (blob_id, mode) = selected_blob(&repo, &odb, tree_id, path)?;
        let remaining = remaining_budget(total, limits);
        // This bounds selected decoded objects, not libgit2's process memory:
        // pack/delta reconstruction may require additional internal resources.
        let len = checked_header(&odb, blob_id, git2::ObjectType::Blob, remaining)?;
        let blob = repo.find_blob(blob_id)?;
        if blob.content().len() != len || blob.content().len() as u64 > remaining {
            return Err(CaptureError::Corrupt("Git object length changed"));
        }
        let bytes = blob.content().to_vec();
        total += bytes.len() as u64;
        files.push(entry(
            path,
            &bytes,
            Some(GitFile {
                oid: blob_id.to_string(),
                mode,
            }),
        ));
        buffers.insert(path.to_string(), bytes);
    }
    assemble_capture(
        CaptureManifest {
            schema_version: CAPTURE_SCHEMA_VERSION,
            source_id: source.clone(),
            kind,
            files,
        },
        buffers,
        limits,
    )
}

fn checked_header(
    odb: &git2::Odb<'_>,
    oid: git2::Oid,
    kind: git2::ObjectType,
    limit: u64,
) -> Result<usize> {
    let (len, actual_kind) = odb.read_header(oid)?;
    if actual_kind != kind {
        return Err(CaptureError::Corrupt("Git object type mismatch"));
    }
    if len as u64 > limit {
        return Err(CaptureError::Limit("Git object bytes"));
    }
    Ok(len)
}

fn selected_blob(
    repo: &git2::Repository,
    odb: &git2::Odb<'_>,
    root_tree: git2::Oid,
    path: &str,
) -> Result<(git2::Oid, u32)> {
    let mut tree_id = root_tree;
    let mut components = path.split('/').peekable();
    while let Some(component) = components.next() {
        checked_header(odb, tree_id, git2::ObjectType::Tree, MAX_MANIFEST_BYTES)?;
        let tree = repo.find_tree(tree_id)?;
        let entry = tree
            .get_name(component)
            .ok_or(CaptureError::Missing("selected Git path"))?;
        let mode = entry.filemode_raw() as u32;
        if components.peek().is_some() {
            if entry.kind() != Some(git2::ObjectType::Tree) || mode != 0o040000 {
                return Err(CaptureError::Invalid(
                    "Git path traverses a non-directory entry",
                ));
            }
            tree_id = entry.id();
        } else {
            if entry.kind() != Some(git2::ObjectType::Blob) || !matches!(mode, 0o100644 | 0o100755)
            {
                return Err(CaptureError::Invalid(
                    "selected Git entry is not a regular blob",
                ));
            }
            return Ok((entry.id(), mode));
        }
    }
    Err(CaptureError::Invalid("empty selected Git path"))
}
