use crate::{
    Capture, CaptureError, CaptureFileRef, CaptureLimits, CaptureManifest, CaptureSpanRef,
    MAX_FILE_BYTES, MAX_MANIFEST_BYTES, MAX_SPAN_BYTES, Result, SourceId, assemble_capture,
    canonical_manifest, manifest_id, validate_capture_id, validate_manifest, validate_range,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, TransactionBehavior, params};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

const STORE_SCHEMA_VERSION: u32 = 2;
const CAPTURES_SCHEMA: &str = "CREATE TABLE captures (id TEXT PRIMARY KEY NOT NULL, manifest BLOB NOT NULL, max_span_bytes INTEGER NOT NULL CHECK (typeof(max_span_bytes) = 'integer' AND max_span_bytes BETWEEN 0 AND 262144))";
const OBJECTS_SCHEMA: &str =
    "CREATE TABLE objects (digest TEXT PRIMARY KEY NOT NULL, bytes BLOB NOT NULL)";
/// Hard logical-data bound, excluding SQLite pages, indexes and WAL overhead.
pub const MAX_STORE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Hard retained-capture count; no automatic eviction occurs.
pub const MAX_STORED_CAPTURES: u64 = 10_000;

/// Logical retention capacity; lower limits are useful for bounded host tasks.
#[derive(Debug, Clone, Copy)]
pub struct StoreLimits {
    /// Deduplicated object bytes plus serialized manifest bytes.
    pub max_bytes: u64,
    /// Maximum distinct capture manifests.
    pub max_captures: u64,
}

impl Default for StoreLimits {
    fn default() -> Self {
        Self {
            max_bytes: MAX_STORE_BYTES,
            max_captures: MAX_STORED_CAPTURES,
        }
    }
}

impl StoreLimits {
    fn validate(self) -> Result<()> {
        if self.max_bytes > MAX_STORE_BYTES || self.max_captures > MAX_STORED_CAPTURES {
            return Err(CaptureError::Invalid("store limits exceed hard maxima"));
        }
        Ok(())
    }
}

/// Metadata for one canonical capture, without paths, raw bytes or read policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CaptureInfo {
    /// Immutable content identity, including the registered source identity.
    pub capture_id: String,
    /// Number of selected file entries, including files that share an object.
    pub file_count: u64,
    /// Sum of selected file lengths; shared objects can be counted repeatedly.
    pub byte_len: u64,
}

/// Atomic local raw-source retention, separate from graphs and proposal stores.
///
/// The trusted host supplies a private application location and permissions.
/// There is no automatic eviction, source fallback or source export. Explicit
/// source deletion requires a matching inventory inside the write transaction.
pub struct CaptureStore {
    connection: Connection,
    limits: StoreLimits,
}

