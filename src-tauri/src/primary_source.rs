//! Retained primary-source evidence; independent of tier and proposal review.

use adapters_lang_ts::captured::{self, Receipt};
use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, DirBuilder, OpenOptions};
use core_graph::source::{FactKey, SourceBinding};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use source_capture::{Capture, CaptureLimits, CaptureStore, SourceId, StoreLimits};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::Manager;

use crate::AppState;
use crate::sources::RegisteredSource;

const MAX_RECEIPTS: u64 = 100_000;
const MAX_RECEIPT_BYTES: u64 = 128 * 1024;
const MAX_RECEIPT_STORE_BYTES: u64 = 64 * 1024 * 1024;
const RECEIPTS_SCHEMA: &str = "CREATE TABLE receipts (id TEXT PRIMARY KEY NOT NULL, source_id TEXT NOT NULL, repo_key TEXT NOT NULL, payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) <= 131072)) STRICT";
const META_SCHEMA: &str =
    "CREATE TABLE receipt_meta (version INTEGER PRIMARY KEY CHECK(version = 1)) STRICT";

fn unavailable() -> String {
    "Captured primary source is unavailable; recover this source again or inspect the separately labeled current working tree.".into()
}

fn storage_error(_: impl std::fmt::Display) -> String {
    "Retained source storage is unavailable or invalid; no source was substituted.".into()
}

fn source_id(value: &str) -> Result<SourceId, String> {
    if value.len() != 36
        || !value.starts_with("src_")
        || !value[4..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("Invalid registered source identity".into());
    }
    SourceId::new(value).map_err(storage_error)
}

fn same_entry(a: &cap_std::fs::Metadata, b: &cap_std::fs::Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

fn private_dir(parent: &Dir, name: &str) -> Result<Dir, String> {
    let mut builder = DirBuilder::new();
    #[cfg(unix)]
    {
        use cap_std::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match parent.create_dir_with(name, &builder) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(storage_error(error)),
    }
    let directory = parent.open_dir_nofollow(name).map_err(storage_error)?;
    let metadata = directory.dir_metadata().map_err(storage_error)?;
    let named = parent.symlink_metadata(name).map_err(storage_error)?;
    if !named.is_dir() || !same_entry(&metadata, &named) {
        return Err(storage_error("private directory changed"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        directory
            .try_clone()
            .map_err(storage_error)?
            .into_std_file()
            .set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(storage_error)?;
    }
    Ok(directory)
}

fn private_file(parent: &Dir, name: &std::ffi::OsStr, create: bool) -> Result<File, String> {
    let prior = match parent.symlink_metadata(name) {
        Ok(metadata) if metadata.is_file() && metadata.nlink() == 1 => Some(metadata),
        Ok(_) => return Err(storage_error("nonregular private file")),
        Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(storage_error(error)),
    };
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .follow(FollowSymlinks::No)
        .nonblock(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // A racing creator is handled by the same no-follow open and post-open
    // identity check; an existing file is never truncated or recreated.
    let file = if prior.is_none() {
        options.create_new(true);
        match parent.open_with(name, &options) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                options.create_new(false);
                parent.open_with(name, &options).map_err(storage_error)?
            }
            Err(error) => return Err(storage_error(error)),
        }
    } else {
        parent.open_with(name, &options).map_err(storage_error)?
    };
    let opened = file.metadata().map_err(storage_error)?;
    let named = parent.symlink_metadata(name).map_err(storage_error)?;
    if !opened.is_file()
        || opened.nlink() != 1
        || !named.is_file()
        || !same_entry(&opened, &named)
        || prior.is_some_and(|prior| !same_entry(&prior, &opened))
    {
        return Err(storage_error("private file changed"));
    }
    let file = file.into_std();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(storage_error)?;
    }
    Ok(file)
}

fn sqlite_sidecars(directory: &Dir, name: &str) -> Result<(), String> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = format!("{name}{suffix}");
        match directory.symlink_metadata(&sidecar) {
            Ok(_) => {
                drop(private_file(directory, sidecar.as_ref(), false)?);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(storage_error(error)),
        }
    }
    Ok(())
}

/// Retained directory and database handles detect substitutions between host
/// operations. SQLite itself still opens sidecars by pathname: this is private
/// same-user application storage, not a sandbox against arbitrary concurrent
/// mutation by another process with that user's filesystem authority.
struct PrivateStorage {
    app_path: PathBuf,
    app: Dir,
    root: Dir,
    locks: Dir,
    databases: [File; 2],
}

impl PrivateStorage {
    fn open(app_data: &Path) -> Result<Self, String> {
        let app_path = dunce::canonicalize(app_data).map_err(storage_error)?;
        let app = Dir::open_ambient_dir(&app_path, cap_std::ambient_authority())
            .map_err(storage_error)?;
        let root = private_dir(&app, "retained-source")?;
        let locks = private_dir(&root, "locks")?;
        let databases = [
            private_file(&root, "captures.sqlite".as_ref(), true)?,
            private_file(&root, "receipts.sqlite".as_ref(), true)?,
        ];
        let storage = Self {
            app_path,
            app,
            root,
            locks,
            databases,
        };
        storage.verify()?;
        Ok(storage)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.app_path.join("retained-source").join(name)
    }

