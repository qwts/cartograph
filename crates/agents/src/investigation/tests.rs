use super::*;
use crate::{TaskFactKey, TaskFactSelection};
use core_prov::{ConfidenceTier, Tier, content_hash};
use serde_json::json;
use std::collections::BTreeSet;

fn request() -> StartInvestigationRequest {
    StartInvestigationRequest {
        schema_version: 1,
        request_nonce: "request-1".into(),
        specialist_id: SpecialistId::DomainAnalyst,
        question: "What governs checkout?".into(),
        scope: InvestigationScope::All,
        provider_mode: InvestigationProviderMode::Local,
        limit_profile: "investigation-v1".into(),
        expected_graph_revision: None,
        conversation_id: None,
        parent_id: None,
    }
}
fn query() -> InvestigationQuery {
    InvestigationQuery {
        scope: InvestigationScope::All,
        kind: None,
        labels: vec![],
        max_facts: 12,
        max_bytes: 16384,
        cursor: None,
    }
}
fn action() -> InvestigationAction {
    InvestigationAction::Finish {
        findings: vec![ProposedInvestigationFinding {
            claim_kind: InvestigationClaimKind::ImplementedBehavior,
            title: "Checkout guard".into(),
            statement: "The transition has a rejection path.".into(),
            citation_ids: vec!["fact-1".into()],
            limitations: vec!["Other routes were not assessed.".into()],
        }],
        knowledge_completeness: KnowledgeCompleteness::Partial,
        limitations: vec!["The investigation covers selected evidence only.".into()],
    }
}
fn ledger() -> InvestigationInputLedger {
    let fact = TaskFactKey::Node {
        id: "node:checkout".into(),
    };
    let digest = format!("node-v1:{}", "a".repeat(64));
    InvestigationInputLedger {
        schema_version: 1,
        graph_snapshot_id: format!("context-v1:{}", "b".repeat(64)),
        scope_snapshot_id: format!("context-v1:{}", "b".repeat(64)),
        revision: 1,
        selected_facts: vec![TaskFactSelection {
            fact: fact.clone(),
            fact_digest: digest.clone(),
            binding: None,
        }],
        receipt_references: vec![],
        citations: vec![InvestigationCitation {
            citation_id: "fact-1".into(),
            fact,
            fact_digest: digest,
            source: None,
            role: None,
            index: None,
            text_hash: None,
            origin: InvestigationEvidenceOrigin::GraphMetadata,
        }],
        queries: vec![InvestigationQueryManifest {
            query: query(),
            response_hash: content_hash(b"test-query-result"),
            response_bytes: 200,
            returned_facts: 1,
            total_selected: 1,
            has_more: false,
        }],
        supplied_history_hash: None,
        supplied_history_bytes: 0,
    }
}

#[test]
fn investigation_specialists_are_versioned_distinct_and_propose_only() {
    // AC-0180: changing specialist identity changes the exact immutable prompt basis.
    let analyst = SpecialistId::DomainAnalyst.definition().unwrap();
    let auditor = SpecialistId::EvidenceAuditor.definition().unwrap();
    assert_ne!(analyst.prompt_fingerprint, auditor.prompt_fingerprint);
    assert_eq!(analyst, SpecialistId::DomainAnalyst.definition().unwrap());
    for specialist in [analyst, auditor] {
        assert_eq!(specialist.tier, Tier::Agentic);
        assert_eq!(specialist.confidence_tier, ConfidenceTier::InferredWeak);
        assert_eq!(
            specialist.operations,
            ["query_context", "read_evidence", "finish"]
        );
        assert_eq!(specialist.version, 2);
    }
}

