use super::*;
use crate::{AgentCandidate, AgentEvidence, DecisionLog};
use core_prov::Tier;

fn evidence(id: &str, path: &str, text: &str) -> AgentEvidence {
    AgentEvidence {
        id: id.into(),
        source: EvidenceRef {
            repo: "acme/cart".into(),
            path: path.into(),
            byte_start: 0,
            byte_end: text.len() as u64,
            commit_sha: "a".repeat(40),
        },
        text: text.into(),
    }
}

fn task() -> AgentTask {
    AgentTask {
        action_id: "escalate:cart".into(),
        gap_id: "gap:cart-call".into(),
        source_id: "symbol:cart".into(),
        edge_label: "CALLS".into(),
        existing_confidence: ConfidenceTier::Gap,
        source_evidence_ids: vec!["source".into()],
        evidence: vec![
            evidence("source", "cart.ts", "raw-source-marker-73194"),
            evidence("target", "validate.ts", "raw-target-marker-73194"),
            evidence("extra", "config.ts", "raw-extra-marker-73194"),
        ],
        candidates: vec![AgentCandidate {
            node_id: "symbol:validate".into(),
            label: "Symbol".into(),
            summary: "raw-summary-marker-73194".into(),
            evidence_ids: vec!["target".into()],
        }],
    }
}

fn proposal(task: &AgentTask, annotation: &str, citations: &[&str]) -> AgentProposal {
    let broker = AgentBroker::bounded_default();
    broker.validate_task(task).unwrap();
    broker
        .validate_response(
            task,
            RawProposal {
                target_id: task.candidates[0].node_id.clone(),
                annotation: annotation.into(),
                citations: citations.iter().map(|id| (*id).into()).collect(),
            },
        )
        .unwrap()
}

fn standard_proposal(task: &AgentTask) -> AgentProposal {
    proposal(task, "Possible call target", &["source", "target"])
}

fn store() -> ProposalStore {
    ProposalStore::open(":memory:").unwrap()
}

#[test]
fn staging_binds_complete_content_and_omits_raw_task_source() {
    // AC-0126: a legacy edge hash cannot identify the exact reviewed content.
    let task = task();
    let original = standard_proposal(&task);
    let changed_annotation = proposal(&task, "A different rationale", &["source", "target"]);
    let changed_citations = proposal(
        &task,
        "Possible call target",
        &["source", "target", "extra"],
    );
    assert_eq!(
        original.provenance.content_hash,
        changed_annotation.provenance.content_hash
    );
    assert_eq!(
        original.provenance.content_hash,
        changed_citations.provenance.content_hash
    );
    let mut store = store();
    let staged = store.stage(&task, &original, 1, "snapshot:one").unwrap();
    assert_eq!(
        staged,
        store.stage(&task, &original, 1, "snapshot:one").unwrap()
    );
    let annotation = store
        .stage(&task, &changed_annotation, 1, "snapshot:one")
        .unwrap();
    let citations = store
        .stage(&task, &changed_citations, 1, "snapshot:one")
        .unwrap();
    let job = store.stage(&task, &original, 2, "snapshot:one").unwrap();
    let snapshot = store.stage(&task, &original, 1, "snapshot:two").unwrap();
    let identities: BTreeSet<_> = [&staged, &annotation, &citations, &job, &snapshot]
        .into_iter()
        .map(|item| &item.proposal_id)
        .collect();
    assert_eq!(identities.len(), 5);
    assert_eq!(store.list(50, None).unwrap().items.len(), 5);
    assert_eq!(staged.basis.evidence.len(), task.evidence.len());
    for (supplied, fingerprint) in task.evidence.iter().zip(&staged.basis.evidence) {
        assert_eq!(supplied.source, fingerprint.source);
        assert_eq!(
            content_hash(supplied.text.as_bytes()),
            fingerprint.text_hash
        );
    }
    assert_eq!(
        staged.basis.candidates[0].summary_hash,
        content_hash(task.candidates[0].summary.as_bytes())
    );
    let durable: String = store
        .conn
        .query_row(
            "SELECT immutable_json FROM staged_agent_proposals WHERE proposal_id = ?1",
            params![staged.proposal_id],
            |row| row.get(0),
        )
        .unwrap();
    let wire = serde_json::to_string(&staged).unwrap();
    for omitted in task
        .evidence
        .iter()
        .map(|item| &item.text)
        .chain(task.candidates.iter().map(|item| &item.summary))
    {
        assert!(!durable.contains(omitted));
        assert!(!wire.contains(omitted));
    }
    let wire: serde_json::Value = serde_json::from_str(&wire).unwrap();
    assert_eq!(wire["gap_id"], original.gap_id);
    assert!(
        wire.get("proposal").is_none(),
        "broker fields stay flat on the wire"
    );
}