    fn verify(&self) -> Result<(), String> {
        if dunce::canonicalize(&self.app_path).map_err(storage_error)? != self.app_path {
            return Err(storage_error("application directory changed"));
        }
        let current_app = Dir::open_ambient_dir(&self.app_path, cap_std::ambient_authority())
            .map_err(storage_error)?;
        let current_root = self
            .app
            .open_dir_nofollow("retained-source")
            .map_err(storage_error)?;
        let current_locks = self
            .root
            .open_dir_nofollow("locks")
            .map_err(storage_error)?;
        for (expected, current, private) in [
            (&self.app, &current_app, false),
            (&self.root, &current_root, true),
            (&self.locks, &current_locks, true),
        ] {
            let metadata = current.dir_metadata().map_err(storage_error)?;
            if !same_entry(&expected.dir_metadata().map_err(storage_error)?, &metadata) {
                return Err(storage_error("private directory substituted"));
            }
            #[cfg(unix)]
            if private {
                use cap_std::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o077 != 0 {
                    return Err(storage_error("private directory permissions"));
                }
            }
            #[cfg(not(unix))]
            let _ = private;
        }
        for (name, expected) in ["captures.sqlite", "receipts.sqlite"]
            .into_iter()
            .zip(&self.databases)
        {
            let current = private_file(&self.root, name.as_ref(), false)?;
            let a = current.metadata().map_err(storage_error)?;
            let b = expected.metadata().map_err(storage_error)?;
            if a.dev() != b.dev() || a.ino() != b.ino() {
                return Err(storage_error("private database substituted"));
            }
            sqlite_sidecars(&self.root, name)?;
        }
        Ok(())
    }
}

/// A fresh OS handle, never unlinked/replaced as part of retention cleanup.
pub(crate) struct RetentionGuard {
    _file: File,
}

impl Drop for RetentionGuard {
    fn drop(&mut self) {
        // This fresh handle is never intentionally shared with descendants.
        // Explicit release also ends the lock if an unrelated fork briefly
        // inherited the open-file description; close remains the fallback.
        let _ = self._file.unlock();
    }
}

struct ReceiptStore {
    conn: Connection,
}

fn require_one(affected: usize) -> Result<(), String> {
    if affected != 1 {
        return Err(storage_error("receipt publication row count"));
    }
    Ok(())
}

fn receipt_schema(conn: &Connection) -> Result<(), String> {
    let objects: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name NOT GLOB 'sqlite_*'",
            [],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    if objects != 2 {
        return Err(storage_error("receipt schema objects"));
    }
    for (name, ddl) in [("receipts", RECEIPTS_SCHEMA), ("receipt_meta", META_SCHEMA)] {
        let exact: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1 AND sql=?2)",
            params![name,ddl], |row| row.get(0)
        ).map_err(storage_error)?;
        if !exact {
            return Err(storage_error("receipt schema"));
        }
    }
    let valid: bool = conn.query_row(
        "SELECT count(*)=1 AND coalesce(min(typeof(version)='integer' AND version=1),0) FROM receipt_meta",
        [], |row| row.get(0)
    ).map_err(storage_error)?;
    if !valid {
        return Err(storage_error("receipt version"));
    }
    Ok(())
}

// Metadata-only preallocation checks apply even after open; indexed ownership
// fields are not trusted just because a SQL predicate selected their row.
fn receipt_storage(conn: &Connection) -> Result<(u64, u64), String> {
    receipt_schema(conn)?;
    let count: i64 = conn
        .query_row("SELECT count(*) FROM receipts", [], |row| row.get(0))
        .map_err(storage_error)?;
    if count < 0 || count > MAX_RECEIPTS as i64 {
        return Err(storage_error("receipt inventory count"));
    }
    let invalid: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM receipts WHERE typeof(id) != 'text'
         OR length(CAST(id AS BLOB)) NOT BETWEEN 1 AND 256
         OR typeof(source_id) != 'text' OR length(CAST(source_id AS BLOB)) != 36
         OR typeof(repo_key) != 'text' OR length(CAST(repo_key AS BLOB)) NOT BETWEEN 1 AND 256
         OR typeof(payload) != 'text' OR length(CAST(payload AS BLOB)) NOT BETWEEN 1 AND ?1)",
            [MAX_RECEIPT_BYTES as i64],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    if invalid {
        return Err(storage_error("receipt metadata bounds"));
    }
    let bytes: i64 = conn
        .query_row(
            "SELECT coalesce(sum(length(CAST(payload AS BLOB))),0) FROM receipts",
            [],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    if bytes < 0 || bytes > MAX_RECEIPT_STORE_BYTES as i64 {
        return Err(storage_error("receipt inventory bytes"));
    }
    // SQLite integers are signed; convert only after the nonnegative bounds.
    Ok((count as u64, bytes as u64))
}