#[test]
fn investigation_discovery_revision_preserves_original_specialist_identity() {
    // AC-0180: the first real-provider artifact must remain readable after a
    // prompt improvement; old IDs must not acquire a different fingerprint.
    let old = SpecialistId::DomainAnalystV1.definition().unwrap();
    assert_eq!(old.version, 1);
    assert_eq!(
        old.prompt_fingerprint,
        "ab952c9fe055e1fde7aa5ca3f1d7f60851b402021460fdbd9011a765e20967c3"
    );
    for (legacy, current, old_id, new_id) in [
        (
            SpecialistId::DomainAnalystV1,
            SpecialistId::DomainAnalyst,
            "domain-analyst@1",
            "domain-analyst@2",
        ),
        (
            SpecialistId::EvidenceAuditorV1,
            SpecialistId::EvidenceAuditor,
            "evidence-auditor@1",
            "evidence-auditor@2",
        ),
    ] {
        assert_eq!(serde_json::to_value(legacy).unwrap(), old_id);
        assert_eq!(serde_json::to_value(current).unwrap(), new_id);
        let restored: SpecialistId = serde_json::from_value(serde_json::json!(old_id)).unwrap();
        assert_eq!(restored.definition().unwrap(), legacy.definition().unwrap());
        assert_ne!(
            legacy.definition().unwrap().prompt_fingerprint,
            current.definition().unwrap().prompt_fingerprint
        );
        assert!(
            !legacy
                .prompt()
                .contains("Empty initial input is not an empty search result")
        );
        assert!(
            current
                .prompt()
                .contains("Empty initial input is not an empty search result")
        );
    }
}

#[test]
fn investigation_intent_identity_binds_original_text_before_redaction() {
    // AC-0181/0188: two secrets that redact equally must not deduplicate as one request.
    let mut first = request();
    first.question = "Explain password=first-synthetic-value".into();
    let mut second = first.clone();
    second.question = "Explain password=second-synthetic-value".into();
    assert_eq!(
        redacted_history_text(&first.question, 2048).unwrap(),
        redacted_history_text(&second.question, 2048).unwrap()
    );
    assert_ne!(first.intent_hash().unwrap(), second.intent_hash().unwrap());
    assert_eq!(
        first.intent_hash().unwrap(),
        first.clone().intent_hash().unwrap()
    );
    second.scope = InvestigationScope::Neighborhood {
        anchor: "node:checkout".into(),
        hops: 0,
    };
    assert!(second.validate().is_err());
    second = request();
    second.parent_id = Some("unlinked-parent".into());
    assert!(second.validate().is_err());
}

#[test]
fn investigation_action_decoder_is_closed_and_never_extracts_or_repairs() {
    // AC-0184: untrusted objects never acquire an arbitrary tool or authority field.
    let valid = serde_json::to_string(&action()).unwrap();
    assert_eq!(InvestigationAction::decode(&valid).unwrap(), action());
    for raw in [format!("```json\n{valid}\n```"), format!("{valid} {{}}"),
        "{\"type\":\"run_command\",\"command\":\"anything\"}".into(),
        "{\"type\":\"finish\",\"type\":\"finish\",\"findings\":[],\"knowledge_completeness\":\"insufficient_evidence\",\"limitations\":[\"Unknown\"]}".into(),
    ] { assert!(InvestigationAction::decode(&raw).is_err()); }
    let mut value = serde_json::to_value(action()).unwrap();
    value["findings"][0]["tier"] = json!("Deterministic");
    assert!(InvestigationAction::decode(&value.to_string()).is_err());
    value = serde_json::to_value(InvestigationAction::QueryContext { query: query() }).unwrap();
    value["query"].as_object_mut().unwrap().remove("cursor");
    assert!(InvestigationAction::decode(&value.to_string()).is_err());
    value["query"]["cursor"] = serde_json::Value::Null;
    value["query"]["max_facts"] = json!(33);
    assert!(InvestigationAction::decode(&value.to_string()).is_err());
}

#[test]
fn investigation_finish_rejects_invented_citations_and_never_upgrades_claims() {
    // AC-0180/0184: even an implemented-behavior claim remains weak T3.
    let available = BTreeSet::from(["fact-1".into()]);
    let admitted = action()
        .admit_finish(&available, ["source-only-unrelated-text"])
        .unwrap();
    assert_eq!(admitted[0].tier, Tier::Agentic);
    assert_eq!(admitted[0].confidence_tier, ConfidenceTier::InferredWeak);
    assert_eq!(admitted[0].finding_id, "finding-1");
    assert_eq!(
        action().admit_finish(&BTreeSet::new(), std::iter::empty()),
        Err(InvestigationError::InvalidCitation)
    );
    // A required ID is a typed reference, not prose replay.
    assert!(action().admit_finish(&available, ["fact-1"]).is_ok());
}