impl fmt::Debug for CaptureStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CaptureStore")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl CaptureStore {
    /// Open a host-owned SQLite/WAL path, creating a fresh version-two schema.
    /// Prototype version-one stores lack retained read policy and are rejected.
    /// The trusted parent is canonicalized (including platform temp aliases);
    /// SQLite itself refuses a symlink at the final database filename.
    pub fn open(path: &Path, limits: StoreLimits) -> Result<Self> {
        limits.validate()?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let name = path
            .file_name()
            .ok_or(CaptureError::Invalid("database filename required"))?;
        let path = std::fs::canonicalize(parent)?.join(name);
        Self::initialize(
            Connection::open_with_flags(
                &path,
                OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )?,
            limits,
        )
    }

    /// Create an ephemeral store with the same validation and transaction rules.
    pub fn in_memory(limits: StoreLimits) -> Result<Self> {
        limits.validate()?;
        Self::initialize(Connection::open_in_memory()?, limits)
    }

    fn initialize(mut connection: Connection, limits: StoreLimits) -> Result<Self> {
        connection.busy_timeout(std::time::Duration::from_secs(1))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: u32 =
            transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version == 0 {
            let tables: u64 = transaction.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT GLOB 'sqlite_*'", [], |row| nonnegative(row, 0)
            )?;
            if tables != 0 {
                return Err(CaptureError::Corrupt("unversioned nonempty store"));
            }
            transaction.execute_batch(CAPTURES_SCHEMA)?;
            transaction.execute_batch(OBJECTS_SCHEMA)?;
            transaction.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
        } else if version != STORE_SCHEMA_VERSION {
            return Err(CaptureError::Corrupt("unsupported store version"));
        }
        check_schema(&transaction)?;
        transaction.commit()?;
        Ok(Self { connection, limits })
    }

    /// Atomically retain the manifest and deduplicated objects. Conflicting
    /// existing rows fail; duplicate content retains the stricter span cap.
    pub fn persist(&mut self, capture: &Capture) -> Result<()> {
        validate_manifest(
            capture.manifest(),
            CaptureLimits {
                max_span_bytes: capture.max_span_bytes(),
                ..CaptureLimits::default()
            },
        )?;
        let manifest = canonical_manifest(capture.manifest(), MAX_MANIFEST_BYTES)?;
        if manifest_id(&manifest) != capture.id() {
            return Err(CaptureError::Corrupt("capture identity mismatch"));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_version(&transaction)?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM captures WHERE id=?1)",
            [capture.id()],
            |row| row.get(0),
        )?;
        if exists {
            let stored = load_capture(&transaction, capture.id())?;
            if stored.manifest() != capture.manifest()
                || capture.files.iter().any(|(path, file)| {
                    stored
                        .files
                        .get(path)
                        .is_none_or(|old| old.bytes() != file.bytes())
                })
            {
                return Err(CaptureError::Corrupt("conflicting existing capture"));
            }
            // The write transaction serializes concurrent replays. Content
            // identity stays immutable; retained policy can only tighten.
            let retained_cap = stored.max_span_bytes().min(capture.max_span_bytes());
            if retained_cap != stored.max_span_bytes() {
                let changed = transaction.execute(
                    "UPDATE captures SET max_span_bytes=?2 WHERE id=?1",
                    params![capture.id(), retained_cap as i64],
                )?;
                if changed != 1 {
                    return Err(CaptureError::Corrupt("capture policy update failed"));
                }
            }
            transaction.commit()?;
            return Ok(());
        }
        let invalid_storage: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM captures WHERE typeof(manifest) != 'blob' OR length(manifest) > ?1
                 OR NOT (CASE WHEN typeof(max_span_bytes) = 'integer' THEN max_span_bytes BETWEEN 0 AND ?3 ELSE 0 END)
                 UNION ALL SELECT 1 FROM objects WHERE typeof(bytes) != 'blob' OR length(bytes) > ?2)",
            params![MAX_MANIFEST_BYTES as i64, MAX_FILE_BYTES as i64, MAX_SPAN_BYTES as i64], |row| row.get(0)
        )?;
        if invalid_storage {
            return Err(CaptureError::Corrupt(
                "invalid retained payload type or length",
            ));
        }
        let mut missing: BTreeMap<&str, &[u8]> = BTreeMap::new();
        for file in capture.files.values() {
            let reference = file.reference();
            if let Some(stored) = read_object(&transaction, &reference.digest)? {
                if stored != file.bytes() {
                    return Err(CaptureError::Corrupt("conflicting existing object"));
                }
            } else if let Some(prior) = missing.insert(&reference.digest, file.bytes())
                && prior != file.bytes()
            {
                return Err(CaptureError::Corrupt("conflicting object identity"));
            }
        }
        let (count, manifest_bytes): (u64, u64) = transaction.query_row(
            "SELECT count(*), coalesce(sum(length(manifest)),0) FROM captures",
            [],
            |row| Ok((nonnegative(row, 0)?, nonnegative(row, 1)?)),
        )?;
        let object_bytes: u64 = transaction.query_row(
            "SELECT coalesce(sum(length(bytes)),0) FROM objects",
            [],
            |row| nonnegative(row, 0),
        )?;
        let new_bytes = missing
            .values()
            .map(|bytes| bytes.len() as u64)
            .sum::<u64>();
        let total = manifest_bytes
            .checked_add(object_bytes)
            .and_then(|value| value.checked_add(manifest.len() as u64))
            .and_then(|value| value.checked_add(new_bytes))
            .ok_or(CaptureError::Limit("stored logical bytes"))?;
        if count >= self.limits.max_captures || total > self.limits.max_bytes {
            return Err(CaptureError::Limit("store capacity"));
        }
        for (digest, bytes) in missing {
            require_inserted(transaction.execute(
                "INSERT INTO objects(digest,bytes) VALUES (?1,?2)",
                params![digest, bytes],
            )?)?;
        }
        require_inserted(transaction.execute(
            "INSERT INTO captures(id,manifest,max_span_bytes) VALUES (?1,?2,?3)",
            params![capture.id(), manifest, capture.max_span_bytes() as i64],
        )?)?;
        transaction.commit()?;
        Ok(())
    }

    /// Load and revalidate metadata, retained span policy and every raw object.
    pub fn load(&self, capture_id: &str) -> Result<Capture> {
        // A read transaction prevents another connection changing length/data
        // between the preallocation bound checks and the actual blob reads.
        let transaction = self.connection.unchecked_transaction()?;
        let capture = load_capture(&transaction, capture_id)?;
        transaction.commit()?;
        Ok(capture)
    }

    /// Validate the canonical manifest, retained policy and complete selected
    /// file/range binding, then copy the verified span bytes. Only the requested
    /// raw object is loaded and hashed; corruption in another object's bytes
    /// does not block this read. `load` still validates every captured object.
    pub fn read_span(&self, reference: &CaptureSpanRef) -> Result<Vec<u8>> {
        let transaction = self.connection.unchecked_transaction()?;
        check_version(&transaction)?;
        let (manifest, max_span_bytes) = load_manifest(&transaction, &reference.file.capture_id)?;
        let entry = manifest
            .files
            .iter()
            .find(|entry| entry.path == reference.file.path)
            .ok_or(CaptureError::Missing("path outside capture"))?;
        let expected = CaptureFileRef {
            source_id: manifest.source_id.clone(),
            capture_id: reference.file.capture_id.clone(),
            path: entry.path.clone(),
            digest: entry.digest.clone(),
            byte_len: entry.byte_len,
        };
        if reference.file != expected {
            return Err(CaptureError::Corrupt("file reference mismatch"));
        }
        validate_range(
            reference.byte_start,
            reference.byte_end,
            entry.byte_len,
            max_span_bytes,
        )?;
        let bytes = read_object(&transaction, &entry.digest)?
            .ok_or(CaptureError::Missing("captured object"))?;
        if bytes.len() as u64 != entry.byte_len {
            return Err(CaptureError::Corrupt("manifest and object length disagree"));
        }
        if let Some(git) = &entry.git
            && git2::Oid::hash_object(git2::ObjectType::Blob, &bytes)?.to_string() != git.oid
        {
            return Err(CaptureError::Corrupt("Git blob identity mismatch"));
        }
        let span = bytes[reference.byte_start as usize..reference.byte_end as usize].to_vec();
        transaction.commit()?;
        Ok(span)
    }

    /// Strict UTF-8 span read; no normalization or lossy decoding is performed.
    pub fn read_text_span(&self, reference: &CaptureSpanRef) -> Result<String> {
        let bytes = self.read_span(reference)?;
        String::from_utf8(bytes).map_err(|_| CaptureError::Encoding)
    }

    /// Return this source's capture metadata sorted by capture identity. Every
    /// bounded manifest is decoded and validated before trusting ownership;
    /// this is not a raw-object integrity audit and loads no source bytes.
    pub fn source_inventory(&self, source: &SourceId) -> Result<Vec<CaptureInfo>> {
        let transaction = self.connection.unchecked_transaction()?;
        let mut inventory = Vec::new();
        visit_manifests(&transaction, |id, manifest| {
            if &manifest.source_id == source {
                inventory.push(capture_info(id, manifest));
            }
            Ok(())
        })?;
        transaction.commit()?;
        Ok(inventory)
    }

    /// Atomically forget exactly this source's current capture set. The caller
    /// supplies the sorted, unique IDs from its preview; a changed inventory
    /// rejects the whole operation. Only objects referenced by deleted captures
    /// and by no remaining capture are removed. Missing/corrupt raw objects do
    /// not establish ownership and are not read; corrupt manifests fail closed.
    /// The host separately guards receipt publication and retains its metadata.
    /// Deletion ends the retained span policy's lifetime; explicit identical
    /// recapture uses its newly supplied cap, without changing historical refs.
    pub fn forget_source(
        &mut self,
        source: &SourceId,
        expected_capture_ids: &[String],
    ) -> Result<u64> {
        if expected_capture_ids.len() as u64 > MAX_STORED_CAPTURES {
            return Err(CaptureError::Limit("capture inventory count"));
        }
        for id in expected_capture_ids {
            validate_capture_id(id)?;
        }
        if expected_capture_ids
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        {
            return Err(CaptureError::Invalid(
                "sorted unique capture inventory required",
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut actual = Vec::new();
        let mut removable = BTreeSet::new();
        visit_manifests(&transaction, |id, manifest| {
            if &manifest.source_id == source {
                actual.push(id.to_owned());
                removable.extend(manifest.files.iter().map(|entry| entry.digest.clone()));
            }
            Ok(())
        })?;
        if actual.as_slice() != expected_capture_ids {
            return Err(CaptureError::Invalid(
                "source inventory changed; refresh required",
            ));
        }
        // Stream a second metadata pass instead of retaining all manifests or
        // all other sources' object references in memory. Nothing is deleted
        // until every manifest has passed validation in this write snapshot.
        visit_manifests(&transaction, |_, manifest| {
            if &manifest.source_id != source {
                for entry in &manifest.files {
                    removable.remove(&entry.digest);
                }
            }
            Ok(())
        })?;
        for id in &actual {
            let affected = transaction.execute("DELETE FROM captures WHERE id=?1", [id])?;
            if affected != 1 {
                return Err(CaptureError::Corrupt(
                    "capture deletion did not remove one row",
                ));
            }
        }
        for digest in removable {
            // An already missing object needs no deletion. The unique digest
            // key and exact schema prevent deleting a different object here.
            transaction.execute("DELETE FROM objects WHERE digest=?1", [digest])?;
        }
        transaction.commit()?;
        Ok(actual.len() as u64)
    }
}

fn capture_info(id: &str, manifest: &CaptureManifest) -> CaptureInfo {
    CaptureInfo {
        capture_id: id.to_owned(),
        file_count: manifest.files.len() as u64,
        // validate_manifest has already checked this sum against its hard cap.
        byte_len: manifest.files.iter().map(|entry| entry.byte_len).sum(),
    }
}

/// Check inventory bounds using metadata before fetching any manifest or key,
/// then decode one bounded manifest at a time in the caller's transaction.
fn visit_manifests(
    connection: &Connection,
    mut visit: impl FnMut(&str, &CaptureManifest) -> Result<()>,
) -> Result<()> {
    check_version(connection)?;
    let count: u64 = connection.query_row("SELECT count(*) FROM captures", [], |row| {
        nonnegative(row, 0)
    })?;
    if count > MAX_STORED_CAPTURES {
        return Err(CaptureError::Limit("capture inventory count"));
    }
    let invalid: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM captures WHERE typeof(id) != 'text'
         OR length(CAST(id AS BLOB)) != 75 OR typeof(manifest) != 'blob'
         OR length(manifest) > ?1
         OR NOT (CASE WHEN typeof(max_span_bytes) = 'integer'
                 THEN max_span_bytes BETWEEN 0 AND ?2 ELSE 0 END))",
        params![MAX_MANIFEST_BYTES as i64, MAX_SPAN_BYTES as i64],
        |row| row.get(0),
    )?;
    if invalid {
        return Err(CaptureError::Corrupt("invalid capture inventory metadata"));
    }
    let manifest_bytes: u64 = connection.query_row(
        "SELECT coalesce(sum(length(manifest)),0) FROM captures",
        [],
        |row| nonnegative(row, 0),
    )?;
    if manifest_bytes > MAX_STORE_BYTES {
        return Err(CaptureError::Limit("capture inventory bytes"));
    }
    let mut statement = connection.prepare("SELECT id FROM captures ORDER BY id")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let (manifest, _) = load_manifest(connection, &id)?;
        visit(&id, &manifest)?;
    }
    Ok(())
}