// The caller retains a transaction after receipt_storage validated all lengths.
fn stored_receipt(conn: &Connection, id: &str) -> Result<Receipt, String> {
    let (source, repo, json): (String, String, String) = conn
        .query_row(
            "SELECT source_id,repo_key,payload FROM receipts WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|_| unavailable())?;
    let receipt = Receipt::from_json(&json).map_err(|_| unavailable())?;
    if receipt.id() != id
        || receipt.source_id().as_str() != source
        || receipt.repo_key() != repo
        || receipt.to_json().map_err(|_| unavailable())? != json
    {
        return Err(unavailable());
    }
    Ok(receipt)
}

impl ReceiptStore {
    fn open(path: &Path) -> Result<Self, String> {
        let parent_path =
            dunce::canonicalize(path.parent().ok_or_else(unavailable)?).map_err(storage_error)?;
        let parent = Dir::open_ambient_dir(&parent_path, cap_std::ambient_authority())
            .map_err(storage_error)?;
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(unavailable)?;
        let checked = private_file(&parent, name.as_ref(), true)?;
        sqlite_sidecars(&parent, name)?;
        let mut conn = Connection::open_with_flags(
            parent_path.join(name),
            OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(storage_error)?;
        conn.busy_timeout(std::time::Duration::from_secs(1))
            .map_err(storage_error)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(storage_error)?;
        conn.pragma_update(None, "synchronous", "FULL")
            .map_err(storage_error)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let count: i64 = tx
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name NOT GLOB 'sqlite_*'",
                [],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        if count == 0 {
            tx.execute_batch(RECEIPTS_SCHEMA).map_err(storage_error)?;
            tx.execute_batch(META_SCHEMA).map_err(storage_error)?;
            require_one(
                tx.execute("INSERT INTO receipt_meta VALUES (1)", [])
                    .map_err(storage_error)?,
            )?;
        }
        receipt_storage(&tx)?;
        tx.commit().map_err(storage_error)?;
        let current = private_file(&parent, name.as_ref(), false)?;
        let a = checked.metadata().map_err(storage_error)?;
        let b = current.metadata().map_err(storage_error)?;
        if a.dev() != b.dev() || a.ino() != b.ino() {
            return Err(storage_error("receipt database changed"));
        }
        sqlite_sidecars(&parent, name)?;
        Ok(Self { conn })
    }

    fn persist(
        &mut self,
        capture: &Capture,
        source: &RegisteredSource,
        receipts: &[Receipt],
    ) -> Result<(), String> {
        if receipts.len() as u64 > MAX_RECEIPTS {
            return Err(storage_error("receipt batch count"));
        }
        let mut rows = std::collections::BTreeMap::new();
        let mut batch_bytes = 0u64;
        for receipt in receipts {
            if receipt.source_id().as_str() != source.source_id
                || receipt.repo_key() != source.repo_key
            {
                return Err(storage_error("receipt ownership"));
            }
            if capture
                .file(&receipt.file().path)
                .map_err(storage_error)?
                .reference()
                != receipt.file()
            {
                return Err(storage_error("receipt capture"));
            }
            for range in receipt.ranges() {
                capture.read_span(&range.captured).map_err(storage_error)?;
            }
            let json = receipt.to_json().map_err(storage_error)?;
            if json.len() as u64 > MAX_RECEIPT_BYTES {
                return Err(storage_error("receipt bounds"));
            }
            if let Some(old) = rows.get(receipt.id()) {
                if old != &json {
                    return Err(storage_error("receipt batch collision"));
                }
            } else {
                batch_bytes += json.len() as u64;
                if batch_bytes > MAX_RECEIPT_STORE_BYTES {
                    return Err(storage_error("receipt batch bytes"));
                }
                rows.insert(receipt.id().to_owned(), json);
            }
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let (mut count, mut bytes) = receipt_storage(&tx)?;
        for (id, payload) in rows {
            let old: Option<(String, String, String)> = tx
                .query_row(
                    "SELECT source_id,repo_key,payload FROM receipts WHERE id=?1",
                    [&id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(storage_error)?;
            if let Some((old_source, old_repo, old_json)) = old {
                if old_source != source.source_id
                    || old_repo != source.repo_key
                    || old_json != payload
                {
                    return Err(storage_error("receipt collision"));
                }
            } else {
                count += 1;
                bytes += payload.len() as u64;
                if count > MAX_RECEIPTS || bytes > MAX_RECEIPT_STORE_BYTES {
                    return Err("Retained receipt metadata capacity reached. Existing evidence is preserved; this recovery was not published.".into());
                }
                require_one(
                    tx.execute(
                        "INSERT INTO receipts (id,source_id,repo_key,payload) VALUES (?1,?2,?3,?4)",
                        params![id, source.source_id, source.repo_key, payload],
                    )
                    .map_err(storage_error)?,
                )?;
            }
        }
        tx.commit().map_err(storage_error)
    }

    fn get(&self, id: &str) -> Result<Receipt, String> {
        if id.is_empty() || id.len() > 256 {
            return Err(unavailable());
        }
        let tx = self.conn.unchecked_transaction().map_err(storage_error)?;
        receipt_storage(&tx)?;
        let receipt = stored_receipt(&tx, id)?;
        tx.commit().map_err(storage_error)?;
        Ok(receipt)
    }

    fn ids(&self, source: &str) -> Result<Vec<String>, String> {
        source_id(source)?;
        let tx = self.conn.unchecked_transaction().map_err(storage_error)?;
        receipt_storage(&tx)?;
        let mut ids = Vec::new();
        {
            let mut statement = tx
                .prepare("SELECT id FROM receipts ORDER BY id")
                .map_err(storage_error)?;
            let mut rows = statement.query([]).map_err(storage_error)?;
            while let Some(row) = rows.next().map_err(storage_error)? {
                let id: String = row.get(0).map_err(storage_error)?;
                let receipt = stored_receipt(&tx, &id)?;
                if receipt.source_id().as_str() == source {
                    ids.push(id);
                }
            }
        }
        tx.commit().map_err(storage_error)?;
        Ok(ids)
    }
}

/// Private stores are locked only for short operations, never while parsing.
pub(crate) struct PrimarySourceStore {
    captures: Mutex<CaptureStore>,
    receipts: Mutex<ReceiptStore>,
    storage: PrivateStorage,
}

impl PrimarySourceStore {
    pub(crate) fn open(app_data: &Path) -> Result<Self, String> {
        let storage = PrivateStorage::open(app_data)?;
        let captures = CaptureStore::open(&storage.path("captures.sqlite"), StoreLimits::default())
            .map_err(storage_error)?;
        let receipts = ReceiptStore::open(&storage.path("receipts.sqlite"))?;
        storage.verify()?;
        Ok(Self {
            captures: Mutex::new(captures),
            receipts: Mutex::new(receipts),
            storage,
        })
    }

    fn guard(&self, source: &str, exclusive: bool) -> Result<RetentionGuard, String> {
        source_id(source)?;
        self.storage.verify()?;
        let file = private_file(&self.storage.locks, format!("{source}.lock").as_ref(), true)?;
        let result = if exclusive {
            file.try_lock()
        } else {
            file.try_lock_shared()
        };
        result.map_err(|_| "Retained source is busy or locking is unavailable; retry after the active operation finishes.".to_string())?;
        let guard = RetentionGuard { _file: file };
        self.storage.verify()?;
        Ok(guard)
    }

    pub(crate) fn prepare(
        &self,
        source: &RegisteredSource,
        root: &Path,
        layers: &[String],
    ) -> Result<PrimaryInput, String> {
        let wanted = layers.is_empty()
            || layers
                .iter()
                .any(|layer| ["server", "events", "client"].contains(&layer.as_str()));
        if !wanted {
            return Ok(PrimaryInput {
                capture: None,
                _guard: None,
            });
        }
        let guard = self.guard(&source.source_id, false)?;
        let paths = captured::enumerate_paths(root).map_err(|_| "Selected TS/JS source could not be captured safely within the supported path and size limits.".to_string())?;
        let capture = source_capture::capture_working_tree(root, &source_id(&source.source_id)?, &paths, CaptureLimits::default()).map_err(|_| "Selected TS/JS source could not be retained within the capture limits; recovery was not published.".to_string())?;
        Ok(PrimaryInput {
            capture: Some(capture),
            _guard: Some(guard),
        })
    }

    pub(crate) fn persist(
        &self,
        input: &PrimaryInput,
        source: &RegisteredSource,
        receipts: &[Receipt],
    ) -> Result<(), String> {
        self.storage.verify()?;
        if let Some(capture) = &input.capture {
            self.captures.lock().map_err(storage_error)?.persist(capture).map_err(|_| "Source retention failed or reached capacity. Use retained-source controls to free space; recovery was not published.".to_string())?;
            self.receipts
                .lock()
                .map_err(storage_error)?
                .persist(capture, source, receipts)?;
        } else if !receipts.is_empty() {
            return Err(storage_error("uncaptured receipts"));
        }
        Ok(())
    }
}

pub(crate) struct PrimaryInput {
    pub capture: Option<Capture>,
    _guard: Option<RetentionGuard>,
}

pub(crate) fn matching_bindings(
    extraction: &adapters_lang_ts::Extraction,
    receipts: &[Receipt],
) -> Vec<SourceBinding> {
    // Match the same last-value winners that graph publication materializes.
    let nodes: std::collections::BTreeMap<_, _> = extraction
        .nodes
        .iter()
        .map(|node| (&node.id, node))
        .collect();
    let edges: std::collections::BTreeMap<_, _> = extraction
        .edges
        .iter()
        .map(|edge| ((&edge.src, &edge.label, &edge.dst), edge))
        .collect();
    receipts
        .iter()
        .filter(|receipt| match receipt.fact_key() {
            FactKey::Node { id } => nodes.get(id).is_some_and(|node| receipt.matches_node(node)),
            FactKey::Edge {
                source,
                label,
                destination,
            } => edges
                .get(&(source, label, destination))
                .is_some_and(|edge| receipt.matches_edge(edge)),
        })
        .map(|receipt| {
            (
                receipt.fact_key().clone(),
                SourceBinding {
                    fact: receipt.fact_key().clone(),
                    repo_key: receipt.repo_key().to_owned(),
                    receipt_id: receipt.id().to_owned(),
                    emitted_fact_digest: receipt.fact_digest().to_owned(),
                },
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>()
        .into_values()
        .collect()
}

#[derive(Serialize)]
pub(crate) struct CapturedRange {
    pub index: usize,
    pub path: String,
    pub byte_start: u64,
    pub byte_end: u64,
}

#[derive(Serialize)]
pub(crate) struct CapturedDescription {
    pub fact: FactKey,
    pub receipt_id: String,
    pub emitted_fact_digest: String,
    pub source_id: String,
    pub repo_key: String,
    pub ranges: Vec<CapturedRange>,
    pub scope: &'static str,
    pub input_closure: &'static str,
}

fn bound_receipt(
    state: &AppState,
    fact: &FactKey,
    expected: Option<&str>,
) -> Result<(Receipt, RetentionGuard), String> {
    // Preliminary metadata locates the guard; release all mutexes before the
    // try-only OS lock, then make the authoritative current-binding check.
    let preliminary = state
        .graph
        .lock()
        .map_err(storage_error)?
        .current_source_binding(fact)
        .map_err(storage_error)?
        .ok_or_else(unavailable)?;
    let source = state
        .sources
        .lock()
        .map_err(storage_error)?
        .get_by_repo(&preliminary.repo_key)?
        .ok_or_else(unavailable)?;
    let guard = state.primary_sources.guard(&source.source_id, false)?;
    let binding = state
        .graph
        .lock()
        .map_err(storage_error)?
        .current_source_binding(fact)
        .map_err(storage_error)?
        .ok_or_else(unavailable)?;
    if binding.repo_key != source.repo_key {
        return Err(unavailable());
    }
    if expected.is_some_and(|id| id != binding.receipt_id) {
        return Err("Captured source selection is stale; select the fact again.".into());
    }
    let receipt = state
        .primary_sources
        .receipts
        .lock()
        .map_err(storage_error)?
        .get(&binding.receipt_id)?;
    if receipt.source_id().as_str() != source.source_id
        || receipt.repo_key() != source.repo_key
        || receipt.fact_key() != fact
        || receipt.fact_digest() != binding.emitted_fact_digest
    {
        return Err(unavailable());
    }
    Ok((receipt, guard))
}

pub(crate) fn describe(state: &AppState, fact: &FactKey) -> Result<CapturedDescription, String> {
    let (receipt, _guard) = bound_receipt(state, fact, None)?;
    Ok(CapturedDescription {
        fact: receipt.fact_key().clone(),
        receipt_id: receipt.id().to_owned(),
        emitted_fact_digest: receipt.fact_digest().to_owned(),
        source_id: receipt.source_id().as_str().to_owned(),
        repo_key: receipt.repo_key().to_owned(),
        ranges: receipt
            .ranges()
            .iter()
            .enumerate()
            .map(|(index, range)| CapturedRange {
                index,
                path: range.evidence.path.clone(),
                byte_start: range.evidence.byte_start,
                byte_end: range.evidence.byte_end,
            })
            .collect(),
        scope: "primary_source_only",
        input_closure: "input_closure_not_established",
    })
}

#[derive(Serialize)]
pub(crate) struct CapturedText {
    pub receipt_id: String,
    pub range_index: usize,
    pub text: String,
    pub path: String,
    pub byte_start: u64,
    pub byte_end: u64,
}

pub(crate) fn read(
    state: &AppState,
    fact: &FactKey,
    receipt_id: &str,
    range_index: usize,
) -> Result<CapturedText, String> {
    let (receipt, _guard) = bound_receipt(state, fact, Some(receipt_id))?;
    let range = receipt.ranges().get(range_index).ok_or_else(unavailable)?;
    let text = state
        .primary_sources
        .captures
        .lock()
        .map_err(storage_error)?
        .read_text_span(&range.captured)
        .map_err(|_| unavailable())?;
    Ok(CapturedText {
        receipt_id: receipt.id().to_owned(),
        range_index,
        text,
        path: range.evidence.path.clone(),
        byte_start: range.evidence.byte_start,
        byte_end: range.evidence.byte_end,
    })
}

#[derive(Serialize)]
pub(crate) struct RetainedSource {
    pub source_id: String,
    pub repo_key: String,
    pub display_name: String,
}

#[derive(Serialize)]
pub(crate) struct RetentionPreview {
    pub source_id: String,
    pub repo_key: String,
    pub display_name: String,
    pub capture_ids: Vec<String>,
    pub captures: usize,
    pub files: u64,
    pub bytes: u64,
    pub receipts: usize,
    pub current_references: usize,
    pub historical_references: usize,
    pub fingerprint: String,
}

fn preview_locked(state: &AppState, source: &RegisteredSource) -> Result<RetentionPreview, String> {
    let inventory = state
        .primary_sources
        .captures
        .lock()
        .map_err(storage_error)?
        .source_inventory(&source_id(&source.source_id)?)
        .map_err(storage_error)?;
    let receipt_ids = state
        .primary_sources
        .receipts
        .lock()
        .map_err(storage_error)?
        .ids(&source.source_id)?;
    let current = state
        .graph
        .lock()
        .map_err(storage_error)?
        .source_bindings_for_repo(&source.repo_key)
        .map_err(storage_error)?;
    let current_count = current
        .iter()
        .filter(|binding| receipt_ids.binary_search(&binding.receipt_id).is_ok())
        .count();
    // Bind the complete associations, not only their count: retained receipts
    // can become current again without changing the capture/receipt inventory.
    // This records the associations observed at preview/recheck; it does not
    // freeze independent graph mutations after the recheck.
    let mut current_bindings: Vec<_> = current
        .iter()
        .map(|binding| {
            (
                &binding.fact,
                &binding.repo_key,
                &binding.receipt_id,
                &binding.emitted_fact_digest,
            )
        })
        .collect();
    current_bindings.sort_unstable();
    let capture_ids: Vec<_> = inventory
        .iter()
        .map(|capture| capture.capture_id.clone())
        .collect();
    let fingerprint = core_prov::content_hash(
        &serde_json::to_vec(&(
            "retained-source-preview-v1",
            &source.source_id,
            &source.repo_key,
            &capture_ids,
            &receipt_ids,
            &current_bindings,
        ))
        .map_err(storage_error)?,
    );
    Ok(RetentionPreview {
        source_id: source.source_id.clone(),
        repo_key: source.repo_key.clone(),
        display_name: source.display_name.clone(),
        captures: inventory.len(),
        files: inventory.iter().map(|capture| capture.file_count).sum(),
        bytes: inventory.iter().map(|capture| capture.byte_len).sum(),
        receipts: receipt_ids.len(),
        current_references: current_count,
        historical_references: receipt_ids.len().saturating_sub(current_count),
        capture_ids,
        fingerprint,
    })
}

fn registered(state: &AppState, id: &str) -> Result<RegisteredSource, String> {
    source_id(id)?;
    state
        .sources
        .lock()
        .map_err(storage_error)?
        .get_by_id(id)?
        .ok_or_else(unavailable)
}

pub(crate) fn preview(state: &AppState, id: &str) -> Result<RetentionPreview, String> {
    let source = registered(state, id)?;
    let _guard = state.primary_sources.guard(id, false)?;
    preview_locked(state, &source)
}

pub(crate) fn forget(state: &AppState, id: &str, fingerprint: &str) -> Result<u64, String> {
    let source = registered(state, id)?;
    let _guard = state.primary_sources.guard(id, true)?;
    let preview = preview_locked(state, &source)?;
    if preview.fingerprint != fingerprint {
        return Err("Retained source changed; refresh the preview before forgetting.".into());
    }
    state
        .primary_sources
        .captures
        .lock()
        .map_err(storage_error)?
        .forget_source(&source_id(id)?, &preview.capture_ids)
        .map_err(storage_error)
}

#[tauri::command]
pub(crate) async fn describe_captured_source(
    fact: FactKey,
    expected_node: Option<core_graph::Node>,
    expected_edge: Option<core_graph::Edge>,
    app: tauri::AppHandle,
) -> Result<CapturedDescription, String> {
    crate::off_ui_thread(move || {
        let expected_digest = match (&fact, expected_node, expected_edge) {
            (FactKey::Node { id }, Some(node), None) if node.id == *id => {
                core_graph::source::node_digest(&node).map_err(storage_error)?
            }
            (
                FactKey::Edge {
                    source,
                    label,
                    destination,
                },
                None,
                Some(edge),
            ) if edge.src == *source && edge.label == *label && edge.dst == *destination => {
                core_graph::source::edge_digest(&edge).map_err(storage_error)?
            }
            _ => return Err(unavailable()),
        };
        let description = describe(&app.state::<AppState>(), &fact)?;
        if description.emitted_fact_digest != expected_digest {
            return Err(
                "Selected fact changed; refresh the graph before inspecting captured source."
                    .into(),
            );
        }
        Ok(description)
    })
    .await
}

#[tauri::command]
pub(crate) async fn read_captured_source(
    fact: FactKey,
    receipt_id: String,
    range_index: usize,
    app: tauri::AppHandle,
) -> Result<CapturedText, String> {
    crate::off_ui_thread(move || read(&app.state::<AppState>(), &fact, &receipt_id, range_index))
        .await
}

#[tauri::command]
pub(crate) async fn list_retained_sources(
    app: tauri::AppHandle,
) -> Result<Vec<RetainedSource>, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<AppState>();
        let sources = state.sources.lock().map_err(storage_error)?.list()?;
        Ok(sources
            .into_iter()
            .map(|source| RetainedSource {
                source_id: source.source_id,
                repo_key: source.repo_key,
                display_name: source.display_name,
            })
            .collect())
    })
    .await
}

#[tauri::command]
pub(crate) async fn preview_forget_source(
    source_id: String,
    app: tauri::AppHandle,
) -> Result<RetentionPreview, String> {
    crate::off_ui_thread(move || preview(&app.state::<AppState>(), &source_id)).await
}

#[tauri::command]
pub(crate) async fn forget_retained_source(
    source_id: String,
    fingerprint: String,
    app: tauri::AppHandle,
) -> Result<u64, String> {
    crate::off_ui_thread(move || forget(&app.state::<AppState>(), &source_id, &fingerprint)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (
        tempfile::TempDir,
        RegisteredSource,
        Capture,
        adapters_lang_ts::Extraction,
        Vec<Receipt>,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(
            target.join("rule.ts"),
            b"export function ready(ok: boolean) { if (!ok) return false; return true; }",
        )
        .unwrap();
        let state_path = directory.path().join("state.sqlite");
        let _findings = crate::findings::FindingStore::open(&state_path).unwrap();
        let mut registry =
            crate::sources::SourceRegistry::open(&state_path, directory.path()).unwrap();
        let source = registry.register_local(&target).unwrap();
        let capture = source_capture::capture_working_tree(
            &target,
            &source_id(&source.source_id).unwrap(),
            &["rule.ts".into()],
            CaptureLimits::default(),
        )
        .unwrap();
        let (extraction, receipts) = captured::extract_file(
            capture.file("rule.ts").unwrap(),
            &adapters_lang_ts::SourceId {
                repo: &source.repo_key,
                commit: "workdir",
            },
        )
        .unwrap();
        assert!(!receipts.is_empty());
        (directory, source, capture, extraction, receipts)
    }

    fn receipt_count(store: &ReceiptStore) -> i64 {
        store
            .conn
            .query_row("SELECT count(*) FROM receipts", [], |row| row.get(0))
            .unwrap()
    }

    // AC-0153: schema/version validation remains active after open, and a
    // suppressed insert never becomes successful source-retention evidence.
    #[test]
    fn receipt_store_revalidates_schema_before_publication_and_reads() {
        let (_directory, source, capture, _, receipts) = fixture();
        for mutation in [
            "CREATE TRIGGER suppress_receipt BEFORE INSERT ON receipts BEGIN SELECT RAISE(IGNORE); END",
            "CREATE INDEX unexpected_receipt_index ON receipts(source_id)",
            "PRAGMA ignore_check_constraints=ON; UPDATE receipt_meta SET version=2",
            "DELETE FROM receipt_meta",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("receipts.sqlite");
            let mut store = ReceiptStore::open(&path).unwrap();
            store.persist(&capture, &source, &receipts).unwrap();
            let count = receipt_count(&store);
            store.conn.execute_batch(mutation).unwrap();
            assert!(store.persist(&capture, &source, &receipts).is_err());
            assert!(store.get(receipts[0].id()).is_err());
            assert!(store.ids(&source.source_id).is_err());
            assert_eq!(receipt_count(&store), count);
            drop(store);
            assert!(ReceiptStore::open(&path).is_err());
        }
    }

    // AC-0153: length/type prechecks reject oversized indexed columns and bodies
    // before loading them; diagnostics never copy corrupt payload contents.
    #[test]
    fn receipt_store_bounds_all_stored_columns_before_loading() {
        let (_directory, source, capture, _, receipts) = fixture();
        for mutation in [
            "UPDATE receipts SET id=printf('%0257d',0) WHERE id=?1",
            "UPDATE receipts SET source_id=printf('%0100000d',0) WHERE id=?1",
            "UPDATE receipts SET repo_key=printf('%0100000d',0) WHERE id=?1",
            "UPDATE receipts SET payload=printf('%0131073d',0) WHERE id=?1",
            "UPDATE receipts SET payload='do-not-echo-private-input' WHERE id=?1",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let mut store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
            store.persist(&capture, &source, &receipts).unwrap();
            let count = receipt_count(&store);
            store
                .conn
                .pragma_update(None, "ignore_check_constraints", true)
                .unwrap();
            store.conn.execute(mutation, [receipts[0].id()]).unwrap();
            let error = store
                .get(receipts[0].id())
                .expect_err("corrupt receipt must fail");
            assert!(!error.contains("do-not-echo-private-input"));
            assert!(store.ids(&source.source_id).is_err());
            assert!(store.persist(&capture, &source, &receipts).is_err());
            assert_eq!(receipt_count(&store), count);
        }
    }

    // AC-0153: SQL ownership indexes cannot hide a receipt from its actual
    // source's preview; every bounded payload is validated before filtering.
    #[test]
    fn receipt_inventory_rejects_hidden_ownership_and_retains_history() {
        let (_directory, source, capture, _, receipts) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("receipts.sqlite");
        let mut store = ReceiptStore::open(&path).unwrap();
        store.persist(&capture, &source, &receipts).unwrap();
        let expected = store.ids(&source.source_id).unwrap();
        drop(store);
        let store = ReceiptStore::open(&path).unwrap();
        assert_eq!(store.ids(&source.source_id).unwrap(), expected);
        let wrong_source = format!("src_{}", "f".repeat(32));
        assert_ne!(wrong_source, source.source_id);
        store
            .conn
            .execute(
                "UPDATE receipts SET source_id=?1 WHERE id=?2",
                params![wrong_source, receipts[0].id()],
            )
            .unwrap();
        assert!(store.ids(&source.source_id).is_err());
        assert!(store.ids(&wrong_source).is_err());
        assert_eq!(receipt_count(&store), expected.len() as i64);
    }

    // AC-0150: a later enrichment sharing an identity wins graph publication;
    // receipt matching follows that same final winner, never the first duplicate.
    #[test]
    fn primary_matching_uses_final_duplicate_fact_winners() {
        let (_directory, _, _, mut extraction, receipts) = fixture();
        let before = matching_bindings(&extraction, &receipts);
        assert!(!before.is_empty());
        for binding in &before {
            match &binding.fact {
                FactKey::Node { id } => {
                    let mut changed = extraction
                        .nodes
                        .iter()
                        .find(|node| &node.id == id)
                        .unwrap()
                        .clone();
                    changed.props["later_enrichment"] = serde_json::json!(true);
                    extraction.nodes.push(changed);
                }
                FactKey::Edge {
                    source,
                    label,
                    destination,
                } => {
                    let mut changed = extraction
                        .edges
                        .iter()
                        .find(|edge| {
                            &edge.src == source && &edge.label == label && &edge.dst == destination
                        })
                        .unwrap()
                        .clone();
                    changed.props["later_enrichment"] = serde_json::json!(true);
                    extraction.edges.push(changed);
                }
            }
        }
        assert!(matching_bindings(&extraction, &receipts).is_empty());
    }

    // AC-0153: private storage rejects known symlink/hardlink substitutions and
    // directory replacement. Fresh OS handles coordinate independent app opens.
    #[cfg(unix)]
    #[test]
    fn primary_storage_rejects_substitution_and_releases_guards() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let source = format!("src_{}", "a".repeat(32));
        let directory = tempfile::tempdir().unwrap();
        let store = PrimarySourceStore::open(directory.path()).unwrap();
        let other = PrimarySourceStore::open(directory.path()).unwrap();
        let shared = store.guard(&source, false).unwrap();
        assert!(other.guard(&source, true).is_err());
        drop(shared);
        drop(other.guard(&source, true).unwrap());
        for (path, mode) in [
            (directory.path().join("retained-source"), 0o700),
            (
                directory.path().join("retained-source/captures.sqlite"),
                0o600,
            ),
            (
                directory
                    .path()
                    .join(format!("retained-source/locks/{source}.lock")),
                0o600,
            ),
        ] {
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                mode
            );
        }
        let locks = directory.path().join("retained-source/locks");
        std::fs::rename(&locks, directory.path().join("old-locks")).unwrap();
        std::fs::create_dir(&locks).unwrap();
        std::fs::set_permissions(&locks, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(store.guard(&source, false).is_err());

        for name in [
            "captures.sqlite",
            "receipts.sqlite",
            "captures.sqlite-wal",
            "receipts.sqlite-shm",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().join("retained-source");
            std::fs::create_dir(&root).unwrap();
            let outside = directory.path().join("outside");
            std::fs::write(&outside, b"untouched").unwrap();
            symlink(&outside, root.join(name)).unwrap();
            assert!(PrimarySourceStore::open(directory.path()).is_err());
            assert_eq!(std::fs::read(&outside).unwrap(), b"untouched");
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("retained-source");
        std::fs::create_dir(&root).unwrap();
        let outside = directory.path().join("outside");
        std::fs::write(&outside, b"untouched").unwrap();
        std::fs::hard_link(&outside, root.join("captures.sqlite")).unwrap();
        assert!(PrimarySourceStore::open(directory.path()).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"untouched");
    }
}
