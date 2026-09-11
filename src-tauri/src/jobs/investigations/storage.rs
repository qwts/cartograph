use super::*;
use std::io::Write;

fn schema() -> Vec<(String, String, String)> {
    let mut objects = vec![
        ("investigation_meta", "CREATE TABLE investigation_meta (singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=1), namespace TEXT NOT NULL) STRICT"),
        ("investigation_conversations", "CREATE TABLE investigation_conversations (id TEXT PRIMARY KEY NOT NULL, created_at TEXT NOT NULL) STRICT"),
        ("investigation_tasks", "CREATE TABLE investigation_tasks (ordinal INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, nonce TEXT NOT NULL UNIQUE, conversation_id TEXT NOT NULL, job_id INTEGER NOT NULL UNIQUE, revision INTEGER NOT NULL CHECK(revision>=0), live INTEGER NOT NULL CHECK(live IN (0,1)), record BLOB NOT NULL, record_hash TEXT NOT NULL) STRICT"),
        ("investigation_events", "CREATE TABLE investigation_events (task_id TEXT NOT NULL, sequence INTEGER NOT NULL, body BLOB NOT NULL, body_hash TEXT NOT NULL, PRIMARY KEY(task_id,sequence)) STRICT"),
        ("investigation_inputs", "CREATE TABLE investigation_inputs (task_id TEXT NOT NULL, revision INTEGER NOT NULL, body BLOB NOT NULL, body_hash TEXT NOT NULL, PRIMARY KEY(task_id,revision)) STRICT"),
        ("investigation_invocations", "CREATE TABLE investigation_invocations (task_id TEXT NOT NULL, step_id TEXT NOT NULL, body BLOB NOT NULL, body_hash TEXT NOT NULL, PRIMARY KEY(task_id,step_id)) STRICT"),
        ("investigation_responses", "CREATE TABLE investigation_responses (task_id TEXT NOT NULL, step_id TEXT NOT NULL, body BLOB NOT NULL, body_hash TEXT NOT NULL, PRIMARY KEY(task_id,step_id)) STRICT"),
        ("investigation_results", "CREATE TABLE investigation_results (task_id TEXT PRIMARY KEY NOT NULL, body BLOB NOT NULL, body_hash TEXT NOT NULL) STRICT"),
        ("investigation_refs", "CREATE TABLE investigation_refs (task_id TEXT NOT NULL, input_revision INTEGER NOT NULL, source_id TEXT NOT NULL, repo_key TEXT NOT NULL, receipt_id TEXT NOT NULL, PRIMARY KEY(task_id,source_id,repo_key,receipt_id)) STRICT"),
    ].into_iter().map(|(name, sql)| (name.to_owned(), "table".to_owned(), sql.to_owned())).collect::<Vec<_>>();
    objects.push(("investigation_refs_source".into(), "index".into(), "CREATE INDEX investigation_refs_source ON investigation_refs(source_id,task_id,repo_key,receipt_id)".into()));
    for table in [
        "investigation_events",
        "investigation_inputs",
        "investigation_invocations",
        "investigation_responses",
        "investigation_results",
        "investigation_refs",
    ] {
        for verb in ["UPDATE", "DELETE"] {
            let name = format!("{table}_no_{}", verb.to_lowercase());
            objects.push((name.clone(), "trigger".into(), format!("CREATE TRIGGER {name} BEFORE {verb} ON {table} BEGIN SELECT RAISE(ABORT, 'immutable investigation record'); END")));
        }
    }
    objects
}

const OWNED: &str =
    "name GLOB 'investigation_*' OR (tbl_name GLOB 'investigation_*' AND name NOT GLOB 'sqlite_*')";

pub(super) fn initialize(
    connection: &mut Connection,
    namespace: &ExecutionNamespace,
) -> Result<(), InvestigationStoreError> {
    connection.pragma_update(None, "synchronous", "FULL")?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let count: i64 = tx.query_row(
        &format!("SELECT count(*) FROM sqlite_schema WHERE {OWNED}"),
        [],
        |r| r.get(0),
    )?;
    if count == 0 {
        for (_, _, sql) in schema() {
            tx.execute_batch(&sql)?;
        }
        one(tx.execute(
            "INSERT INTO investigation_meta VALUES (1,1,?1)",
            [&namespace.value],
        )?)?;
    }
    validate(&tx, namespace)?;
    tx.commit()?;
    Ok(())
}