fn nonnegative(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}

fn require_inserted(affected: usize) -> Result<()> {
    if affected != 1 {
        return Err(CaptureError::Corrupt(
            "capture publication did not insert exactly one row",
        ));
    }
    Ok(())
}

fn check_version(connection: &Connection) -> Result<()> {
    let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != STORE_SCHEMA_VERSION {
        return Err(CaptureError::Corrupt("unsupported store version"));
    }
    check_schema(connection)?;
    Ok(())
}

fn check_schema(connection: &Connection) -> Result<()> {
    // Only our two ordinary tables are admitted. A view can reevaluate its
    // payload between the length and body queries even within one transaction;
    // triggers could silently alter a publication. Neither belongs here.
    let count: u64 = connection.query_row(
        "SELECT count(*) FROM sqlite_master WHERE name NOT GLOB 'sqlite_*'",
        [],
        |row| nonnegative(row, 0),
    )?;
    if count != 2 {
        return Err(CaptureError::Corrupt("unexpected store schema objects"));
    }
    for (table, key, value, schema, pragma) in [
        (
            "captures",
            "id",
            "manifest",
            CAPTURES_SCHEMA,
            "PRAGMA table_xinfo(captures)",
        ),
        (
            "objects",
            "digest",
            "bytes",
            OBJECTS_SCHEMA,
            "PRAGMA table_xinfo(objects)",
        ),
    ] {
        // This is a private versioned schema: accept exactly the DDL we create,
        // including conflict policies, rather than guessing compatibility from
        // visible columns. table_info alone hides generated columns.
        let exact: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1 AND type='table' AND sql=?2)",
            params![table, schema],
            |row| row.get(0),
        )?;
        if !exact {
            return Err(CaptureError::Corrupt("incompatible ordinary store table"));
        }
        let mut statement = connection.prepare(pragma)?;
        let columns = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut expected = vec![
            (key.to_string(), "TEXT".to_string(), 1, None, 1, 0),
            (value.to_string(), "BLOB".to_string(), 1, None, 0, 0),
        ];
        if table == "captures" {
            expected.push(("max_span_bytes".into(), "INTEGER".into(), 1, None, 0, 0));
        }
        if columns != expected {
            return Err(CaptureError::Corrupt("incompatible store columns or keys"));
        }
    }
    Ok(())
}

fn read_object(connection: &Connection, digest: &str) -> Result<Option<Vec<u8>>> {
    let metadata: Option<(String, u64)> = connection
        .query_row(
            "SELECT typeof(bytes),length(bytes) FROM objects WHERE digest=?1",
            [digest],
            |row| Ok((row.get(0)?, nonnegative(row, 1)?)),
        )
        .optional()?;
    let Some((kind, len)) = metadata else {
        return Ok(None);
    };
    if kind != "blob" || len > MAX_FILE_BYTES {
        return Err(CaptureError::Corrupt(
            "invalid stored object type or length",
        ));
    }
    let bytes: Vec<u8> = connection.query_row(
        "SELECT bytes FROM objects WHERE digest=?1",
        [digest],
        |row| row.get(0),
    )?;
    if bytes.len() as u64 != len || blake3::hash(&bytes).to_hex().as_str() != digest {
        return Err(CaptureError::Corrupt(
            "stored object digest or length mismatch",
        ));
    }
    Ok(Some(bytes))
}

// Callers validate the store schema/version and keep one transaction alive
// across metadata checks, manifest decoding and any selected object reads.
fn load_manifest(connection: &Connection, id: &str) -> Result<(CaptureManifest, u64)> {
    validate_capture_id(id)?;
    let metadata: Option<(String, u64, Option<i64>)> = connection
        .query_row(
            // CASE never returns a malformed policy payload (for example an
            // oversized BLOB). Check its type/range before fetching raw data.
            "SELECT typeof(manifest),length(manifest),
             CASE WHEN typeof(max_span_bytes) = 'integer' THEN max_span_bytes END
             FROM captures WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, nonnegative(row, 1)?, row.get(2)?)),
        )
        .optional()?;
    let (kind, len, cap) = metadata.ok_or(CaptureError::Missing("capture manifest"))?;
    let max_span_bytes = cap
        .and_then(|cap| u64::try_from(cap).ok())
        .filter(|cap| *cap <= MAX_SPAN_BYTES)
        .ok_or(CaptureError::Corrupt("invalid retained span policy"))?;
    if kind != "blob" || len > MAX_MANIFEST_BYTES {
        return Err(CaptureError::Corrupt(
            "invalid stored manifest type or length",
        ));
    }
    let raw: Vec<u8> =
        connection.query_row("SELECT manifest FROM captures WHERE id=?1", [id], |row| {
            row.get(0)
        })?;
    if raw.len() as u64 != len || manifest_id(&raw) != id {
        return Err(CaptureError::Corrupt("stored manifest identity mismatch"));
    }
    let manifest: CaptureManifest = serde_json::from_slice(&raw)
        .map_err(|_| CaptureError::Corrupt("invalid stored manifest encoding"))?;
    validate_manifest(&manifest, CaptureLimits::default())?;
    if canonical_manifest(&manifest, MAX_MANIFEST_BYTES)? != raw {
        return Err(CaptureError::Corrupt("noncanonical stored manifest"));
    }
    Ok((manifest, max_span_bytes))
}