#[test]
fn staging_rejects_task_text_replay_before_durable_writes() {
    // AC-0126: all original task material is checked, not only cited/selected
    // material. Rejection must not archive canaries in a row, database or WAL.
    let mut task = task();
    task.candidates.push(AgentCandidate {
        node_id: "symbol:unselected".into(),
        label: "Symbol".into(),
        summary: "raw-unselected-summary-marker-85379".into(),
        evidence_ids: vec!["extra".into()],
    });
    let omitted: Vec<_> = task
        .evidence
        .iter()
        .map(|item| item.text.clone())
        .chain(task.candidates.iter().map(|item| item.summary.clone()))
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("proposals.sqlite");
    let mut store = ProposalStore::open(&path).unwrap();
    let valid = standard_proposal(&task);
    let baseline = store.stage(&task, &valid, 1, "snapshot:one").unwrap();
    for text in &omitted {
        for annotation in [text.clone(), format!("Rationale: `{text}`; see citations.")] {
            // The broker accepts this output; durable admission must reject it.
            let replay = proposal(&task, &annotation, &["source", "target"]);
            let changes = store.conn.total_changes();
            let error = store.stage(&task, &replay, 2, "snapshot:one").unwrap_err();
            assert!(matches!(error, StagingError::AnnotationReplaysTaskText));
            assert_eq!(
                error.to_string(),
                "proposal annotation replays supplied task text"
            );
            assert_eq!(format!("{error:?}"), "AnnotationReplaysTaskText");
            assert_eq!(store.conn.total_changes(), changes);
            assert_eq!(store.list(50, None).unwrap().items, vec![baseline.clone()]);
        }
    }
    let check_files = || {
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let bytes = std::fs::read(entry.unwrap().path()).unwrap();
            for text in &omitted {
                assert!(
                    !bytes
                        .windows(text.len())
                        .any(|part| part == text.as_bytes())
                );
            }
        }
    };
    check_files(); // Includes the live WAL, before checkpoint/connection close.
    drop(store);
    let store = ProposalStore::open(&path).unwrap();
    assert_eq!(store.list(50, None).unwrap().items, vec![baseline]);
    check_files();
}

#[test]
fn staging_replay_guard_handles_normalization_excerpts_and_short_items() {
    // AC-0126: whitespace changes cannot replay full items or 48-scalar excerpts.
    // Short complete overlaps intentionally reject; case/encoding inference is
    // outside this deterministic admission contract.
    let mut task = task();
    task.evidence[2] = evidence("extra", "config.ts", "if (ready) {\r\n  execute();\n}");
    let replay = proposal(
        &task,
        "Observed: if(ready){execute();}",
        &["source", "target"],
    );
    assert!(matches!(
        store().stage(&task, &replay, 1, "snapshot:one"),
        Err(StagingError::AnnotationReplaysTaskText)
    ));

    let excerpt: String = (0..48)
        .map(|index| char::from_u32(0x4e00 + index).unwrap())
        .collect();
    task.evidence[2] = evidence("extra", "config.ts", &format!("before{excerpt}after"));
    let spaced: String = excerpt
        .chars()
        .map(|character| format!("{character}\u{2003}"))
        .collect();
    let replay = proposal(&task, &format!("Excerpt: {spaced}"), &["source", "target"]);
    assert!(matches!(
        store().stage(&task, &replay, 1, "snapshot:one"),
        Err(StagingError::AnnotationReplaysTaskText)
    ));
    let shorter: String = excerpt.chars().take(47).collect();
    let bounded = proposal(&task, &format!("Excerpt: {shorter}"), &["source", "target"]);
    assert!(store().stage(&task, &bounded, 1, "snapshot:one").is_ok());

    task.candidates[0].summary = "a".into();
    let common = standard_proposal(&task);
    assert!(matches!(
        store().stage(&task, &common, 1, "snapshot:one"),
        Err(StagingError::AnnotationReplaysTaskText)
    ));
    task.candidates[0].summary = "\r\n\u{2003}".into();
    task.evidence[2] = evidence("extra", "config.ts", "\t ");
    let benign = standard_proposal(&task);
    assert!(store().stage(&task, &benign, 1, "snapshot:one").is_ok());
}

