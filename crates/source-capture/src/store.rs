use crate::{
    Capture, CaptureError, CaptureLimits, CaptureManifest, CaptureSpanRef, MAX_FILE_BYTES,
    MAX_MANIFEST_BYTES, Result, assemble_capture, canonical_manifest, manifest_id,
    validate_capture_id, validate_manifest,
};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

const STORE_SCHEMA_VERSION: u32 = 1;
const CAPTURES_SCHEMA: &str =
    "CREATE TABLE captures (id TEXT PRIMARY KEY NOT NULL, manifest BLOB NOT NULL)";
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

/// Atomic local raw-source retention, separate from graphs and proposal stores.
///
/// The trusted host supplies a private application location and permissions.
/// There is no eviction, source fallback, source export, or deletion API.
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
    /// Open a host-owned SQLite/WAL path, creating a fresh version-one schema.
    pub fn open(path: &Path, limits: StoreLimits) -> Result<Self> {
        limits.validate()?;
        Self::initialize(Connection::open(path)?, limits)
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
    /// existing rows fail; repeated identical persistence makes no changes.
    pub fn persist(&mut self, capture: &Capture) -> Result<()> {
        validate_manifest(capture.manifest(), CaptureLimits::default())?;
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
            transaction.commit()?;
            return Ok(());
        }
        let invalid_storage: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM captures WHERE typeof(manifest) != 'blob' OR length(manifest) > ?1
                 UNION ALL SELECT 1 FROM objects WHERE typeof(bytes) != 'blob' OR length(bytes) > ?2)",
            params![MAX_MANIFEST_BYTES as i64, MAX_FILE_BYTES as i64], |row| row.get(0)
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
            "INSERT INTO captures(id,manifest) VALUES (?1,?2)",
            params![capture.id(), manifest],
        )?)?;
        transaction.commit()?;
        Ok(())
    }

    /// Load and revalidate bounded metadata and every retained raw file object.
    pub fn load(&self, capture_id: &str) -> Result<Capture> {
        // A read transaction prevents another connection changing length/data
        // between the preallocation bound checks and the actual blob reads.
        let transaction = self.connection.unchecked_transaction()?;
        let capture = load_capture(&transaction, capture_id)?;
        transaction.commit()?;
        Ok(capture)
    }

    /// Validate the full file/range reference and copy the verified span bytes.
    pub fn read_span(&self, reference: &CaptureSpanRef) -> Result<Vec<u8>> {
        let capture = self.load(&reference.file.capture_id)?;
        Ok(capture.read_span(reference)?.to_vec())
    }

    /// Strict UTF-8 span read; no normalization or lossy decoding is performed.
    pub fn read_text_span(&self, reference: &CaptureSpanRef) -> Result<String> {
        let bytes = self.read_span(reference)?;
        String::from_utf8(bytes).map_err(|_| CaptureError::Encoding)
    }
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
        // This is a new private v1 schema: accept exactly the DDL we create,
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
        let expected = vec![
            (key.to_string(), "TEXT".to_string(), 1, None, 1, 0),
            (value.to_string(), "BLOB".to_string(), 1, None, 0, 0),
        ];
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

fn load_capture(connection: &Connection, id: &str) -> Result<Capture> {
    check_version(connection)?;
    validate_capture_id(id)?;
    let metadata: Option<(String, u64)> = connection
        .query_row(
            "SELECT typeof(manifest),length(manifest) FROM captures WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, nonnegative(row, 1)?)),
        )
        .optional()?;
    let (kind, len) = metadata.ok_or(CaptureError::Missing("capture manifest"))?;
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
    let manifest: CaptureManifest = serde_json::from_slice(&raw)?;
    validate_manifest(&manifest, CaptureLimits::default())?;
    if canonical_manifest(&manifest, MAX_MANIFEST_BYTES)? != raw {
        return Err(CaptureError::Corrupt("noncanonical stored manifest"));
    }
    let mut buffers = BTreeMap::new();
    for entry in &manifest.files {
        let bytes = read_object(connection, &entry.digest)?
            .ok_or(CaptureError::Missing("captured object"))?;
        if bytes.len() as u64 != entry.byte_len {
            return Err(CaptureError::Corrupt("manifest and object length disagree"));
        }
        buffers.insert(entry.path.clone(), bytes);
    }
    let capture = assemble_capture(manifest, buffers, CaptureLimits::default())?;
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
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(name), bytes).unwrap();
        capture_working_tree(
            dir.path(),
            &SourceId::new("host-store-fixture").unwrap(),
            &[name.into()],
            CaptureLimits::default(),
        )
        .unwrap()
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
                    "INSERT INTO captures(id,manifest) VALUES (?1,?2)",
                    params![id, bytes],
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

    // AC-0135: a lookalike v1 database, view, missing key or sqliteX-prefixed
    // trigger must not be trusted or repaired as an ordinary capture store.
    #[test]
    fn incompatible_schema_and_disguised_triggers_fail_closed() {
        for schema in [
            "PRAGMA user_version=2;",
            "PRAGMA user_version=1;",
            "CREATE TABLE captures(id TEXT PRIMARY KEY NOT NULL,manifest BLOB NOT NULL); CREATE VIEW objects AS SELECT 'digest' AS digest, randomblob(20) AS bytes; PRAGMA user_version=1;",
            "CREATE TABLE captures(id TEXT NOT NULL,manifest BLOB NOT NULL); CREATE TABLE objects(digest TEXT PRIMARY KEY NOT NULL,bytes BLOB NOT NULL); PRAGMA user_version=1;",
            "CREATE TABLE captures (id TEXT PRIMARY KEY NOT NULL, manifest BLOB NOT NULL, suppression INTEGER GENERATED ALWAYS AS (0) VIRTUAL UNIQUE ON CONFLICT IGNORE); CREATE TABLE objects (digest TEXT PRIMARY KEY NOT NULL, bytes BLOB NOT NULL); PRAGMA user_version=1;",
            "CREATE TABLE captures (id TEXT PRIMARY KEY ON CONFLICT IGNORE NOT NULL, manifest BLOB NOT NULL); CREATE TABLE objects (digest TEXT PRIMARY KEY NOT NULL, bytes BLOB NOT NULL); PRAGMA user_version=1;",
            "CREATE TABLE sqliteXhidden(x TEXT);",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("bad.sqlite");
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch(schema).unwrap();
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