pub(super) fn validate(
    connection: &Connection,
    namespace: &ExecutionNamespace,
) -> Result<(), InvestigationStoreError> {
    execution::validate(connection, namespace)?;
    let expected = schema();
    let (count, bounded): (i64, bool) = connection.query_row(
        &format!("SELECT count(*), coalesce(min(typeof(name)='text' AND length(CAST(name AS BLOB))<=128 AND typeof(type)='text' AND length(CAST(type AS BLOB))<=16 AND typeof(sql)='text' AND length(CAST(sql AS BLOB))<=4096),0) FROM sqlite_schema WHERE {OWNED}"),
        [], |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if usize::try_from(count).ok() != Some(expected.len()) || !bounded {
        return Err(InvestigationStoreError::Invalid);
    }
    let actual = connection
        .prepare(&format!(
            "SELECT name,type,sql FROM sqlite_schema WHERE {OWNED}"
        ))?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if expected.iter().any(|item| !actual.contains(item)) {
        return Err(InvestigationStoreError::Invalid);
    }
    let rows: i64 =
        connection.query_row("SELECT count(*) FROM investigation_meta", [], |r| r.get(0))?;
    let valid: bool = connection.query_row("SELECT coalesce(min(singleton=1 AND version=1 AND typeof(namespace)='text' AND length(CAST(namespace AS BLOB))=32 AND namespace=?1),0) FROM investigation_meta", [&namespace.value], |r| r.get(0))?;
    if rows != 1 || !valid {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
}

struct BoundedWriter {
    bytes: Vec<u8>,
    maximum: usize,
}
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.maximum.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("bounded investigation record"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn encode(
    value: &impl Serialize,
    maximum: usize,
) -> Result<Vec<u8>, InvestigationStoreError> {
    let mut writer = BoundedWriter {
        bytes: Vec::new(),
        maximum,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| InvestigationStoreError::Capacity)?;
    Ok(writer.bytes)
}

fn decode<T: serde::de::DeserializeOwned + Serialize>(
    bytes: &[u8],
    hash: &str,
) -> Result<T, InvestigationStoreError> {
    if !hash_valid(hash) || core_prov::content_hash(bytes) != hash {
        return Err(InvestigationStoreError::Invalid);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| InvestigationStoreError::Invalid)?;
    let value = decode_record(text)?;
    if encode(&value, MAX_RECORD_BYTES)? != bytes {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(value)
}

pub(super) fn timestamp(connection: &Connection) -> Result<String, InvestigationStoreError> {
    Ok(
        connection.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%SZ','now')", [], |r| {
            r.get(0)
        })?,
    )
}

pub(super) fn new_id(
    connection: &Connection,
    prefix: &str,
) -> Result<String, InvestigationStoreError> {
    let suffix: String =
        connection.query_row("SELECT lower(hex(randomblob(16)))", [], |r| r.get(0))?;
    Ok(format!("{prefix}:{suffix}"))
}

pub(super) fn signed(value: u64) -> Result<i64, InvestigationStoreError> {
    i64::try_from(value).map_err(|_| InvestigationStoreError::Invalid)
}

pub(super) fn validate_provider(
    provider: &InvestigationProvider,
) -> Result<(), InvestigationStoreError> {
    if !provider.available || provider.unavailable_reason.is_some() {
        return Err(InvestigationStoreError::Unavailable);
    }
    for value in [&provider.provider_id, &provider.model, &provider.endpoint]
        .into_iter()
        .chain(provider.deployment.iter())
    {
        if value.is_empty() || value.len() > 2048 || redacted_history_text(value, 2048)? != *value {
            return Err(InvestigationStoreError::Invalid);
        }
    }
    Ok(())
}

pub(super) fn validate_usage(
    usage: &InvestigationUsage,
    limits: &InvestigationLimits,
) -> Result<(), InvestigationStoreError> {
    if usage.model_invocations > limits.model_invocations
        || usage.tool_actions > limits.tool_actions
        || usage.selected_facts > limits.selected_facts
        || usage.evidence_requests > limits.evidence_requests
        || usage.evidence_items > limits.evidence_items
        || usage.evidence_bytes > limits.evidence_bytes
        || usage.captured_validation_bytes > limits.captured_validation_bytes
        || usage.generated_token_reservations > limits.generated_token_reservations
    {
        return Err(InvestigationStoreError::Capacity);
    }
    Ok(())
}

fn validate_task(task: &StoredTask) -> Result<(), InvestigationStoreError> {
    let detail = &task.detail;
    let summary = &detail.summary;
    if task.schema_version != 1
        || summary.schema_version != 1
        || !id_valid(&summary.investigation_id)
        || !id_valid(&summary.conversation_id)
        || !id_valid(&task.nonce)
        || !hash_valid(&task.intent_hash)
        || summary.job_id <= 0
        || summary.parent_id.as_ref().is_some_and(|v| !id_valid(v))
        || detail.context_owner.len() != 32
        || !detail
            .context_owner
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || detail.specialist != summary.specialist_id.definition()?
        || detail.provider.mode != summary.provider_mode
        || detail.limits != InvestigationLimits::default()
        || redacted_history_text(&summary.question, MAX_QUESTION_BYTES)? != summary.question
        || summary.last_event_sequence > MAX_EVENTS
        || summary.actions.can_cancel
            != (task.phase != Phase::Terminal && !summary.cancel_requested)
        || !summary.actions.can_follow_up
        || summary.invocation_pending != (task.phase == Phase::Invocation)
        || task.ledger_hash.is_some() != task.ledger_revision.is_some()
        || task.ledger_hash.as_ref().is_some_and(|v| !hash_valid(v))
        || detail.citations.len() > 76
        || summary.created_at.len() > 32
        || summary.updated_at.len() > 32
    {
        return Err(InvestigationStoreError::Invalid);
    }
    summary.scope.validate()?;
    validate_provider(&detail.provider)?;
    validate_usage(&detail.usage, &detail.limits)?;
    let valid_status = match task.phase {
        Phase::Queued => summary.status == InvestigationStatus::Queued && task.execution.is_none(),
        Phase::Preparing => {
            summary.status == InvestigationStatus::Preparing && task.execution.is_some()
        }
        Phase::Consent => {
            summary.status == InvestigationStatus::AwaitingConsent && task.pending.is_some()
        }
        Phase::Ready | Phase::Invocation | Phase::Action | Phase::Tool => {
            summary.status == InvestigationStatus::Running
        }
        Phase::Terminal => matches!(
            summary.status,
            InvestigationStatus::Completed
                | InvestigationStatus::Failed
                | InvestigationStatus::Cancelled
                | InvestigationStatus::Interrupted
                | InvestigationStatus::OutcomeUnknown
        ),
    };
    if !valid_status || (summary.has_result != (summary.status == InvestigationStatus::Completed)) {
        return Err(InvestigationStoreError::Invalid);
    }
    if let Some(identity) = &task.execution {
        if identity.namespace != detail.context_owner
            || identity.job_id != summary.job_id
            || identity.generation <= 0
            || identity.owner.len() != 32
            || !identity
                .owner
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(InvestigationStoreError::Invalid);
        }
    } else if !matches!(task.phase, Phase::Queued | Phase::Terminal) {
        return Err(InvestigationStoreError::Invalid);
    }
    if let Some(pending) = &task.pending {
        validate_step(pending)?;
    }
    if task.pending.is_some() != matches!(task.phase, Phase::Consent | Phase::Invocation)
        || (task.approved && task.phase != Phase::Consent)
        || task.action_tool.is_some() != matches!(task.phase, Phase::Action | Phase::Tool)
    {
        return Err(InvestigationStoreError::Invalid);
    }
    encode(summary, 4096)?;
    Ok(())
}

pub(super) fn validate_step(
    step: &PendingInvestigationStep,
) -> Result<(), InvestigationStoreError> {
    if !id_valid(&step.step_id)
        || !hash_valid(&step.payload_hash)
        || !hash_valid(&step.input_ledger_hash)
        || step.input_bytes == 0
        || step.input_bytes > MAX_INPUT_BYTES
        || step.generated_tokens == 0
        || step.generated_tokens > 2048
    {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
}

pub(super) fn load_task(
    connection: &Connection,
    id: &str,
) -> Result<StoredTask, InvestigationStoreError> {
    if !id_valid(id) {
        return Err(InvestigationStoreError::Invalid);
    }
    let row = connection.query_row(
        "SELECT CASE WHEN typeof(record)='blob' AND length(record)<=262144 THEN record END, CASE WHEN typeof(record_hash)='text' AND length(CAST(record_hash AS BLOB))=64 THEN record_hash END, revision, live, job_id, CASE WHEN typeof(nonce)='text' AND length(CAST(nonce AS BLOB))<=128 THEN nonce END, CASE WHEN typeof(conversation_id)='text' AND length(CAST(conversation_id AS BLOB))<=128 THEN conversation_id END FROM investigation_tasks WHERE id=?1",
        [id], |r| Ok((r.get::<_,Option<Vec<u8>>>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,Option<String>>(6)?)),
    ).optional()?.ok_or(InvestigationStoreError::Missing)?;
    let (Some(bytes), Some(hash), revision, live, job_id, Some(nonce), Some(conversation)) = row
    else {
        return Err(InvestigationStoreError::Invalid);
    };
    let task: StoredTask = decode(&bytes, &hash)?;
    validate_task(&task)?;
    let namespace: String = connection.query_row(
        "SELECT namespace FROM investigation_meta WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if task.detail.summary.investigation_id != id
        || signed(task.detail.summary.revision)? != revision
        || live != i64::from(task.phase != Phase::Terminal)
        || task.detail.summary.job_id != job_id
        || task.nonce != nonce
        || task.detail.summary.conversation_id != conversation
        || task.detail.context_owner != namespace
    {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(task)
}

pub(super) fn insert_task(
    connection: &Connection,
    task: &StoredTask,
) -> Result<(), InvestigationStoreError> {
    validate_task(task)?;
    let bytes = encode(task, MAX_RECORD_BYTES)?;
    one(connection.execute("INSERT INTO investigation_tasks(id,nonce,conversation_id,job_id,revision,live,record,record_hash) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)", params![task.detail.summary.investigation_id,task.nonce,task.detail.summary.conversation_id,task.detail.summary.job_id,signed(task.detail.summary.revision)?,i64::from(task.phase!=Phase::Terminal),bytes,core_prov::content_hash(&bytes)])?)
}

pub(super) fn save_task(
    connection: &Connection,
    task: &StoredTask,
    expected_revision: u64,
    terminal: bool,
) -> Result<(), InvestigationStoreError> {
    validate_task(task)?;
    let bytes = encode(task, MAX_RECORD_BYTES)?;
    one(connection.execute("UPDATE investigation_tasks SET revision=?2,live=?3,record=?4,record_hash=?5 WHERE id=?1 AND revision=?6", params![task.detail.summary.investigation_id,signed(task.detail.summary.revision)?,i64::from(task.phase!=Phase::Terminal),bytes,core_prov::content_hash(&bytes),signed(expected_revision)?])?)?;
    check_task_capacity(connection, &task.detail.summary.investigation_id, terminal)
}

pub(super) fn check_start_capacity(
    connection: &Connection,
    new_conversation: bool,
) -> Result<(), InvestigationStoreError> {
    let (tasks, live, conversations): (i64,i64,i64) = connection.query_row("SELECT (SELECT count(*) FROM investigation_tasks), (SELECT count(*) FROM investigation_tasks WHERE live=1), (SELECT count(*) FROM investigation_conversations)", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let tasks = usize::try_from(tasks).map_err(|_| InvestigationStoreError::Invalid)?;
    let live = usize::try_from(live).map_err(|_| InvestigationStoreError::Invalid)?;
    let conversations =
        usize::try_from(conversations).map_err(|_| InvestigationStoreError::Invalid)?;
    if tasks >= MAX_TASKS
        || live >= MAX_LIVE_TASKS
        || (new_conversation && conversations >= MAX_TASKS)
        || tasks
            .checked_add(1)
            .and_then(|v| v.checked_mul(TASK_CAPACITY))
            .is_none_or(|v| v > STORE_CAPACITY)
    {
        return Err(InvestigationStoreError::Capacity);
    }
    Ok(())
}

pub(super) fn check_task_capacity(
    connection: &Connection,
    id: &str,
    terminal: bool,
) -> Result<(), InvestigationStoreError> {
    // Fixed scalar/index/namespace/conversation allowance is charged in addition
    // to every stored byte body. SQL BLOB casts prevent malformed TEXT rows from
    // undercounting multibyte strings. SQLite pages/WAL are deliberately excluded.
    let mut bytes: usize = 1024;
    let record: i64 = connection.query_row("SELECT length(CAST(record AS BLOB))+length(CAST(record_hash AS BLOB))+length(CAST(id AS BLOB))+length(CAST(nonce AS BLOB))+length(CAST(conversation_id AS BLOB)) FROM investigation_tasks WHERE id=?1", [id], |r| r.get(0))?;
    bytes = bytes
        .checked_add(usize::try_from(record).map_err(|_| InvestigationStoreError::Invalid)?)
        .ok_or(InvestigationStoreError::Capacity)?;
    for table in [
        "investigation_events",
        "investigation_inputs",
        "investigation_invocations",
        "investigation_responses",
        "investigation_results",
    ] {
        let sum:i64=connection.query_row(&format!("SELECT coalesce(sum(length(CAST(body AS BLOB))+length(CAST(body_hash AS BLOB))+256),0) FROM {table} WHERE task_id=?1"), [id], |r| r.get(0))?;
        bytes = bytes
            .checked_add(usize::try_from(sum).map_err(|_| InvestigationStoreError::Invalid)?)
            .ok_or(InvestigationStoreError::Capacity)?;
    }
    let refs:i64=connection.query_row("SELECT coalesce(sum(length(CAST(task_id AS BLOB))+length(CAST(source_id AS BLOB))+length(CAST(repo_key AS BLOB))+length(CAST(receipt_id AS BLOB))+128),0) FROM investigation_refs WHERE task_id=?1", [id], |r| r.get(0))?;
    bytes = bytes
        .checked_add(usize::try_from(refs).map_err(|_| InvestigationStoreError::Invalid)?)
        .ok_or(InvestigationStoreError::Capacity)?;
    let maximum = if terminal {
        TASK_CAPACITY
    } else {
        TASK_CAPACITY - TERMINAL_RESERVE
    };
    if bytes > maximum {
        return Err(InvestigationStoreError::Capacity);
    }
    Ok(())
}

pub(super) fn event(
    connection: &Connection,
    task: &mut StoredTask,
    kind: InvestigationEventKind,
    step: Option<&str>,
    tool: Option<InvestigationTool>,
    summary: &'static str,
) -> Result<(), InvestigationStoreError> {
    let next = task
        .detail
        .summary
        .last_event_sequence
        .checked_add(1)
        .ok_or(InvestigationStoreError::Capacity)?;
    let maximum = if task.phase == Phase::Terminal {
        MAX_EVENTS
    } else if kind == InvestigationEventKind::CancelRequested {
        MAX_EVENTS - 2
    } else {
        MAX_EVENTS - 3
    };
    if next > maximum {
        return Err(InvestigationStoreError::Capacity);
    }
    task.detail.summary.last_event_sequence = next;
    let event = InvestigationEvent {
        investigation_id: task.detail.summary.investigation_id.clone(),
        sequence: next,
        revision: task.detail.summary.revision,
        kind,
        step_id: step.map(str::to_owned),
        tool,
        summary: summary.into(),
        created_at: timestamp(connection)?,
    };
    let body = encode(&event, 16 * 1024)?;
    one(connection.execute(
        "INSERT INTO investigation_events VALUES (?1,?2,?3,?4)",
        params![
            event.investigation_id,
            signed(next)?,
            body,
            core_prov::content_hash(&body)
        ],
    )?)
}

pub(super) fn bump(
    connection: &Connection,
    task: &mut StoredTask,
) -> Result<(), InvestigationStoreError> {
    task.detail.summary.revision = task
        .detail
        .summary
        .revision
        .checked_add(1)
        .ok_or(InvestigationStoreError::Capacity)?;
    task.detail.summary.updated_at = timestamp(connection)?;
    task.detail.summary.actions.can_cancel =
        task.phase != Phase::Terminal && !task.detail.summary.cancel_requested;
    task.detail.summary.invocation_pending = task.phase == Phase::Invocation;
    Ok(())
}

pub(super) fn read_body<T: serde::de::DeserializeOwned + Serialize>(
    connection: &Connection,
    table: &str,
    key_column: &str,
    id: &str,
    key: &dyn rusqlite::ToSql,
) -> Result<Option<T>, InvestigationStoreError> {
    let sql = format!(
        "SELECT CASE WHEN typeof(body)='blob' AND length(body)<=262144 THEN body END, CASE WHEN typeof(body_hash)='text' AND length(CAST(body_hash AS BLOB))=64 THEN body_hash END FROM {table} WHERE task_id=?1 AND {key_column}=?2"
    );
    let row = connection
        .query_row(&sql, params![id, key], |r| {
            Ok((
                r.get::<_, Option<Vec<u8>>>(0)?,
                r.get::<_, Option<String>>(1)?,
            ))
        })
        .optional()?;
    row.map(|(body, hash)| {
        let (Some(body), Some(hash)) = (body, hash) else {
            return Err(InvestigationStoreError::Invalid);
        };
        decode(&body, &hash)
    })
    .transpose()
}

pub(super) fn load_ledger(
    connection: &Connection,
    task: &StoredTask,
) -> Result<Option<InvestigationInputLedger>, InvestigationStoreError> {
    let Some(revision) = task.ledger_revision else {
        return Ok(None);
    };
    let ledger: InvestigationInputLedger = read_body(
        connection,
        "investigation_inputs",
        "revision",
        &task.detail.summary.investigation_id,
        &signed(revision)?,
    )?
    .ok_or(InvestigationStoreError::Invalid)?;
    ledger.validate()?;
    if Some(ledger.fingerprint()?) != task.ledger_hash
        || ledger.revision != revision
        || Some(&ledger.graph_snapshot_id) != task.detail.summary.graph_snapshot_id.as_ref()
        || Some(&ledger.scope_snapshot_id) != task.detail.scope_snapshot_id.as_ref()
        || ledger.citations != task.detail.citations
    {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(Some(ledger))
}

pub(super) fn load_result(
    connection: &Connection,
    task: &StoredTask,
) -> Result<Option<InvestigationResult>, InvestigationStoreError> {
    let id = &task.detail.summary.investigation_id;
    let result: Option<InvestigationResult> =
        read_body(connection, "investigation_results", "task_id", id, &id)?;
    if result.is_some() != task.detail.summary.has_result {
        return Err(InvestigationStoreError::Invalid);
    }
    if let Some(result) = &result {
        let ledger = load_ledger(connection, task)?.ok_or(InvestigationStoreError::Invalid)?;
        result.validate(&ledger)?;
        if result.investigation_id != *id {
            return Err(InvestigationStoreError::Invalid);
        }
    }
    Ok(result)
}

pub(super) fn insert_body(
    connection: &Connection,
    table: &str,
    id: &str,
    key: &dyn rusqlite::ToSql,
    value: &impl Serialize,
    maximum: usize,
) -> Result<(), InvestigationStoreError> {
    let body = encode(value, maximum)?;
    one(connection.execute(
        &format!("INSERT INTO {table} VALUES (?1,?2,?3,?4)"),
        params![id, key, body, core_prov::content_hash(&body)],
    )?)
}