#[test]
fn staging_replay_inputs_are_bounded_and_accepted_text_is_unchanged() {
    // AC-0126: validate resource bounds before basis hashing/normalization, and
    // leave accepted annotations and post-review idempotence byte-for-byte intact.
    let mut task = task();
    let original = standard_proposal(&task);
    let mut huge_annotation = original.clone();
    huge_annotation.annotation = "x".repeat(MAX_STAGED_RECORD_BYTES + 1);
    let mut store = store();
    assert!(matches!(
        store.stage(&task, &huge_annotation, 1, "snapshot:one"),
        Err(StagingError::RecordTooLarge { .. })
    ));
    let mut large_task = task.clone();
    large_task.candidates[0].summary = "x".repeat(MAX_STAGED_RECORD_BYTES / 2 + 1);
    let mut second = large_task.candidates[0].clone();
    second.node_id = "symbol:second".into();
    large_task.candidates.push(second);
    assert!(matches!(
        store.stage(&large_task, &original, 1, "snapshot:one"),
        Err(StagingError::TaskSummariesTooLarge)
    ));
    assert!(store.list(50, None).unwrap().items.is_empty());

    task.candidates[0].summary = "x".repeat(MAX_STAGED_RECORD_BYTES);
    let accepted_text = "Possible  call\n target — see citations.";
    let valid = proposal(&task, accepted_text, &["source", "target"]);
    let staged = store.stage(&task, &valid, 1, "snapshot:one").unwrap();
    assert_eq!(staged.proposal.annotation, accepted_text);
    let reviewed = store
        .review(&staged.proposal_id, 0, ProposalDecision::Accepted, None)
        .unwrap();
    assert_eq!(
        reviewed,
        store.stage(&task, &valid, 1, "snapshot:one").unwrap()
    );
    assert_eq!(reviewed.proposal, valid);
}

#[test]
fn staging_rejects_results_outside_original_task_contract() {
    // AC-0126: validation proves candidate, citation and provenance membership
    // against the original host task, not a caller's self-consistent fact hash.
    let task = task();
    let original = standard_proposal(&task);
    let mut forgeries = Vec::new();
    let mut forged = original.clone();
    forged.target_id = "symbol:invented".into();
    forgeries.push(forged);
    let mut forged = original.clone();
    forged.source_id = "symbol:other".into();
    forgeries.push(forged);
    let mut forged = original.clone();
    forged.edge_label = "PUBLISHES".into();
    forgeries.push(forged);
    let mut forged = original.clone();
    forged.provenance.evidence[0].path = "uncited.ts".into();
    forgeries.push(forged);
    let mut forged = original.clone();
    forged.provenance.evidence.remove(0);
    forgeries.push(forged);
    let mut forged = original.clone();
    forged.provenance.evidence.pop();
    forgeries.push(forged);
    let mut forged = original.clone();
    forged
        .provenance
        .evidence
        .push(task.evidence[0].source.clone());
    forgeries.push(forged);
    let mut forged = original.clone();
    forged.provenance.confidence_tier = ConfidenceTier::Confirmed;
    forgeries.push(forged);
    let mut forged = original.clone();
    forged.provenance.tier = Tier::Deterministic;
    forgeries.push(forged);
    let mut forged = original.clone();
    forged.provenance.extractor_id = "invented".into();
    forgeries.push(forged);
    let mut forged = original.clone();
    forged.basis_hash = content_hash(b"invented basis");
    forgeries.push(forged);
    let mut store = store();
    for (index, mut forged) in forgeries.into_iter().enumerate() {
        // Even recomputing the legacy hash cannot make altered content belong
        // to the host task or acquire a different producing provenance.
        forged.provenance.content_hash = content_hash(
            &serde_json::to_vec(&(
                &forged.gap_id,
                &forged.source_id,
                &forged.target_id,
                &forged.edge_label,
                &forged.basis_hash,
            ))
            .unwrap(),
        );
        assert!(
            store.stage(&task, &forged, 1, "snapshot:one").is_err(),
            "forgery {index}"
        );
    }
    let mut changed_task = task.clone();
    changed_task.evidence[0]
        .text
        .push_str(" different host bytes");
    assert!(
        store
            .stage(&changed_task, &original, 1, "snapshot:one")
            .is_err()
    );
    let mut unbounded = task.clone();
    unbounded.evidence[0].text = "x".repeat(BrokerLimits::default().max_span_bytes + 1);
    assert!(
        store
            .stage(&unbounded, &original, 1, "snapshot:one")
            .is_err()
    );
    assert!(store.stage(&task, &original, 0, "snapshot:one").is_err());
    assert!(store.stage(&task, &original, 1, "").is_err());
    assert!(store.list(50, None).unwrap().items.is_empty());
}