#[test]
fn investigation_replay_checks_uncited_short_split_and_unicode_source() {
    // AC-0184/0188: every supplied string participates, regardless of citations.
    let source = "a_private_implementation_detail_that_must_never_be_persisted_as_model_prose";
    assert_eq!(
        reject_prose_replay([&source[..30], &source[30..]], [source]),
        Err(InvestigationError::SourceReplay)
    );
    assert_eq!(
        reject_prose_replay(["Paraphrased except UNSEEN detail"], ["UNSEEN"]),
        Err(InvestigationError::SourceReplay)
    );
    let unicode = "私".repeat(48);
    let spaced = unicode.chars().map(|c| format!("{c} ")).collect::<String>();
    assert_eq!(
        reject_prose_replay([spaced.as_str()], [unicode.as_str()]),
        Err(InvestigationError::SourceReplay)
    );
    assert_eq!(
        reject_prose_replay(["password=synthetic-secret"], std::iter::empty()),
        Err(InvestigationError::SensitiveOutput)
    );
    assert!(reject_prose_replay(["A new interpretation."], ["entirely unrelated"]).is_ok());
}

#[test]
fn investigation_ledger_rejects_changed_associations_and_unjustified_citations() {
    // AC-0182/0188: a saved manifest is internally exact, never a loose ID list.
    let original = ledger();
    original.validate().unwrap();
    let fingerprint = original.fingerprint().unwrap();
    let mut changed = original.clone();
    changed.citations[0].fact_digest = format!("node-v1:{}", "c".repeat(64));
    assert!(changed.validate().is_err());
    changed = original.clone();
    changed.citations.push(changed.citations[0].clone());
    assert!(changed.validate().is_err());
    changed = original.clone();
    changed.selected_facts[0].binding = Some(crate::TaskSourceAssociation {
        repo_key: "repo-a".into(),
        receipt_id: format!("ts-primary-v2:{}", "d".repeat(64)),
        emitted_fact_digest: changed.selected_facts[0].fact_digest.clone(),
    });
    assert!(changed.validate().is_err());
    changed
        .receipt_references
        .push(InvestigationReceiptReference {
            source_id: "src-one".into(),
            repo_key: "repo-a".into(),
            receipt_id: format!("ts-primary-v2:{}", "d".repeat(64)),
        });
    assert_ne!(changed.fingerprint().unwrap(), fingerprint);
    changed.queries[0].returned_facts = 0;
    assert!(changed.validate().is_err());
}

#[test]
fn investigation_result_roundtrip_retains_basis_and_rejects_authority_tampering() {
    // AC-0187/0188: admission and later reading require no current source lookup.
    let ledger = ledger();
    let result = InvestigationResult::admit(
        "inv-one",
        &ledger,
        &action(),
        ["unrelated-context"],
        Some("fixture-model".into()),
        "2026-09-11T00:00:00Z".into(),
    )
    .unwrap();
    let stored = serde_json::to_string(&result).unwrap();
    let restored: InvestigationResult = decode_record(&stored).unwrap();
    restored.validate(&ledger).unwrap();
    assert_eq!(restored, result);
    let mut changed = restored;
    changed.findings[0].confidence_tier = ConfidenceTier::Confirmed;
    assert!(changed.validate(&ledger).is_err());
    let mut raw = serde_json::to_value(&result).unwrap();
    raw["unexpected"] = json!(true);
    assert!(decode_record::<InvestigationResult>(&raw.to_string()).is_err());
}

