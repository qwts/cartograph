use super::*;
use crate::PreparedAgentTask;
use crate::prepared::tests::{change_receipt, fixture, result};
use serde_json::json;

fn stage(store: &mut ProposalStore, task: &PreparedAgentTask, job: i64) -> StagedProposal {
    store
        .stage_prepared(
            task,
            &result(task, "Possible target link", &["source", "target"]),
            job,
            &task.source_basis().graph_snapshot_id,
        )
        .unwrap()
}

#[test]
fn prepared_staging_binds_copied_input_and_preserves_id_only_review_history() {
    // AC-0175/AC-0178: copied input can finish without any source/current-state
    // read. Every supplied item is fingerprinted; no raw source/summary is stored.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stages.sqlite");
    let task = fixture();
    let mut store = ProposalStore::open(&path).unwrap();
    let first = stage(&mut store, &task, 1);
    assert_eq!(first.schema_version, 2);
    assert_eq!(first.basis.schema_version, 2);
    assert_eq!(first.evidence_binding, EvidenceBinding::PerItem);
    assert_eq!(first.source_basis.as_ref(), Some(task.source_basis()));
    let raw: String = store
        .conn
        .query_row(
            "SELECT immutable_json FROM staged_agent_proposals WHERE proposal_id=?1",
            [&first.proposal_id],
            |r| r.get(0),
        )
        .unwrap();
    for text in task
        .task()
        .evidence
        .iter()
        .map(|e| &e.text)
        .chain(task.task().candidates.iter().map(|c| &c.summary))
    {
        assert!(!raw.contains(text));
    }
    let changed = change_receipt(&task);
    let second = stage(&mut store, &changed, 1);
    assert_ne!(first.proposal_id, second.proposal_id);
    let accepted = store
        .review(&first.proposal_id, 0, ProposalDecision::Accepted, None)
        .unwrap();
    assert_eq!(
        accepted.context_status,
        ContextStatus::AwaitingReconciliation
    );
    assert_eq!(accepted.source_basis, first.source_basis);
    drop(store);
    let mut store = ProposalStore::open(&path).unwrap();
    assert_eq!(stage(&mut store, &task, 1), accepted);
    assert!(matches!(
        store.review(&first.proposal_id, 0, ProposalDecision::Rejected, None),
        Err(StagingError::StaleReviewRevision { .. })
    ));
    let retained: String = store
        .conn
        .query_row(
            "SELECT immutable_json FROM staged_agent_proposals WHERE proposal_id=?1",
            [&first.proposal_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(raw, retained);
    let references = store.captured_receipt_references("src_registered").unwrap();
    assert_eq!(references.len(), 2);
    assert_eq!(references[0].evidence_id, "source");
    assert!(references.windows(2).all(|p| p[0] < p[1]));
    assert!(
        store
            .captured_receipt_references("another_source")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn prepared_staging_reuses_all_item_replay_and_citation_alias_admission() {
    // AC-0175: uncited evidence and unselected candidate summaries retain the
    // existing replay restriction; aliases still need an injective assignment.
    let task = fixture();
    let mut store = ProposalStore::open(":memory:").unwrap();
    for annotation in [
        &task.task().evidence[1].text,
        &task.task().candidates[0].summary,
    ] {
        let proposal = result(&task, annotation, &["source", "target"]);
        assert!(matches!(
            store.stage_prepared(&task, &proposal, 1, &task.source_basis().graph_snapshot_id),
            Err(StagingError::AnnotationReplaysTaskText)
        ));
    }
    let mut raw = task.task().clone();
    let mut basis = task.source_basis().clone();
    raw.evidence[2].source = raw.evidence[0].source.clone();
    raw.evidence[2].text = raw.evidence[0].text.clone();
    // The target's unverified read can have the same legacy reference; it does
    // not borrow the source's captured association or captured origin.
    basis.selection.captured_validation_bytes = 128;
    let aliases = PreparedAgentTask::new(raw, basis).unwrap();
    let proposal = result(&aliases, "Possible target link", &["source", "target"]);
    assert_eq!(
        proposal.provenance.evidence[0],
        proposal.provenance.evidence[1]
    );
    let mut invalid = proposal.clone();
    invalid.provenance.evidence.pop();
    assert!(
        store
            .stage_prepared(
                &aliases,
                &invalid,
                1,
                &aliases.source_basis().graph_snapshot_id
            )
            .is_err()
    );
    assert!(
        store
            .stage_prepared(
                &aliases,
                &proposal,
                1,
                &aliases.source_basis().graph_snapshot_id
            )
            .is_ok()
    );
}

#[test]
fn v2_staging_decoder_rejects_unknown_nested_fields_nulls_and_tampered_fingerprints() {
    // AC-0175/AC-0178: strict v2 preflight reaches old permissive nested types,
    // and neither envelope fallback nor unbound metadata can disappear on decode.
    let task = fixture();
    let original = StageContentV2::build(
        &task,
        &result(&task, "Possible target link", &["source", "target"]),
        1,
        &task.source_basis().graph_snapshot_id,
    )
    .unwrap();
    let value = serde_json::to_value(&original).unwrap();
    let cases: [fn(&mut serde_json::Value); 15] = [
        |v| v["schema_version"] = json!(3),
        |v| v["schema_version"] = json!(null),
        |v| {
            v.as_object_mut().unwrap().remove("schema_version");
        },
        |v| v["source_basis"] = json!(null),
        |v| v["source_basis"]["schema_version"] = json!(1),
        |v| v["proposal"]["source_text"] = json!("never stored"),
        |v| v["proposal"]["provenance"]["root"] = json!("private"),
        |v| v["proposal"]["provenance"]["evidence"][0]["raw"] = json!("private"),
        |v| v["basis"]["evidence"][0]["source"]["raw"] = json!("private"),
        |v| v["basis"]["candidates"][0]["summary"] = json!("private"),
        |v| {
            v["source_basis"]["selected_facts"][0]
                .as_object_mut()
                .unwrap()
                .remove("binding");
        },
        |v| v["selected_facts_fingerprint"] = json!("0".repeat(64)),
        |v| {
            v["source_basis"]["graph_snapshot_id"] = json!(format!("context-v1:{}", "0".repeat(64)))
        },
        |v| v["basis"]["candidates"][0]["summary_bytes"] = json!(131073),
        |v| v["evidence_binding"] = json!("working_tree_unverified"),
    ];
    for mutate in cases {
        let mut v = value.clone();
        mutate(&mut v);
        assert!(ImmutableContent::decode(&v.to_string()).is_err());
    }
    let mut deep = "null".to_owned();
    for _ in 0..20 {
        deep = format!("[{deep}]");
    }
    assert!(strict::preflight(&format!("{{\"schema_version\":2,\"nested\":{deep}}}")).is_err());
    assert!(strict::preflight("{\"schema_version\":2,\"schema_version\":2}").is_err());
    let mut v = value.clone();
    v["basis"]["evidence"] = json!(vec![value["basis"]["evidence"][0].clone(); 13]);
    assert!(ImmutableContent::decode(&v.to_string()).is_err());
    let mut store = ProposalStore::open(":memory:").unwrap();
    let staged = stage(&mut store, &task, 1);
    let mut public = serde_json::to_value(&staged).unwrap();
    public["source_basis"] = json!(null);
    assert!(serde_json::from_value::<StagedProposal>(public).is_err());
    let mut public = serde_json::to_value(&staged).unwrap();
    public.as_object_mut().unwrap().remove("source_basis");
    assert!(serde_json::from_value::<StagedProposal>(public).is_err());
}

#[test]
fn literal_v1_history_keeps_its_wire_identity_and_mixed_version_cursor() {
    // AC-0178: this historical recipe deliberately does not use either current
    // stage builder. Its field order, v1 prefix and un-domain-prefixed JSON hash
    // are fixed independently, and review/restart must preserve those exact bytes.
    const OLD: &str = concat!(
        "{\"schema_version\":1,\"proposal\":{\"gap_id\":\"gap\",\"source_id\":\"source\",\"target_id\":\"target\",\"edge_label\":\"CALLS\",\"annotation\":\"Legacy rationale\",\"basis_hash\":\"BASIS\",\"provenance\":{\"tier\":\"Agentic\",\"confidence_tier\":\"InferredWeak\",\"evidence\":[{\"repo\":\"r\",\"path\":\"a.ts\",\"byte_start\":0,\"byte_end\":1,\"commit_sha\":\"c\"}],\"extractor_id\":\"t3.agent-broker\",\"content_hash\":\"FACT\"}},",
        "\"job_id\":1,\"graph_snapshot_id\":\"snapshot:old\",\"basis\":{\"schema_version\":1,\"action_id\":\"old\",\"gap_id\":\"gap\",\"source_id\":\"source\",\"edge_label\":\"CALLS\",\"existing_confidence\":\"Gap\",\"source_evidence_ids\":[\"a\"],\"evidence\":[{\"id\":\"a\",\"source\":{\"repo\":\"r\",\"path\":\"a.ts\",\"byte_start\":0,\"byte_end\":1,\"commit_sha\":\"c\"},\"text_hash\":\"TEXT\",\"text_bytes\":1}],\"candidates\":[{\"node_id\":\"target\",\"label\":\"Symbol\",\"summary_hash\":\"SUMMARY\",\"summary_bytes\":1,\"evidence_ids\":[\"a\"]}]},\"evidence_binding\":\"working_tree_unverified\",\"context_status\":\"awaiting_reconciliation\"}"
    );
    let hash = "1".repeat(64);
    let fact =
        content_hash(&serde_json::to_vec(&("gap", "source", "target", "CALLS", &hash)).unwrap());
    let literal = OLD
        .replace("BASIS", &hash)
        .replace("FACT", &fact)
        .replace("TEXT", &"2".repeat(64))
        .replace("SUMMARY", &"3".repeat(64));
    let id = format!("proposal-stage-v1:{}", content_hash(literal.as_bytes()));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.sqlite");
    let mut store = ProposalStore::open(&path).unwrap();
    store
        .conn
        .execute(
            "INSERT INTO staged_agent_proposals(proposal_id,immutable_json) VALUES(?1,?2)",
            params![id, literal],
        )
        .unwrap();
    let legacy = store.get(&id).unwrap().unwrap();
    assert_eq!(
        ImmutableContent::V1(Box::new(immutable_content(&legacy)))
            .json()
            .unwrap(),
        literal
    );
    assert!(legacy.source_basis.is_none());
    assert!(
        !serde_json::to_value(&legacy)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("source_basis")
    );
    assert_eq!(
        serde_json::from_value::<StagedProposal>(serde_json::to_value(&legacy).unwrap()).unwrap(),
        legacy
    );
    stage(&mut store, &fixture(), 2);
    let page = store.list(1, None).unwrap();
    assert_eq!(page.items[0].schema_version, 2);
    let cursor = page.next_cursor.unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&cursor).unwrap()["version"],
        1
    );
    let accepted = store
        .review(
            &id,
            0,
            ProposalDecision::Accepted,
            Some("historical decision"),
        )
        .unwrap();
    drop(store);
    let store = ProposalStore::open(&path).unwrap();
    assert_eq!(store.list(1, Some(&cursor)).unwrap().items, vec![accepted]);
    let persisted: String = store
        .conn
        .query_row(
            "SELECT immutable_json FROM staged_agent_proposals WHERE proposal_id=?1",
            [&id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(persisted, literal);
    assert_eq!(
        store
            .captured_receipt_references("src_registered")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn prepared_staging_retention_scan_and_sql_payloads_fail_closed_at_bounds() {
    // AC-0178: retention previews cannot silently omit a corrupt row or an
    // unscanned tail. SQLite type/byte checks precede owned string allocation.
    let mut store = ProposalStore::open(":memory:").unwrap();
    stage(&mut store, &fixture(), 1);
    store
        .conn
        .execute_batch(
            "DROP TRIGGER staged_agent_proposals_immutable; PRAGMA ignore_check_constraints=ON;",
        )
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE staged_agent_proposals SET immutable_json=?1",
            ["é".repeat(MAX_STAGED_RECORD_BYTES / 2 + 1)],
        )
        .unwrap();
    assert!(store.list(1, None).is_err());
    assert!(store.captured_receipt_references("src_registered").is_err());
    store
        .conn
        .execute("DELETE FROM staged_agent_proposals", [])
        .unwrap();
    store.conn.execute_batch("WITH RECURSIVE n(v) AS (SELECT 1 UNION ALL SELECT v+1 FROM n WHERE v<4097) INSERT INTO staged_agent_proposals(proposal_id,immutable_json) SELECT 'fake:'||v,'{}' FROM n;").unwrap();
    assert!(matches!(
        store.captured_receipt_references("src_registered"),
        Err(StagingError::ReferenceInventoryLimit)
    ));
}

#[test]
fn prepared_staging_reserves_no_note_review_headroom() {
    // AC-0175/AC-0178: the v2 metadata envelope obeys the same complete-record
    // bound and transactional no-note review admission as historical stages.
    let task = fixture();
    let mut store = ProposalStore::open(":memory:").unwrap();
    let baseline = stage(&mut store, &task, 1);
    let size = serde_json::to_vec(&baseline).unwrap().len();
    let budget = MAX_STAGED_RECORD_BYTES - size + baseline.proposal.annotation.len();
    let too_close = result(&task, &"x".repeat(budget), &["source", "target"]);
    assert!(matches!(
        store.stage_prepared(&task, &too_close, 1, &task.source_basis().graph_snapshot_id),
        Err(StagingError::RecordTooLarge { .. })
    ));
    let mut reviewed = baseline.clone();
    reviewed.review_revision = i64::MAX as u64;
    reviewed.review_decision = Some(ProposalDecision::Rejected);
    reviewed.reviewed_at = Some("9999-12-31T23:59:59Z".into());
    let reserve = serde_json::to_vec(&reviewed).unwrap().len() - size;
    let admitted = result(&task, &"x".repeat(budget - reserve), &["source", "target"]);
    let staged = store
        .stage_prepared(&task, &admitted, 1, &task.source_basis().graph_snapshot_id)
        .unwrap();
    let accepted = store
        .review(&staged.proposal_id, 0, ProposalDecision::Accepted, None)
        .unwrap();
    assert!(serde_json::to_vec(&accepted).unwrap().len() <= MAX_STAGED_RECORD_BYTES);
}