#[test]
fn citation_aliases_cannot_satisfy_disjoint_memberships_with_one_reference() {
    // AC-0126: distinct citation IDs can share a source span. Their multiplicity
    // and required source/target memberships still need a valid broker assignment.
    let mut task = task();
    task.evidence[1].source = task.evidence[0].source.clone();
    task.evidence[1].text = task.evidence[0].text.clone();
    let valid = standard_proposal(&task);
    let mut store = store();
    store.stage(&task, &valid, 1, "snapshot:one").unwrap();
    let mut incomplete = valid.clone();
    incomplete.provenance.evidence.pop();
    assert!(store.stage(&task, &incomplete, 1, "snapshot:one").is_err());

    // A genuinely shared membership can cite its single ID once, matching the
    // broker contract; the manifest must not invent two distinct citations.
    task.candidates[0].evidence_ids = vec!["source".into()];
    let shared = proposal(&task, "Shared evidence", &["source"]);
    store.stage(&task, &shared, 1, "snapshot:one").unwrap();
}

#[test]
fn pending_and_reviewed_stages_survive_restart_with_id_only_cas() {
    // AC-0127: restart restores pending work, and independent reviewers cannot
    // overwrite each other's decision or replace the underlying staged body.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("proposals.sqlite");
    let task = task();
    let proposal = standard_proposal(&task);
    let staged = {
        let mut store = ProposalStore::open(&path).unwrap();
        store.stage(&task, &proposal, 1, "snapshot:one").unwrap()
    };
    let mut first = ProposalStore::open(&path).unwrap();
    let mut second = ProposalStore::open(&path).unwrap();
    assert_eq!(
        first.get(&staged.proposal_id).unwrap(),
        Some(staged.clone())
    );
    assert_eq!(
        second
            .get(&staged.proposal_id)
            .unwrap()
            .unwrap()
            .review_revision,
        0
    );
    let reviewed = first
        .review(
            &staged.proposal_id,
            0,
            ProposalDecision::Accepted,
            Some("Reviewed rationale"),
        )
        .unwrap();
    assert_eq!(reviewed.review_revision, 1);
    assert_eq!(reviewed.review_decision, Some(ProposalDecision::Accepted));
    assert_eq!(reviewed.proposal, staged.proposal);
    assert_eq!(reviewed.proposal_id, staged.proposal_id);
    assert!(matches!(
        second.review(&staged.proposal_id, 0, ProposalDecision::Rejected, None),
        Err(StagingError::StaleReviewRevision {
            expected: 0,
            actual: 1
        })
    ));
    assert!(matches!(
        first.review("not-a-stage", 0, ProposalDecision::Accepted, None),
        Err(StagingError::UnknownProposal(_))
    ));
    assert_eq!(
        first.stage(&task, &proposal, 1, "snapshot:one").unwrap(),
        reviewed
    );
    drop(first);
    drop(second);
    let mut reopened = ProposalStore::open(&path).unwrap();
    assert_eq!(reopened.get(&staged.proposal_id).unwrap(), Some(reviewed));
    let rejected = reopened
        .review(
            &staged.proposal_id,
            1,
            ProposalDecision::Rejected,
            Some("New review"),
        )
        .unwrap();
    assert_eq!(rejected.review_revision, 2);
    assert_eq!(rejected.review_decision, Some(ProposalDecision::Rejected));
    assert_eq!(immutable_content(&rejected), immutable_content(&staged));
}