#[test]
fn investigation_insufficient_evidence_is_a_valid_honest_finish() {
    // AC-0184: empty unsupported output does not masquerade as scoped findings.
    let mut finish = InvestigationAction::Finish {
        findings: vec![],
        knowledge_completeness: KnowledgeCompleteness::InsufficientEvidence,
        limitations: vec!["No supporting evidence was found within this bounded scope.".into()],
    };
    assert!(
        finish
            .admit_finish(&BTreeSet::new(), std::iter::empty())
            .is_ok()
    );
    if let InvestigationAction::Finish {
        knowledge_completeness,
        ..
    } = &mut finish
    {
        *knowledge_completeness = KnowledgeCompleteness::Partial;
    }
    assert!(
        finish
            .admit_finish(&BTreeSet::new(), std::iter::empty())
            .is_err()
    );
}

#[test]
fn investigation_record_preflight_rejects_depth_duplicate_and_byte_overflow() {
    // AC-0188: bad shapes are refused before recursive typed body allocation.
    let deep = format!("{}0{}", "[".repeat(34), "]".repeat(34));
    assert!(decode_record::<serde_json::Value>(&deep).is_err());
    assert!(decode_record::<serde_json::Value>("{\"a\":1,\"a\":2}").is_err());
    assert!(
        decode_record::<serde_json::Value>(&format!("\"{}\"", "x".repeat(MAX_RECORD_BYTES)))
            .is_err()
    );
}

#[test]
fn investigation_prepared_input_binds_all_query_text_and_rejects_replacement() {
    // AC-0182/0184: uncited graph strings still participate in admission, and
    // changing the actual query bytes cannot retain the original manifest.
    let private = "Never copy this complete private implementation string into durable findings.";
    let page = json!({"facts":[{"properties":{"implementation":private}}]}).to_string();
    let mut basis = ledger();
    basis.queries[0].response_hash = content_hash(page.as_bytes());
    basis.queries[0].response_bytes = page.len();
    let input = InvestigationInput::prepare(
        "Explain the bounded evidence",
        SpecialistId::DomainAnalyst,
        basis.clone(),
        std::slice::from_ref(&page),
        &[],
        &[],
        &InvestigationUsage::default(),
    )
    .unwrap();
    let payload = input.action("inv-one:step-1".into()).unwrap();
    assert!(payload.payload.prompt.contains(private));
    let mut answer = action();
    if let InvestigationAction::Finish { findings, .. } = &mut answer {
        findings[0].statement = private.into();
    }
    assert_eq!(
        input.admit_result("inv-one", &answer, None, "2026-09-11T00:00:00Z".into()),
        Err(InvestigationError::SourceReplay)
    );
    assert!(
        InvestigationInput::prepare(
            "Explain the bounded evidence",
            SpecialistId::DomainAnalyst,
            basis,
            &[page.replace("private", "replaced")],
            &[],
            &[],
            &InvestigationUsage::default()
        )
        .is_err()
    );
}

#[test]
fn investigation_short_graph_names_remain_usable_but_raw_excerpts_do_not() {
    // AC-0184/0188: per-scalar metadata admission must not prohibit all English
    // prose because a recovered symbol is named a; source excerpts stay private.
    let page = json!({"facts":[{"properties":{"name":"a","value":"false"}}]}).to_string();
    let mut basis = ledger();
    basis.queries[0].response_hash = content_hash(page.as_bytes());
    basis.queries[0].response_bytes = page.len();
    let input = InvestigationInput::prepare(
        "Explain the evidence",
        SpecialistId::DomainAnalyst,
        basis,
        &[page],
        &[],
        &[],
        &InvestigationUsage::default(),
    )
    .unwrap();
    assert!(
        input
            .admit_result("inv-one", &action(), None, "2026-09-11T00:00:00Z".into())
            .is_ok()
    );
    assert_eq!(
        reject_prose_replay(["The source says return false"], ["return false"]),
        Err(InvestigationError::SourceReplay)
    );
}