fn load_capture(connection: &Connection, id: &str) -> Result<Capture> {
    check_version(connection)?;
    let (manifest, max_span_bytes) = load_manifest(connection, id)?;
    let mut buffers = BTreeMap::new();
    for entry in &manifest.files {
        let bytes = read_object(connection, &entry.digest)?
            .ok_or(CaptureError::Missing("captured object"))?;
        if bytes.len() as u64 != entry.byte_len {
            return Err(CaptureError::Corrupt("manifest and object length disagree"));
        }
        buffers.insert(entry.path.clone(), bytes);
    }
    let capture = assemble_capture(
        manifest,
        buffers,
        CaptureLimits {
            max_span_bytes,
            ..CaptureLimits::default()
        },
    )?;
    if capture.id() != id {
        return Err(CaptureError::Corrupt("loaded capture identity mismatch"));
    }
    Ok(capture)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SourceId, capture_working_tree};

    fn captured(name: &str, bytes: &[u8]) -> Capture {
        captured_with_cap(name, bytes, MAX_SPAN_BYTES)
    }

    fn captured_with_cap(name: &str, bytes: &[u8], max_span_bytes: u64) -> Capture {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(name), bytes).unwrap();
        capture_working_tree(
            dir.path(),
            &SourceId::new("host-store-fixture").unwrap(),
            &[name.into()],
            CaptureLimits {
                max_span_bytes,
                ..CaptureLimits::default()
            },
        )
        .unwrap()
    }

    fn captured_files(source: &str, files: &[(&str, &[u8])]) -> Capture {
        let dir = tempfile::tempdir().unwrap();
        for (path, bytes) in files {
            std::fs::write(dir.path().join(path), bytes).unwrap();
        }
        capture_working_tree(
            dir.path(),
            &SourceId::new(source).unwrap(),
            &files
                .iter()
                .map(|(path, _)| (*path).into())
                .collect::<Vec<_>>(),
            CaptureLimits::default(),
        )
        .unwrap()
    }

    fn inventory_ids(store: &CaptureStore, source: &SourceId) -> Vec<String> {
        store
            .source_inventory(source)
            .unwrap()
            .into_iter()
            .map(|info| info.capture_id)
            .collect()
    }

    fn counts(store: &CaptureStore) -> (u64, u64) {
        store
            .connection
            .query_row(
                "SELECT (SELECT count(*) FROM captures),(SELECT count(*) FROM objects)",
                [],
                |row| Ok((nonnegative(row, 0)?, nonnegative(row, 1)?)),
            )
            .unwrap()
    }

    // AC-0135: restart preserves exact raw objects; identical writes are idempotent.
    #[test]
    fn durable_store_reopens_exact_bytes_and_deduplicates_objects() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("captures.sqlite");
        let first = captured("first.ts", b"private source\r\n");
        let second = captured("second.ts", b"private source\r\n");
        let span = first.file("first.ts").unwrap().span(0, 14).unwrap();
        {
            let mut store = CaptureStore::open(&database, StoreLimits::default()).unwrap();
            store.persist(&first).unwrap();
            store.persist(&first).unwrap();
            store.persist(&second).unwrap();
            assert_eq!(counts(&store), (2, 1));
            assert!(!format!("{store:?}").contains("private source"));
        }
        let store = CaptureStore::open(&database, StoreLimits::default()).unwrap();
        let loaded = store.load(first.id()).unwrap();
        assert_eq!(loaded.id(), first.id());
        assert_eq!(
            loaded.file("first.ts").unwrap().bytes(),
            b"private source\r\n"
        );
        assert_eq!(store.read_text_span(&span).unwrap(), "private source");
        let mut wrong = span.clone();
        wrong.file.source_id = SourceId::new("other").unwrap();
        assert!(store.read_span(&wrong).is_err());
    }

    // AC-0153: the SQLite connection itself refuses a symlink database path;
    // checking an earlier filesystem handle cannot authorize a later target.
    #[cfg(unix)]
    #[test]
    fn capture_store_open_refuses_symlink_database_paths() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("captures.sqlite");
        let alias = directory.path().join("alias.sqlite");
        let capture = captured("source", b"untouched");
        let mut store = CaptureStore::open(&database, StoreLimits::default()).unwrap();
        store.persist(&capture).unwrap();
        drop(store);
        symlink(&database, &alias).unwrap();
        assert!(CaptureStore::open(&alias, StoreLimits::default()).is_err());
        let store = CaptureStore::open(&database, StoreLimits::default()).unwrap();
        assert_eq!(
            store.load(capture.id()).unwrap().manifest(),
            capture.manifest()
        );
    }

    // AC-0153: a source preview contains only sorted metadata; explicit forgetting
    // removes its own manifests and unshared objects, preserving other sources.
    #[test]
    fn source_inventory_and_forgetting_preserve_shared_objects() {
        let first = captured_files(
            "first-source",
            &[("shared.ts", b"shared"), ("private.ts", b"first-only")],
        );
        let historical = captured_files("first-source", &[("previous.ts", b"previous")]);
        let second = captured_files(
            "second-source",
            &[("shared.ts", b"shared"), ("private.ts", b"second-only")],
        );
        let mut store = CaptureStore::in_memory(StoreLimits::default()).unwrap();
        for capture in [&second, &historical, &first] {
            store.persist(capture).unwrap();
        }
        // A pre-existing orphan is not attributed to the source being forgotten.
        let orphan_digest = blake3::hash(b"orphan").to_hex().to_string();
        store
            .connection
            .execute(
                "INSERT INTO objects(digest,bytes) VALUES (?1,?2)",
                params![orphan_digest, b"orphan".as_slice()],
            )
            .unwrap();
        assert_eq!(counts(&store), (3, 5));
        let source = &first.manifest().source_id;
        let inventory = store.source_inventory(source).unwrap();
        let mut expected = vec![
            CaptureInfo {
                capture_id: first.id().into(),
                file_count: 2,
                byte_len: 16,
            },
            CaptureInfo {
                capture_id: historical.id().into(),
                file_count: 1,
                byte_len: 8,
            },
        ];
        expected.sort_by(|a, b| a.capture_id.cmp(&b.capture_id));
        assert_eq!(inventory, expected);
        let wire = serde_json::to_string(&inventory).unwrap();
        for omitted in ["first-only", "shared.ts", "first-source", "max_span_bytes"] {
            assert!(!wire.contains(omitted));
        }
        let ids = inventory_ids(&store, source);
        assert_eq!(store.forget_source(source, &ids).unwrap(), 2);
        assert_eq!(counts(&store), (1, 3));
        assert!(store.source_inventory(source).unwrap().is_empty());
        assert!(store.load(first.id()).is_err());
        assert!(store.load(historical.id()).is_err());
        let shared_span = second.file("shared.ts").unwrap().span(0, 6).unwrap();
        assert_eq!(store.read_span(&shared_span).unwrap(), b"shared");
        assert_eq!(
            store.load(second.id()).unwrap().manifest(),
            second.manifest()
        );
        assert_eq!(
            read_object(&store.connection, &orphan_digest)
                .unwrap()
                .unwrap(),
            b"orphan"
        );
        assert_eq!(store.forget_source(source, &[]).unwrap(), 0);
        let second_source = &second.manifest().source_id;
        let second_ids = inventory_ids(&store, second_source);
        assert_eq!(store.forget_source(second_source, &second_ids).unwrap(), 1);
        assert_eq!(counts(&store), (0, 1));
    }

    // AC-0153: stale previews, invalid ordering and corrupt unrelated ownership
    // all fail before publication, leaving every manifest and object in place.
    #[test]
    fn source_forgetting_rejects_stale_or_corrupt_inventory_atomically() {
        let first = captured("one", b"one");
        let later = captured("two", b"two");
        let source = &first.manifest().source_id;
        let mut store = CaptureStore::in_memory(StoreLimits::default()).unwrap();
        store.persist(&first).unwrap();
        let stale = inventory_ids(&store, source);
        store.persist(&later).unwrap();
        assert!(store.forget_source(source, &stale).is_err());
        let current = inventory_ids(&store, source);
        let mut reversed = current.clone();
        reversed.reverse();
        for wrong in [vec![], reversed, vec![current[0].clone(); 2]] {
            assert!(store.forget_source(source, &wrong).is_err());
            assert_eq!(inventory_ids(&store, source), current);
            assert_eq!(counts(&store), (2, 2));
        }

        let other = captured_files("other-source", &[("other", b"other")]);
        for corruption in [
            "UPDATE captures SET manifest=X'7B7D' WHERE id=?1",
            "UPDATE captures SET manifest='éé' WHERE id=?1",
            "UPDATE captures SET manifest=zeroblob(8388609) WHERE id=?1",
            "UPDATE captures SET id=zeroblob(1000) WHERE id=?1",
            "UPDATE captures SET max_span_bytes=-1 WHERE id=?1",
        ] {
            let mut store = CaptureStore::in_memory(StoreLimits::default()).unwrap();
            store.persist(&first).unwrap();
            store.persist(&other).unwrap();
            let expected = inventory_ids(&store, source);
            store
                .connection
                .pragma_update(None, "ignore_check_constraints", true)
                .unwrap();
            store.connection.execute(corruption, [other.id()]).unwrap();
            assert!(store.source_inventory(source).is_err());
            assert!(store.forget_source(source, &expected).is_err());
            assert_eq!(counts(&store), (2, 2));
            assert_eq!(store.load(first.id()).unwrap().manifest(), first.manifest());
            assert_eq!(
                read_object(&store.connection, &other.manifest().files[0].digest)
                    .unwrap()
                    .unwrap(),
                b"other"
            );
        }

        // Hash-consistent payloads still need schema, source and canonical-order
        // validation. Decoder errors must not echo an untrusted enum value.
        let mut wrong_version = other.manifest().clone();
        wrong_version.schema_version += 1;
        let mut duplicate_path = other.manifest().clone();
        duplicate_path.files.push(duplicate_path.files[0].clone());
        let mut noncanonical = canonical_manifest(other.manifest(), MAX_MANIFEST_BYTES).unwrap();
        noncanonical.push(b' ');
        for raw in [
            serde_json::to_vec(&wrong_version).unwrap(),
            serde_json::to_vec(&duplicate_path).unwrap(),
            noncanonical,
            br#"{"schema_version":1,"source_id":"","kind":{"kind":"working_tree"},"files":[]}"#.to_vec(),
            br#"{"schema_version":1,"source_id":"other-source","kind":{"kind":"do-not-echo-private-input"},"files":[]}"#.to_vec(),
        ] {
            let mut store = CaptureStore::in_memory(StoreLimits::default()).unwrap();
            store.persist(&first).unwrap();
            store.persist(&other).unwrap();
            let expected = inventory_ids(&store, source);
            store.connection.execute(
                "UPDATE captures SET id=?1,manifest=?2 WHERE id=?3",
                params![manifest_id(&raw), raw, other.id()],
            ).unwrap();
            let error = store.source_inventory(source).unwrap_err();
            assert!(!error.to_string().contains("do-not-echo-private-input"));
            assert!(store.forget_source(source, &expected).is_err());
            assert_eq!(counts(&store), (2, 2));
            assert!(store.load(first.id()).is_ok());
        }
    }

    // AC-0153: metadata bounds and the private schema are checked before any
    // manifest body is trusted, including corruption introduced after open.
    #[test]
    fn source_inventory_rejects_schema_and_preallocation_bounds() {
        let capture = captured("source", b"abcd");
        let source = &capture.manifest().source_id;
        let ids = vec![capture.id().to_string()];
        for corruption in [
            "PRAGMA user_version=1",
            "CREATE TRIGGER sqliteXdrop BEFORE DELETE ON captures BEGIN SELECT RAISE(IGNORE); END",
        ] {
            let mut store = CaptureStore::in_memory(StoreLimits::default()).unwrap();
            store.persist(&capture).unwrap();
            store.connection.execute_batch(corruption).unwrap();
            assert!(store.source_inventory(source).is_err());
            assert!(store.forget_source(source, &ids).is_err());
            assert!(
                store
                    .read_span(&capture.file("source").unwrap().span(0, 1).unwrap())
                    .is_err()
            );
            assert_eq!(counts(&store), (1, 1));
        }

        let mut store = CaptureStore::in_memory(StoreLimits::default()).unwrap();
        store.persist(&capture).unwrap();
        store
            .connection
            .execute(
                "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x < ?1)
             INSERT INTO captures(id,manifest,max_span_bytes)
             SELECT printf('capture-v1:%064x',x), X'7B7D', 0 FROM n",
                [MAX_STORED_CAPTURES as i64],
            )
            .unwrap();
        // Count rejection precedes even the deliberately bad manifest bodies.
        assert!(matches!(
            store.source_inventory(source),
            Err(CaptureError::Limit("capture inventory count"))
        ));
        assert!(matches!(
            store.forget_source(source, &ids),
            Err(CaptureError::Limit("capture inventory count"))
        ));
        assert_eq!(counts(&store), (MAX_STORED_CAPTURES + 1, 1));
    }

    // AC-0152, AC-0153: restart and changed working bytes cannot satisfy a
    // forgotten reference; explicit identical recapture can restore its ID.
    #[test]
    fn forgotten_captures_stay_unavailable_until_explicit_identical_recapture() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("captures.sqlite");
        let original = captured_with_cap("source", b"abcd", 2);
        let changed = captured_with_cap("source", b"wxyz", 2);
        assert_ne!(original.id(), changed.id());
        let reference = original.file("source").unwrap().span(0, 2).unwrap();
        let source = &original.manifest().source_id;
        {
            let mut store = CaptureStore::open(&database, StoreLimits::default()).unwrap();
            store.persist(&original).unwrap();
            let ids = inventory_ids(&store, source);
            assert_eq!(store.forget_source(source, &ids).unwrap(), 1);
            assert_eq!(counts(&store), (0, 0));
        }
        {
            let mut store = CaptureStore::open(&database, StoreLimits::default()).unwrap();
            assert!(store.read_span(&reference).is_err());
            store.persist(&changed).unwrap();
            assert!(store.read_span(&reference).is_err());
            let ids = inventory_ids(&store, source);
            store.forget_source(source, &ids).unwrap();
            // Acquisition is explicit; no path or working-tree read occurs in
            // the store. This independent capture supplies identical bytes.
            let recaptured = captured_with_cap("source", b"abcd", 2);
            assert_eq!(recaptured.id(), original.id());
            store.persist(&recaptured).unwrap();
        }
        let store = CaptureStore::open(&database, StoreLimits::default()).unwrap();
        assert_eq!(store.read_text_span(&reference).unwrap(), "ab");
        let mut over_cap = reference.clone();
        over_cap.byte_end = 3;
        assert!(store.read_span(&over_cap).is_err());
        assert_eq!(counts(&store), (1, 1));
    }

    // AC-0152: inspection hashes the selected complete object, but never loads
    // unrelated objects from its manifest. Full-capture load stays stricter.
    #[test]
    fn selected_span_reads_validate_only_the_requested_object() {
        let capture = captured_files(
            "selected-source",
            &[("good.ts", "aéz".as_bytes()), ("other.ts", b"other")],
        );
        let reference = capture.file("good.ts").unwrap().span(0, 3).unwrap();
        let other_digest = &capture.file("other.ts").unwrap().reference().digest;
        for corruption in [
            "UPDATE objects SET bytes=X'00' WHERE digest=?1",
            "UPDATE objects SET bytes=zeroblob(16777217) WHERE digest=?1",
            "UPDATE objects SET bytes='éé' WHERE digest=?1",
            "DELETE FROM objects WHERE digest=?1",
        ] {
            let mut store = CaptureStore::in_memory(StoreLimits::default()).unwrap();
            store.persist(&capture).unwrap();
            store
                .connection
                .execute(corruption, [other_digest])
                .unwrap();
            assert!(store.load(capture.id()).is_err());
            assert_eq!(store.read_text_span(&reference).unwrap(), "aé");
            let mut split_utf8 = reference.clone();
            split_utf8.byte_end = 2;
            assert_eq!(store.read_span(&split_utf8).unwrap(), &[b'a', 0xc3]);
            assert!(matches!(
                store.read_text_span(&split_utf8),
                Err(CaptureError::Encoding)
            ));
            // Metadata inventory is not an attestation that all objects exist.
            let source = &capture.manifest().source_id;
            let ids = inventory_ids(&store, source);
            assert_eq!(ids, vec![capture.id().to_string()]);
            assert_eq!(store.forget_source(source, &ids).unwrap(), 1);
            assert_eq!(counts(&store), (0, 0));
        }
        let mut store = CaptureStore::in_memory(StoreLimits::default()).unwrap();
        store.persist(&capture).unwrap();
        store
            .connection
            .execute(
                "UPDATE objects SET bytes=?1 WHERE digest=?2",
                params!["aéx".as_bytes(), reference.file.digest],
            )
            .unwrap();
        // Even a corruption outside the requested three bytes invalidates the
        // selected object's digest; slicing before verification is forbidden.
        assert!(store.read_span(&reference).is_err());
    }

    // AC-0152: selected reads retain complete membership, range, policy, manifest
    // and Git-object validation; optimizing reads never weakens those bindings.
    #[test]
    fn selected_span_reads_reject_manifest_and_reference_corruption() {
        let capture = captured_with_cap("source", b"abcd", 2);
        let reference = capture.file("source").unwrap().span(0, 2).unwrap();
        let mut store = CaptureStore::in_memory(StoreLimits::default()).unwrap();
        store.persist(&capture).unwrap();
        for mutation in 0..8 {
            let mut wrong = reference.clone();
            match mutation {
                0 => wrong.file.source_id = SourceId::new("another-source").unwrap(),
                1 => wrong.file.capture_id = format!("capture-v1:{}", "0".repeat(64)),
                2 => wrong.file.path = "unselected".into(),
                3 => wrong.file.digest = "0".repeat(64),
                4 => wrong.file.byte_len += 1,
                5 => wrong.byte_end = wrong.byte_start,
                6 => wrong.byte_end = 3,
                7 => wrong.byte_start = u64::MAX,
                _ => unreachable!(),
            }
            assert!(store.read_span(&wrong).is_err());
        }
        let mut version = capture.manifest().clone();
        version.schema_version += 1;
        let mut duplicate = capture.manifest().clone();
        duplicate.files.push(duplicate.files[0].clone());
        let mut wrong_length = capture.manifest().clone();
        wrong_length.files[0].byte_len += 1;
        let mut wrong_git = capture.manifest().clone();
        wrong_git.kind = crate::CaptureKind::Git {
            commit: "1".repeat(40),
            tree: "2".repeat(40),
        };
        wrong_git.files[0].git = Some(crate::GitFile {
            oid: "3".repeat(40),
            mode: 0o100644,
        });
        for manifest in [version, duplicate, wrong_length, wrong_git] {
            let raw = canonical_manifest(&manifest, MAX_MANIFEST_BYTES).unwrap();
            let id = manifest_id(&raw);
            store
                .connection
                .execute(
                    "INSERT INTO captures(id,manifest,max_span_bytes) VALUES (?1,?2,2)",
                    params![id, raw],
                )
                .unwrap();
            let mut wrong = reference.clone();
            wrong.file.capture_id = id;
            wrong.file.byte_len = manifest.files[0].byte_len;
            assert!(store.read_span(&wrong).is_err());
            assert_eq!(store.read_text_span(&reference).unwrap(), "ab");
        }
        for corrupt in [
            "UPDATE captures SET manifest=X'7B7D' WHERE id=?1",
            "UPDATE captures SET manifest=zeroblob(8388609) WHERE id=?1",
            "UPDATE captures SET max_span_bytes=-1 WHERE id=?1",
        ] {
            let mut store = CaptureStore::in_memory(StoreLimits::default()).unwrap();
            store.persist(&capture).unwrap();
            store
                .connection
                .pragma_update(None, "ignore_check_constraints", true)
                .unwrap();
            store.connection.execute(corrupt, [capture.id()]).unwrap();
            assert!(store.read_span(&reference).is_err());
        }
    }

    // AC-0135: read policy survives a real restart independently of canonical
    // content identity, and either duplicate-persistence order retains the min.
    #[test]
    fn retained_span_policy_survives_restart_and_never_widens() {
        let narrow = captured_with_cap("source", b"abcd", 2);
        let broad = captured("source", b"abcd");
        assert_eq!(narrow.id(), broad.id());
        assert_eq!(narrow.manifest(), broad.manifest());
        let allowed = narrow.file("source").unwrap().span(0, 2).unwrap();
        let denied = broad.file("source").unwrap().span(0, 3).unwrap();
        assert_eq!(
            serde_json::to_value(narrow.file("source").unwrap().reference()).unwrap(),
            serde_json::to_value(broad.file("source").unwrap().reference()).unwrap()
        );
        assert!(narrow.read_span(&denied).is_err());
        for narrow_first in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("captures.sqlite");
            let (first, second) = if narrow_first {
                (&narrow, &broad)
            } else {
                (&broad, &narrow)
            };
            {
                let mut store = CaptureStore::open(&path, StoreLimits::default()).unwrap();
                store.persist(first).unwrap();
                assert_eq!(store.read_span(&denied).is_err(), narrow_first);
            }
            {
                let mut store = CaptureStore::open(&path, StoreLimits::default()).unwrap();
                store.persist(second).unwrap();
                assert_eq!(counts(&store), (1, 1));
                assert_eq!(store.load(narrow.id()).unwrap().max_span_bytes(), 2);
            }
            let mut reopened = CaptureStore::open(&path, StoreLimits::default()).unwrap();
            // Replaying either input after another restart never loosens policy.
            reopened.persist(&broad).unwrap();
            reopened.persist(&narrow).unwrap();
            let loaded = reopened.load(narrow.id()).unwrap();
            assert_eq!(loaded.id(), narrow.id());
            assert_eq!(loaded.manifest(), narrow.manifest());
            assert_eq!(loaded.max_span_bytes(), 2);
            assert!(loaded.file("source").unwrap().span(0, 3).is_err());
            assert!(loaded.read_span(&denied).is_err());
            assert!(reopened.read_span(&denied).is_err());
            assert!(reopened.read_text_span(&denied).is_err());
            assert_eq!(reopened.read_span(&allowed).unwrap(), b"ab");
            assert_eq!(reopened.read_text_span(&allowed).unwrap(), "ab");
            assert_eq!(counts(&reopened), (1, 1));
        }
        // Tightening future store reads does not revoke previously returned
        // immutable buffers or change what their content references identify.
        assert_eq!(broad.read_span(&denied).unwrap(), b"abc");

        let git_dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(git_dir.path()).unwrap();
        let blob = repo.blob(b"abcd").unwrap();
        let tree = {
            let mut builder = repo.treebuilder(None).unwrap();
            builder.insert("source", blob, 0o100644).unwrap();
            repo.find_tree(builder.write().unwrap()).unwrap()
        };
        let signature =
            git2::Signature::new("fixture", "fixture@example.invalid", &git2::Time::new(1, 0))
                .unwrap();
        let commit = repo
            .commit(None, &signature, &signature, "fixture", &tree, &[])
            .unwrap();
        let git_capture = crate::capture_git(
            git_dir.path(),
            &SourceId::new("host-git-policy-fixture").unwrap(),
            &commit.to_string(),
            &["source".into()],
            CaptureLimits {
                max_span_bytes: 2,
                ..CaptureLimits::default()
            },
        )
        .unwrap();
        let allowed = git_capture.file("source").unwrap().span(0, 2).unwrap();
        let mut denied = allowed.clone();
        denied.byte_end = 3;
        let store_dir = tempfile::tempdir().unwrap();
        let path = store_dir.path().join("captures.sqlite");
        {
            let mut store = CaptureStore::open(&path, StoreLimits::default()).unwrap();
            store.persist(&git_capture).unwrap();
        }
        let reopened = CaptureStore::open(&path, StoreLimits::default()).unwrap();
        assert_eq!(reopened.load(git_capture.id()).unwrap().max_span_bytes(), 2);
        assert_eq!(reopened.read_span(&allowed).unwrap(), b"ab");
        assert!(reopened.read_span(&denied).is_err());
    }

    // AC-0135: even an empty capture retains its policy; an absent, mistyped or
    // invalid cap cannot be replaced by a default or repaired from a replay.
    #[test]
    fn empty_capture_policy_and_invalid_stored_caps_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let source = SourceId::new("host-empty-policy-fixture").unwrap();
        let empty = |max_span_bytes| {
            capture_working_tree(
                dir.path(),
                &source,
                &[],
                CaptureLimits {
                    max_span_bytes,
                    ..CaptureLimits::default()
                },
            )
            .unwrap()
        };
        let no_spans = empty(0);
        let broad = empty(MAX_SPAN_BYTES);
        assert_eq!(no_spans.id(), broad.id());
        let path = dir.path().join("captures.sqlite");
        {
            let mut store = CaptureStore::open(&path, StoreLimits::default()).unwrap();
            store.persist(&broad).unwrap();
            store.persist(&no_spans).unwrap();
        }
        let mut reopened = CaptureStore::open(&path, StoreLimits::default()).unwrap();
        reopened.persist(&broad).unwrap();
        assert_eq!(reopened.load(no_spans.id()).unwrap().max_span_bytes(), 0);
        assert_eq!(counts(&reopened), (1, 0));
        let zero_file = captured_with_cap("source", b"abcd", 0);
        let reference = captured("source", b"abcd")
            .file("source")
            .unwrap()
            .span(0, 1)
            .unwrap();
        reopened.persist(&zero_file).unwrap();
        assert!(reopened.read_span(&reference).is_err());

        let good = captured_with_cap("source", b"abcd", 2);
        for invalid in [
            "-1",
            "262145",
            "2.5",
            "'not-a-policy'",
            "zeroblob(16777217)",
        ] {
            let mut store = CaptureStore::in_memory(StoreLimits::default()).unwrap();
            store.persist(&good).unwrap();
            // Model external corruption without weakening the required DDL.
            store
                .connection
                .pragma_update(None, "ignore_check_constraints", true)
                .unwrap();
            store
                .connection
                .execute(
                    &format!("UPDATE captures SET max_span_bytes={invalid}, manifest=X'7B7D'"),
                    [],
                )
                .unwrap();
            // The policy error must precede even manifest identity validation,
            // and therefore all raw source object reads.
            assert!(matches!(
                store.load(good.id()),
                Err(CaptureError::Corrupt("invalid retained span policy"))
            ));
            assert!(matches!(
                store.persist(&good),
                Err(CaptureError::Corrupt("invalid retained span policy"))
            ));
            assert!(store.persist(&captured("other", b"other")).is_err());
            assert_eq!(counts(&store), (1, 1));
        }

        for version in [1, STORE_SCHEMA_VERSION] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("prototype.sqlite");
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch("CREATE TABLE captures (id TEXT PRIMARY KEY NOT NULL, manifest BLOB NOT NULL); CREATE TABLE objects (digest TEXT PRIMARY KEY NOT NULL, bytes BLOB NOT NULL);").unwrap();
            connection
                .pragma_update(None, "user_version", version)
                .unwrap();
            let manifest = canonical_manifest(good.manifest(), MAX_MANIFEST_BYTES).unwrap();
            connection
                .execute(
                    "INSERT INTO captures(id,manifest) VALUES (?1,?2)",
                    params![good.id(), manifest],
                )
                .unwrap();
            drop(connection);
            assert!(CaptureStore::open(&path, StoreLimits::default()).is_err());
            let connection = Connection::open(&path).unwrap();
            assert_eq!(
                connection
                    .pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
                    .unwrap(),
                version
            );
            assert_eq!(
                connection
                    .query_row("SELECT count(*) FROM captures", [], |row| nonnegative(
                        row, 0
                    ))
                    .unwrap(),
                1
            );
        }
    }

    // AC-0135: logical-byte and count exhaustion publish neither partial objects
    // nor a manifest and do not destroy already-readable captures.
    #[test]
    fn store_capacity_rejection_is_atomic_and_preserves_prior_capture() {
        let first = captured("one", b"1111");
        let second = captured("two", b"2222");
        let first_bytes = canonical_manifest(first.manifest(), MAX_MANIFEST_BYTES)
            .unwrap()
            .len() as u64
            + 4;
        for limits in [
            StoreLimits {
                max_bytes: first_bytes,
                max_captures: 2,
            },
            StoreLimits {
                max_bytes: MAX_STORE_BYTES,
                max_captures: 1,
            },
        ] {
            let mut store = CaptureStore::in_memory(limits).unwrap();
            store.persist(&first).unwrap();
            store.persist(&first).unwrap();
            assert!(store.persist(&second).is_err());
            assert_eq!(counts(&store), (1, 1));
            assert_eq!(
                store.load(first.id()).unwrap().file("one").unwrap().bytes(),
                b"1111"
            );
            assert!(store.load(second.id()).is_err());
        }
        assert!(
            CaptureStore::in_memory(StoreLimits {
                max_bytes: MAX_STORE_BYTES + 1,
                ..StoreLimits::default()
            })
            .is_err()
        );
    }

    // AC-0135: corruption is never repaired using a newly supplied good capture.
    #[test]
    fn corrupt_or_missing_objects_fail_without_repair() {
        let capture = captured("source", b"correct");
        for corrupt in [true, false] {
            let mut store = CaptureStore::in_memory(StoreLimits::default()).unwrap();
            store.persist(&capture).unwrap();
            if corrupt {
                store
                    .connection
                    .execute("UPDATE objects SET bytes=?1", [b"wrong!!".as_slice()])
                    .unwrap();
            } else {
                store.connection.execute("DELETE FROM objects", []).unwrap();
            }
            assert!(store.load(capture.id()).is_err());
            assert!(store.persist(&capture).is_err());
            assert_eq!(counts(&store), (1, u64::from(corrupt)));
        }
    }

    // AC-0135: body lengths/types are rejected by metadata-only queries before
    // fetching a Vec; the transaction keeps that observation and read consistent.
    #[test]
    fn stored_payload_limits_and_types_are_checked_before_loading() {
        let capture = captured("source", b"correct");
        for corruption in [
            "UPDATE objects SET bytes=zeroblob(16777217)",
            "UPDATE captures SET manifest=zeroblob(8388609)",
            "UPDATE objects SET bytes='ééééééé'",
            "UPDATE captures SET manifest='ééééééé'",
        ] {
            let mut store = CaptureStore::in_memory(StoreLimits::default()).unwrap();
            store.persist(&capture).unwrap();
            store.connection.execute(corruption, []).unwrap();
            assert!(store.load(capture.id()).is_err());
        }
    }

    // AC-0135: a corrupt unrelated TEXT row cannot undercount raw UTF-8 bytes
    // in the logical-store capacity aggregate.
    #[test]
    fn non_blob_retained_rows_cannot_bypass_capacity_accounting() {
        let mut store = CaptureStore::in_memory(StoreLimits {
            max_bytes: 1024,
            max_captures: 3,
        })
        .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO objects(digest,bytes) VALUES (?1,?2)",
                params!["0".repeat(64), "é".repeat(400)],
            )
            .unwrap();
        assert!(store.persist(&captured("new", b"payload")).is_err());
        assert_eq!(counts(&store), (0, 1));
    }

    // AC-0135: schema version, canonical identity and internal membership are
    // all validated even when a modified manifest is stored under its new hash.
    #[test]
    fn stored_manifest_version_membership_and_identity_fail_closed() {
        let capture = captured("source", b"correct");
        let mut wrong_version = capture.manifest().clone();
        wrong_version.schema_version += 1;
        let mut duplicate = capture.manifest().clone();
        duplicate.files.push(duplicate.files[0].clone());
        let mut wrong_length = capture.manifest().clone();
        wrong_length.files[0].byte_len += 1;
        for manifest in [wrong_version, duplicate, wrong_length] {
            let mut store = CaptureStore::in_memory(StoreLimits::default()).unwrap();
            store.persist(&capture).unwrap();
            let bytes = serde_json::to_vec(&manifest).unwrap();
            let id = manifest_id(&bytes);
            store
                .connection
                .execute(
                    "INSERT INTO captures(id,manifest,max_span_bytes) VALUES (?1,?2,?3)",
                    params![id, bytes, MAX_SPAN_BYTES as i64],
                )
                .unwrap();
            assert!(store.load(&id).is_err());
            assert!(store.load(capture.id()).is_ok());
        }
        let mut store = CaptureStore::in_memory(StoreLimits::default()).unwrap();
        store.persist(&capture).unwrap();
        store
            .connection
            .execute("UPDATE captures SET manifest=?1", [b"{}".as_slice()])
            .unwrap();
        assert!(store.load(capture.id()).is_err());
        assert!(store.persist(&capture).is_err());
    }

    // AC-0135: a lookalike database, view, missing key or sqliteX-prefixed
    // trigger must not be trusted or repaired as an ordinary capture store.
    #[test]
    fn incompatible_schema_and_disguised_triggers_fail_closed() {
        let missing_key =
            CAPTURES_SCHEMA.replace("id TEXT PRIMARY KEY NOT NULL", "id TEXT NOT NULL");
        let generated = format!(
            "{}, suppression INTEGER GENERATED ALWAYS AS (0) VIRTUAL UNIQUE ON CONFLICT IGNORE)",
            CAPTURES_SCHEMA.strip_suffix(')').unwrap()
        );
        let ignored_conflict = CAPTURES_SCHEMA.replace(
            "PRIMARY KEY NOT NULL",
            "PRIMARY KEY ON CONFLICT IGNORE NOT NULL",
        );
        for schema in [
            format!("PRAGMA user_version={};", STORE_SCHEMA_VERSION + 1),
            format!("PRAGMA user_version={STORE_SCHEMA_VERSION};"),
            format!(
                "{CAPTURES_SCHEMA}; CREATE VIEW objects AS SELECT 'digest' AS digest, randomblob(20) AS bytes; PRAGMA user_version={STORE_SCHEMA_VERSION};"
            ),
            format!("{missing_key}; {OBJECTS_SCHEMA}; PRAGMA user_version={STORE_SCHEMA_VERSION};"),
            format!("{generated}; {OBJECTS_SCHEMA}; PRAGMA user_version={STORE_SCHEMA_VERSION};"),
            format!(
                "{ignored_conflict}; {OBJECTS_SCHEMA}; PRAGMA user_version={STORE_SCHEMA_VERSION};"
            ),
            "CREATE TABLE sqliteXhidden(x TEXT);".into(),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("bad.sqlite");
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch(&schema).unwrap();
            let version: u32 = connection
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            drop(connection);
            assert!(CaptureStore::open(&path, StoreLimits::default()).is_err());
            let connection = Connection::open(&path).unwrap();
            assert_eq!(
                connection
                    .pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))
                    .unwrap(),
                version
            );
        }
        let mut store = CaptureStore::in_memory(StoreLimits::default()).unwrap();
        store.connection.execute_batch("CREATE TRIGGER sqliteXdrop BEFORE INSERT ON captures BEGIN SELECT RAISE(IGNORE); END;").unwrap();
        assert!(store.persist(&captured("source", b"correct")).is_err());
        assert_eq!(counts(&store), (0, 0));
    }

    // AC-0135: signed SQLite values and ignored/extra writes cannot turn into
    // a huge unsigned budget or an apparently successful publication.
    #[test]
    fn negative_sql_values_and_suppressed_publication_fail_closed() {
        let connection = Connection::open_in_memory().unwrap();
        assert!(
            connection
                .query_row("SELECT -1", [], |row| nonnegative(row, 0))
                .is_err()
        );
        assert_eq!(
            connection
                .query_row("SELECT 0", [], |row| nonnegative(row, 0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT 2147483648", [], |row| nonnegative(row, 0))
                .unwrap(),
            MAX_STORE_BYTES
        );
        assert!(require_inserted(0).is_err());
        assert!(require_inserted(2).is_err());
        let transaction = connection.unchecked_transaction().unwrap();
        transaction.execute_batch("CREATE TABLE retained (id INTEGER PRIMARY KEY, raw BLOB); INSERT INTO retained VALUES (1,X'61');").unwrap();
        let ignored = transaction
            .execute("INSERT OR IGNORE INTO retained VALUES (1,X'62')", [])
            .unwrap();
        assert!(require_inserted(ignored).is_err());
        drop(transaction);
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name='retained'",
                    [],
                    |row| nonnegative(row, 0)
                )
                .unwrap(),
            0
        );
    }
}