#[test]
fn staged_history_is_bounded_deterministic_and_resumable_after_restart() {
    // AC-0127: the durable queue includes pending and reviewed records, with
    // stable keyset ordering and explicit continuation independent of timestamps.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("proposals.sqlite");
    let task = task();
    let proposal = standard_proposal(&task);
    let mut store = ProposalStore::open(&path).unwrap();
    let one = store.stage(&task, &proposal, 1, "snapshot:one").unwrap();
    let two = store.stage(&task, &proposal, 2, "snapshot:one").unwrap();
    let three = store.stage(&task, &proposal, 3, "snapshot:one").unwrap();
    store
        .review(&two.proposal_id, 0, ProposalDecision::Accepted, None)
        .unwrap();
    let first = store.list(2, None).unwrap();
    assert_eq!(
        first
            .items
            .iter()
            .map(|item| &item.proposal_id)
            .collect::<Vec<_>>(),
        vec![&three.proposal_id, &two.proposal_id]
    );
    assert_eq!(first.items[0].review_decision, None);
    assert_eq!(
        first.items[1].review_decision,
        Some(ProposalDecision::Accepted)
    );
    let four = store.stage(&task, &proposal, 4, "snapshot:one").unwrap();
    store
        .review(&one.proposal_id, 0, ProposalDecision::Rejected, None)
        .unwrap();
    drop(store);
    let store = ProposalStore::open(&path).unwrap();
    let second = store.list(2, first.next_cursor.as_deref()).unwrap();
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].proposal_id, one.proposal_id);
    assert_eq!(
        second.items[0].review_decision,
        Some(ProposalDecision::Rejected)
    );
    assert!(second.next_cursor.is_none());
    assert_eq!(
        store.list(1, None).unwrap().items[0].proposal_id,
        four.proposal_id
    );
    assert!(matches!(
        store.list(0, None),
        Err(StagingError::InvalidLimit)
    ));
    assert!(matches!(
        store.list(MAX_STAGED_PAGE_ITEMS + 1, None),
        Err(StagingError::InvalidLimit)
    ));
    for cursor in [
        "not-json",
        r#"{"version":2,"ceiling":4,"before":3}"#,
        r#"{"version":1,"ceiling":4,"before":5}"#,
    ] {
        assert!(matches!(
            store.list(2, Some(cursor)),
            Err(StagingError::InvalidCursor)
        ));
    }
}

#[test]
fn oversized_payloads_and_storage_failures_do_not_commit_stages_or_reviews() {
    // AC-0126/AC-0127: the complete record is bounded and failure never returns
    // a successful staged body or leaves a partially applied review behind.
    let task = task();
    let original = standard_proposal(&task);
    let mut store = store();
    let staged = store.stage(&task, &original, 1, "snapshot:one").unwrap();
    let huge = proposal(
        &task,
        &"x".repeat(MAX_STAGED_RECORD_BYTES),
        &["source", "target"],
    );
    assert!(matches!(
        store.stage(&task, &huge, 2, "snapshot:one"),
        Err(StagingError::RecordTooLarge { .. })
    ));
    assert!(matches!(
        store.review(
            &staged.proposal_id,
            0,
            ProposalDecision::Accepted,
            Some(&"x".repeat(MAX_STAGED_RECORD_BYTES))
        ),
        Err(StagingError::RecordTooLarge { .. })
    ));
    assert_eq!(
        store.get(&staged.proposal_id).unwrap(),
        Some(staged.clone())
    );
    store
        .conn
        .execute_batch(
            "CREATE TRIGGER reject_stage BEFORE INSERT ON staged_agent_proposals
         BEGIN SELECT RAISE(ABORT, 'simulated staging storage failure'); END;",
        )
        .unwrap();
    assert!(matches!(
        store.stage(&task, &original, 2, "snapshot:one"),
        Err(StagingError::Storage(_))
    ));
    assert_eq!(store.list(50, None).unwrap().items, vec![staged.clone()]);
    store.conn.execute_batch(
        "CREATE TRIGGER reject_review BEFORE UPDATE OF review_revision ON staged_agent_proposals
         BEGIN SELECT RAISE(ABORT, 'simulated review storage failure'); END;",
    ).unwrap();
    assert!(matches!(
        store.review(&staged.proposal_id, 0, ProposalDecision::Accepted, None),
        Err(StagingError::Storage(_))
    ));
    assert_eq!(store.get(&staged.proposal_id).unwrap(), Some(staged));
}