#[test]
fn investigation_detail_roundtrip_closes_flattened_and_nested_unknown_fields() {
    // AC-0188: reusable public DTOs have a strict new storage contract.
    let detail = InvestigationDetail {
        summary: InvestigationSummary {
            schema_version: 1,
            investigation_id: "inv-one".into(),
            conversation_id: "conversation-one".into(),
            parent_id: None,
            job_id: 1,
            specialist_id: SpecialistId::DomainAnalyst,
            question: "Explain checkout".into(),
            scope: InvestigationScope::All,
            provider_mode: InvestigationProviderMode::Local,
            status: InvestigationStatus::Queued,
            revision: 0,
            cancel_requested: false,
            invocation_pending: false,
            actions: InvestigationActions {
                can_cancel: true,
                can_follow_up: false,
            },
            graph_snapshot_id: None,
            last_event_sequence: 1,
            has_result: false,
            created_at: "2026-09-11T00:00:00Z".into(),
            updated_at: "2026-09-11T00:00:00Z".into(),
        },
        context_owner: "context-owner".into(),
        origin: InvestigationOrigin::App,
        specialist: SpecialistId::DomainAnalyst.definition().unwrap(),
        provider: InvestigationProvider {
            mode: InvestigationProviderMode::Local,
            provider_id: "test".into(),
            model: "fixture".into(),
            endpoint: "http://127.0.0.1:11434".into(),
            deployment: None,
            available: true,
            unavailable_reason: None,
        },
        scope_snapshot_id: None,
        limits: InvestigationLimits::default(),
        usage: InvestigationUsage::default(),
        citations: vec![],
        error: None,
    };
    let raw = serde_json::to_string(&detail).unwrap();
    assert_eq!(decode_record::<InvestigationDetail>(&raw).unwrap(), detail);
    let mut altered = serde_json::to_value(&detail).unwrap();
    altered["unknown"] = json!(true);
    assert!(decode_record::<InvestigationDetail>(&altered.to_string()).is_err());
}

#[test]
fn investigation_nested_prior_finding_prose_cannot_be_replayed() {
    // AC-0184/0188: a short prior statement inside JSON retains source privacy.
    let mut prior_action = action();
    if let InvestigationAction::Finish { findings, .. } = &mut prior_action {
        findings[0].statement = "Reserved internal conclusion".into();
    }
    let parent = InvestigationResult::admit(
        "inv-parent",
        &ledger(),
        &prior_action,
        std::iter::empty(),
        None,
        "2026-09-11T00:00:00Z".into(),
    )
    .unwrap();
    let history = vec![
        json!({
            "parent_investigation_id": "inv-parent",
            "original_graph_snapshot_id": parent.graph_snapshot_id,
            "original_scope": {"type":"all"},
            "saved_execution_status": "completed",
            "cancellation_requested": false,
            "invocation_outcome_pending": false,
            "authority": "Prior findings remain T3/InferredWeak.",
            "saved_result": parent
        })
        .to_string(),
    ];
    let bytes = serde_json::to_vec(&history).unwrap();
    let mut basis = ledger();
    basis.supplied_history_hash = Some(content_hash(&bytes));
    basis.supplied_history_bytes = bytes.len();
    let page = json!({"facts":[]}).to_string();
    basis.queries[0].response_hash = content_hash(page.as_bytes());
    basis.queries[0].response_bytes = page.len();
    let input = InvestigationInput::prepare(
        "Explain evidence",
        SpecialistId::DomainAnalyst,
        basis,
        &[page],
        &[],
        &history,
        &InvestigationUsage::default(),
    )
    .unwrap();
    let mut response = action();
    if let InvestigationAction::Finish { findings, .. } = &mut response {
        findings[0].statement =
            "I agree with the Reserved internal conclusion from earlier.".into();
    }
    assert_eq!(
        input.admit_result("inv-child", &response, None, "2026-09-11T00:00:00Z".into()),
        Err(InvestigationError::SourceReplay)
    );
}

#[test]
fn investigation_output_metadata_cannot_complete_a_split_source_replay() {
    // AC-0184/0188: metadata is joined with all result prose during admission.
    let mut answer = action();
    if let InvestigationAction::Finish { limitations, .. } = &mut answer {
        *limitations = vec!["retained-private-".into()];
    }
    assert_eq!(
        InvestigationResult::admit(
            "inv-one",
            &ledger(),
            &answer,
            ["retained-private-source-value"],
            Some("source-value".into()),
            "2026-09-11T00:00:00Z".into()
        ),
        Err(InvestigationError::SourceReplay)
    );
}
