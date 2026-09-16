use super::*;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryCursor {
    version: u32,
    namespace: String,
    conversation: Option<String>,
    high: i64,
    before: i64,
}

impl JobStore {
    pub(crate) fn investigation_history(
        &self,
        conversation_id: Option<&str>,
        cursor: Option<&str>,
    ) -> Result<InvestigationPage, InvestigationStoreError> {
        if conversation_id.is_some_and(|id| !id_valid(id)) {
            return Err(InvestigationStoreError::Invalid);
        }
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let selection = match cursor {
            Some(cursor) => {
                if cursor.len() > 512 {
                    return Err(InvestigationStoreError::Invalid);
                }
                let cursor: HistoryCursor = decode_record(cursor)?;
                if cursor.version != 1
                    || cursor.namespace != self.namespace.value
                    || cursor.conversation.as_deref() != conversation_id
                    || cursor.high < 0
                    || cursor.before <= 0
                    || cursor.before > cursor.high
                {
                    return Err(InvestigationStoreError::Invalid);
                }
                cursor
            }
            None => {
                let high: i64 = tx.query_row(
                    "SELECT coalesce(max(ordinal),0) FROM investigation_tasks",
                    [],
                    |r| r.get(0),
                )?;
                HistoryCursor {
                    version: 1,
                    namespace: self.namespace.value.clone(),
                    conversation: conversation_id.map(str::to_owned),
                    high,
                    before: high
                        .checked_add(1)
                        .ok_or(InvestigationStoreError::Capacity)?,
                }
            }
        };
        let rows=tx.prepare("SELECT ordinal,CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB))<=128 THEN id END FROM investigation_tasks WHERE ordinal<=?1 AND ordinal<?2 AND (?3 IS NULL OR conversation_id=?3) ORDER BY ordinal DESC LIMIT 51")?
            .query_map(params![selection.high,selection.before,conversation_id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,Option<String>>(1)?)))?
            .collect::<Result<Vec<_>,_>>()?;
        let mut items = Vec::new();
        let mut remaining = 256 * 1024 - 4096;
        let mut before = selection.before;
        let mut more = false;
        for (ordinal, id) in rows {
            let id = id.ok_or(InvestigationStoreError::Invalid)?;
            if ordinal <= 0 {
                return Err(InvestigationStoreError::Invalid);
            }
            let task = load_task(&tx, &id)?;
            let size = encode(&task.detail.summary, 4096)?.len() + 1;
            if items.len() == 50 || size > remaining {
                more = true;
                break;
            }
            remaining -= size;
            before = ordinal;
            items.push(task.detail.summary);
        }
        let next_cursor = if more {
            Some(
                String::from_utf8(encode(
                    &HistoryCursor {
                        before,
                        ..selection
                    },
                    512,
                )?)
                .map_err(|_| InvestigationStoreError::Invalid)?,
            )
        } else {
            None
        };
        let page = InvestigationPage { items, next_cursor };
        encode(&page, 256 * 1024)?;
        tx.commit()?;
        Ok(page)
    }

    pub(crate) fn investigation_events(
        &self,
        id: &str,
        after_sequence: u64,
    ) -> Result<InvestigationEventPage, InvestigationStoreError> {
        if after_sequence > MAX_EVENTS {
            return Err(InvestigationStoreError::Invalid);
        }
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let task = load_task(&tx, id)?;
        let sequences=tx.prepare("SELECT sequence FROM investigation_events WHERE task_id=?1 AND sequence>?2 ORDER BY sequence LIMIT 51")?
            .query_map(params![id,signed(after_sequence)?],|r|r.get::<_,i64>(0))?.collect::<Result<Vec<_>,_>>()?;
        let mut items = Vec::new();
        let mut remaining = 256 * 1024 - 4096;
        let mut next_sequence = after_sequence;
        let mut has_more = false;
        for sequence in sequences {
            let event: InvestigationEvent =
                read_body(&tx, "investigation_events", "sequence", id, &sequence)?
                    .ok_or(InvestigationStoreError::Invalid)?;
            if event.investigation_id != id
                || signed(event.sequence)? != sequence
                || event.sequence != next_sequence + 1
                || event.sequence > task.detail.summary.last_event_sequence
                || event.revision > task.detail.summary.revision
                || event.summary.len() > 1024
                || redacted_history_text(&event.summary, 1024)? != event.summary
                || event.step_id.as_ref().is_some_and(|v| !id_valid(v))
                || event.created_at.len() > 32
            {
                return Err(InvestigationStoreError::Invalid);
            }
            let size = encode(&event, 16 * 1024)?.len() + 1;
            if items.len() == 50 || size > remaining {
                has_more = true;
                break;
            }
            remaining -= size;
            next_sequence = event.sequence;
            items.push(event);
        }
        if !has_more && next_sequence < task.detail.summary.last_event_sequence {
            return Err(InvestigationStoreError::Invalid);
        }
        let page = InvestigationEventPage {
            investigation_id: id.into(),
            items,
            next_sequence,
            has_more,
        };
        encode(&page, 256 * 1024)?;
        tx.commit()?;
        Ok(page)
    }

    pub(crate) fn investigation_receipt_references(
        &self,
        source_id: &str,
    ) -> Result<Vec<InvestigationReceiptUse>, InvestigationStoreError> {
        if !id_valid(source_id) {
            return Err(InvestigationStoreError::Invalid);
        }
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        // Reconcile every immutable admitted ledger before applying the source
        // filter. Checking only matching index rows cannot detect a deleted or
        // mislabelled reference, even when the canonical triggers are restored.
        let (tasks, inputs, refs): (i64, i64, i64) = tx.query_row(
            "SELECT (SELECT count(*) FROM investigation_tasks), (SELECT count(*) FROM investigation_inputs), (SELECT count(*) FROM investigation_refs)",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if !(0..=128).contains(&tasks)
            || !(0..=1152).contains(&inputs)
            || !(0..=8192).contains(&refs)
        {
            return Err(InvestigationStoreError::Capacity);
        }
        let ids = tx.prepare("SELECT CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB))<=128 THEN id END FROM investigation_tasks ORDER BY id")?
            .query_map([], |row| row.get::<_, Option<String>>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        type ReferenceKey = (String, String, String, String);
        let mut expected = std::collections::BTreeMap::<ReferenceKey, i64>::new();
        let mut checked_inputs = 0i64;
        for id in ids {
            let id = id.ok_or(InvestigationStoreError::Invalid)?;
            let task = load_task(&tx, &id)?;
            // Verify the current pointer/hash as well as the independently
            // retained revisions. Historical references are never subtracted.
            let current = load_ledger(&tx, &task)?;
            let revisions = tx.prepare("SELECT revision FROM investigation_inputs WHERE task_id=?1 ORDER BY revision LIMIT 10")?
                .query_map([&id], |row| row.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            if revisions.len() > 9
                || revisions.last().copied() != task.ledger_revision.map(signed).transpose()?
            {
                return Err(InvestigationStoreError::Invalid);
            }
            for revision in revisions {
                checked_inputs += 1;
                let ledger: InvestigationInputLedger =
                    read_body(&tx, "investigation_inputs", "revision", &id, &revision)?
                        .ok_or(InvestigationStoreError::Invalid)?;
                ledger.validate()?;
                encode(&ledger, MAX_MANIFEST_BYTES)?;
                if signed(ledger.revision)? != revision
                    || Some(&ledger.graph_snapshot_id)
                        != task.detail.summary.graph_snapshot_id.as_ref()
                    || Some(&ledger.scope_snapshot_id) != task.detail.scope_snapshot_id.as_ref()
                    || current.as_ref().is_none_or(|latest| {
                        !ledger
                            .receipt_references
                            .iter()
                            .all(|reference| latest.receipt_references.contains(reference))
                    })
                {
                    return Err(InvestigationStoreError::Invalid);
                }
                for reference in ledger.receipt_references {
                    expected
                        .entry((
                            id.clone(),
                            reference.source_id,
                            reference.repo_key,
                            reference.receipt_id,
                        ))
                        .or_insert(revision);
                    if expected.len() > 8192 {
                        return Err(InvestigationStoreError::Capacity);
                    }
                }
            }
        }
        if checked_inputs != inputs || i64::try_from(expected.len()).ok() != Some(refs) {
            return Err(InvestigationStoreError::Invalid);
        }
        let rows = tx.prepare("SELECT CASE WHEN typeof(task_id)='text' AND length(CAST(task_id AS BLOB))<=128 THEN task_id END,input_revision,CASE WHEN typeof(source_id)='text' AND length(CAST(source_id AS BLOB))<=256 THEN source_id END,CASE WHEN typeof(repo_key)='text' AND length(CAST(repo_key AS BLOB))<=256 THEN repo_key END,CASE WHEN typeof(receipt_id)='text' AND length(CAST(receipt_id AS BLOB))<=256 THEN receipt_id END FROM investigation_refs ORDER BY task_id,source_id,repo_key,receipt_id")?
            .query_map([], |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?, row.get::<_, Option<String>>(4)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut references = Vec::new();
        for (task_id, input_revision, indexed_source, repo_key, receipt_id) in rows {
            let (Some(task_id), Some(indexed_source), Some(repo_key), Some(receipt_id)) =
                (task_id, indexed_source, repo_key, receipt_id)
            else {
                return Err(InvestigationStoreError::Invalid);
            };
            let key = (
                task_id.clone(),
                indexed_source.clone(),
                repo_key.clone(),
                receipt_id.clone(),
            );
            if expected.remove(&key) != Some(input_revision) {
                return Err(InvestigationStoreError::Invalid);
            }
            if indexed_source == source_id {
                references.push(InvestigationReceiptUse {
                    investigation_id: task_id,
                    source_id: indexed_source,
                    repo_key,
                    receipt_id,
                });
            }
        }
        if !expected.is_empty() {
            return Err(InvestigationStoreError::Invalid);
        }
        tx.commit()?;
        Ok(references)
    }
}