#[test]
fn staging_reserves_enough_record_budget_for_a_later_no_note_review() {
    // AC-0126/AC-0127: a pending DTO that fits by itself must not commit when
    // ordinary review metadata would make it permanently unreviewable.
    let task = task();
    let original = standard_proposal(&task);
    let mut store = store();
    let baseline = store.stage(&task, &original, 1, "snapshot:one").unwrap();
    let baseline_bytes = serde_json::to_vec(&baseline).unwrap().len();
    let annotation_budget = MAX_STAGED_RECORD_BYTES - baseline_bytes + original.annotation.len();
    let pending_only = proposal(&task, &"x".repeat(annotation_budget), &["source", "target"]);
    let pending_only_id = StageContent::build(&task, &pending_only, 1, "snapshot:one")
        .unwrap()
        .identity()
        .unwrap();
    assert!(matches!(
        store.stage(&task, &pending_only, 1, "snapshot:one"),
        Err(StagingError::RecordTooLarge { .. })
    ));
    assert!(store.get(&pending_only_id).unwrap().is_none());
    assert_eq!(store.list(50, None).unwrap().items, vec![baseline.clone()]);

    // The boundary with enough space for a 19-digit revision and UTC review
    // timestamp remains stageable and can then be accepted or rejected.
    let mut largest_review = baseline;
    largest_review.review_revision = i64::MAX as u64;
    largest_review.review_decision = Some(ProposalDecision::Rejected);
    largest_review.reviewed_at = Some("9999-12-31T23:59:59Z".into());
    let headroom = serde_json::to_vec(&largest_review).unwrap().len() - baseline_bytes;
    let reviewable = proposal(
        &task,
        &"x".repeat(annotation_budget - headroom),
        &["source", "target"],
    );
    let staged = store.stage(&task, &reviewable, 1, "snapshot:one").unwrap();
    let accepted = store
        .review(&staged.proposal_id, 0, ProposalDecision::Accepted, None)
        .unwrap();
    assert!(serde_json::to_vec(&accepted).unwrap().len() <= MAX_STAGED_RECORD_BYTES);
    assert_eq!(accepted.review_decision, Some(ProposalDecision::Accepted));
    assert_eq!(accepted.proposal_id, staged.proposal_id);
}

#[test]
fn acceptance_and_commit_shaped_citations_never_certify_freshness() {
    // AC-0130/AC-0131: exact fingerprints and snapshot identity bind supplied
    // material, while acceptance retains unverified evidence and pending context.
    let original_task = task();
    let original_proposal = standard_proposal(&original_task);
    let mut store = store();
    let staged = store
        .stage(&original_task, &original_proposal, 1, "snapshot:unchanged")
        .unwrap();
    let accepted = store
        .review(&staged.proposal_id, 0, ProposalDecision::Accepted, None)
        .unwrap();
    assert_eq!(
        accepted.evidence_binding,
        EvidenceBinding::WorkingTreeUnverified
    );
    assert_eq!(
        accepted.context_status,
        ContextStatus::AwaitingReconciliation
    );
    assert_eq!(accepted.proposal.provenance.tier, Tier::Agentic);
    assert_eq!(
        accepted.proposal.provenance.confidence_tier,
        ConfidenceTier::InferredWeak
    );
    assert_eq!(accepted.proposal, original_proposal);
    let wire = serde_json::to_value(&accepted).unwrap();
    assert_eq!(wire["evidence_binding"], "working_tree_unverified");
    assert_eq!(wire["context_status"], "awaiting_reconciliation");
    let mut changed_task = original_task.clone();
    changed_task.evidence[0].text = "different supplied bytes".into();
    let changed_proposal = standard_proposal(&changed_task);
    let changed = store
        .stage(&changed_task, &changed_proposal, 1, "snapshot:unchanged")
        .unwrap();
    assert_ne!(
        changed.basis.evidence[0].text_hash,
        accepted.basis.evidence[0].text_hash
    );
    assert_ne!(changed.proposal_id, accepted.proposal_id);
    assert_eq!(changed.review_revision, 0);
    assert_eq!(
        changed.evidence_binding,
        EvidenceBinding::WorkingTreeUnverified
    );
    assert_eq!(
        changed.context_status,
        ContextStatus::AwaitingReconciliation
    );
}

#[test]
fn legacy_decisions_remain_historical_and_job_cleanup_does_not_delete_stages() {
    // AC-0131: legacy caller-body decisions cannot become stages. The staging
    // store has no graph mutation API and no lifetime dependency on jobs.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let task = task();
    let proposal = standard_proposal(&task);
    let mut legacy = DecisionLog::open(&path).unwrap();
    let historical = legacy
        .record(&proposal, ProposalDecision::Accepted, Some("Legacy review"))
        .unwrap();
    let mut store = ProposalStore::open(&path).unwrap();
    assert!(store.list(50, None).unwrap().items.is_empty());
    assert!(matches!(
        store.review(
            &proposal.provenance.content_hash,
            0,
            ProposalDecision::Accepted,
            None
        ),
        Err(StagingError::UnknownProposal(_))
    ));
    store
        .conn
        .execute_batch("CREATE TABLE jobs (id INTEGER PRIMARY KEY); INSERT INTO jobs VALUES (1);")
        .unwrap();
    let staged = store.stage(&task, &proposal, 1, "snapshot:one").unwrap();
    store
        .review(&staged.proposal_id, 0, ProposalDecision::Rejected, None)
        .unwrap();
    store.conn.execute_batch("DELETE FROM jobs;").unwrap();
    drop(store);
    let store = ProposalStore::open(&path).unwrap();
    assert_eq!(
        store
            .get(&staged.proposal_id)
            .unwrap()
            .unwrap()
            .review_decision,
        Some(ProposalDecision::Rejected)
    );
    assert_eq!(legacy.list().unwrap(), vec![historical]);
}

#[test]
fn immutable_storage_and_version_checks_reject_replaced_review_material() {
    // AC-0126: persisted complete-content identity and supported schema are
    // revalidated before reads/reviews; corrupt content cannot be approved.
    let task = task();
    let proposal = standard_proposal(&task);
    let mut store = store();
    let staged = store.stage(&task, &proposal, 1, "snapshot:one").unwrap();
    assert!(
        store
            .conn
            .execute(
                "UPDATE staged_agent_proposals SET immutable_json = '{}' WHERE proposal_id = ?1",
                params![staged.proposal_id],
            )
            .is_err()
    );
    let mut content = immutable_content(&staged);
    content.schema_version = 2;
    store
        .conn
        .execute_batch("DROP TRIGGER staged_agent_proposals_immutable")
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE staged_agent_proposals SET immutable_json = ?1 WHERE proposal_id = ?2",
            params![serde_json::to_string(&content).unwrap(), staged.proposal_id],
        )
        .unwrap();
    assert!(matches!(
        store.get(&staged.proposal_id),
        Err(StagingError::InvalidRecord)
    ));
    content.schema_version = STAGING_SCHEMA_VERSION;
    content.proposal.annotation = "Changed after staging".into();
    store
        .conn
        .execute(
            "UPDATE staged_agent_proposals SET immutable_json = ?1 WHERE proposal_id = ?2",
            params![serde_json::to_string(&content).unwrap(), staged.proposal_id],
        )
        .unwrap();
    assert!(matches!(
        store.review(&staged.proposal_id, 0, ProposalDecision::Accepted, None),
        Err(StagingError::InvalidRecord)
    ));
}
