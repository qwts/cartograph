use super::*;
use crate::*;
use core_prov::{ConfidenceTier, EvidenceRef};
use llm::{
    Completion, EgressPolicy, Embedding, Locality, ProviderCaps, ProviderCompletionRequest,
    ProviderError,
};
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) fn fixture() -> PreparedAgentTask {
    let evidence = |id: &str, path: &str, text: &str| AgentEvidence {
        id: id.into(),
        source: EvidenceRef {
            repo: "acme/project".into(),
            path: path.into(),
            byte_start: 0,
            byte_end: text.len() as u64,
            commit_sha: "a".repeat(40),
        },
        text: text.into(),
    };
    let task = AgentTask {
        action_id: "escalate:prepared".into(),
        gap_id: "gap:resolution".into(),
        source_id: "symbol:source".into(),
        edge_label: "CALLS".into(),
        existing_confidence: ConfidenceTier::Gap,
        source_evidence_ids: vec!["source".into(), "gap".into()],
        evidence: vec![
            evidence("source", "source.ts", "source private marker 73194"),
            evidence("gap", "gap.ts", "uncited private marker 76213"),
            evidence("target", "target.ts", "target private marker 97814"),
        ],
        candidates: vec![AgentCandidate {
            node_id: "symbol:target".into(),
            label: "Symbol".into(),
            summary: "unpublished candidate summary 87613".into(),
            evidence_ids: vec!["target".into()],
        }],
    };
    let fact = |id: &str| TaskFactKey::Node { id: id.into() };
    let receipt_id = format!("ts-primary-v2:{}", "b".repeat(64));
    let digest = format!("node-v1:{}", "d".repeat(64));
    let mut selected_facts = vec![
        TaskFactSelection {
            fact: fact(&task.source_id),
            fact_digest: digest.clone(),
            binding: Some(TaskSourceAssociation {
                repo_key: "acme/project".into(),
                receipt_id: receipt_id.clone(),
                emitted_fact_digest: digest.clone(),
            }),
        },
        TaskFactSelection {
            fact: fact(&task.gap_id),
            fact_digest: digest.clone(),
            binding: None,
        },
        TaskFactSelection {
            fact: fact(&task.candidates[0].node_id),
            fact_digest: digest,
            binding: None,
        },
        TaskFactSelection {
            fact: TaskFactKey::Edge {
                source: task.source_id.clone(),
                label: task.edge_label.clone(),
                destination: task.gap_id.clone(),
            },
            fact_digest: format!("edge-v1:{}", "e".repeat(64)),
            binding: None,
        },
    ];
    selected_facts.sort_by(|a, b| a.fact.cmp(&b.fact));
    let source_basis = TaskSourceBasisV2 {
        schema_version: 2,
        graph_snapshot_id: format!("context-v1:{}", "f".repeat(64)),
        selected_facts,
        evidence: vec![
            TaskEvidenceOrigin {
                evidence_id: "source".into(),
                fact: fact(&task.source_id),
                role: TaskRangeRole::Provenance,
                index: 0,
                origin: TaskEvidenceKind::CapturedPrimarySource {
                    registered_source_id: "src_registered".into(),
                    receipt_id,
                    receipt_inventory_index: 0,
                    captured: TaskCaptureSpanRef {
                        file: TaskCaptureFileRef {
                            source_id: "src_registered".into(),
                            capture_id: format!("capture-v1:{}", "a".repeat(64)),
                            path: "source.ts".into(),
                            digest: "c".repeat(64),
                            byte_len: 128,
                        },
                        byte_start: 0,
                        byte_end: task.evidence[0].text.len() as u64,
                    },
                    scope: PrimarySourceScope::PrimarySourceOnly,
                    input_closure: InputClosureStatus::InputClosureNotEstablished,
                },
            },
            TaskEvidenceOrigin {
                evidence_id: "gap".into(),
                fact: fact(&task.gap_id),
                role: TaskRangeRole::Provenance,
                index: 0,
                origin: TaskEvidenceKind::WorkingTreeUnverified,
            },
            TaskEvidenceOrigin {
                evidence_id: "target".into(),
                fact: fact(&task.candidates[0].node_id),
                role: TaskRangeRole::Provenance,
                index: 0,
                origin: TaskEvidenceKind::WorkingTreeUnverified,
            },
        ],
        selection: SelectionReport {
            metadata_lookahead: 3,
            acquisition_attempts: 3,
            supplied_evidence: 3,
            supplied_candidates: 1,
            captured_validation_bytes: 128,
            limits: SelectionLimits::default(),
            omissions: vec![],
            metadata_preselected_not_read: 0,
            unread_tail: false,
            stop_reason: SelectionStopReason::NeighborhoodExhausted,
        },
    };
    PreparedAgentTask::new(task, source_basis).unwrap()
}
pub(crate) fn result(
    task: &PreparedAgentTask,
    annotation: &str,
    citations: &[&str],
) -> AgentProposal {
    AgentBroker::bounded_default()
        .validate_response_with_basis(
            task.task(),
            RawProposal {
                target_id: task.task().candidates[0].node_id.clone(),
                annotation: annotation.into(),
                citations: citations.iter().map(|s| (*s).into()).collect(),
            },
            task.basis_hash().unwrap(),
        )
        .unwrap()
}
pub(crate) fn change_receipt(task: &PreparedAgentTask) -> PreparedAgentTask {
    let mut source = task.source_basis().clone();
    let receipt = format!("ts-primary-v2:{}", "1".repeat(64));
    for fact in &mut source.selected_facts {
        if let Some(binding) = &mut fact.binding {
            binding.receipt_id = receipt.clone();
        }
    }
    if let TaskEvidenceKind::CapturedPrimarySource { receipt_id, .. } =
        &mut source.evidence[0].origin
    {
        *receipt_id = receipt;
    }
    PreparedAgentTask::new(task.task().clone(), source).unwrap()
}
struct Provider {
    calls: AtomicUsize,
}
impl LlmProvider for Provider {
    fn id(&self) -> &str {
        "cloud:prepared-test"
    }
    fn locality(&self) -> Locality {
        Locality::Cloud
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: false,
            chat: true,
            tool_use: false,
        }
    }
    fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        Err(ProviderError::Unsupported("embedding"))
    }
    fn complete(&self, request: &ProviderCompletionRequest) -> Result<Completion, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(request.prompt().contains("source_basis_fingerprint"));
        assert_eq!(request.spans().len(), 3);
        Ok(Completion {text:r#"{"target_id":"symbol:target","annotation":"Possible target link","citations":["source","target"]}"#.into()})
    }
}
#[test]
fn prepared_receipt_only_changes_invalidate_identity_and_cloud_consent() {
    // AC-0175/AC-0176: same graph, source span and action with different receipt
    // identity requires new consent; raw provider material cannot manufacture it.
    let task = fixture();
    let changed = change_receipt(&task);
    assert_eq!(task.task(), changed.task());
    assert_ne!(task.basis_hash().unwrap(), changed.basis_hash().unwrap());
    let broker = AgentBroker::bounded_default();
    let provider = Provider {
        calls: AtomicUsize::new(0),
    };
    let firewall = EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Agentic]));
    let first = broker
        .preview_prepared(&provider, &firewall, &task)
        .unwrap();
    let second = broker
        .preview_prepared(&provider, &firewall, &changed)
        .unwrap();
    assert_ne!(first.payload_hash, second.payload_hash);
    let grant = ConsentGrant::from_preview(&first);
    assert!(
        broker
            .propose_prepared(&provider, &firewall, &changed, Some(&grant))
            .is_err()
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let proposal = broker
        .propose_prepared(
            &provider,
            &firewall,
            &changed,
            Some(&ConsentGrant::from_preview(&second)),
        )
        .unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(proposal.basis_hash, changed.basis_hash().unwrap());
    assert_ne!(
        proposal.provenance.content_hash,
        result(&task, "Possible target link", &["source", "target"])
            .provenance
            .content_hash
    );
}
#[test]
fn prepared_hash_is_action_independent_and_membership_canonical() {
    // AC-0175: canonical semantic hash remains separate from consent action ID.
    let prepared = fixture();
    let mut task = prepared.task().clone();
    task.action_id = "another-action".into();
    task.evidence.reverse();
    task.source_evidence_ids.reverse();
    let mut basis = prepared.source_basis().clone();
    basis.evidence.reverse();
    let changed = PreparedAgentTask::new(task, basis).unwrap();
    assert_eq!(
        prepared.basis_hash().unwrap(),
        changed.basis_hash().unwrap()
    );
    let mut task = prepared.task().clone();
    task.candidates[0].summary.push('!');
    let changed = PreparedAgentTask::new(task, prepared.source_basis().clone()).unwrap();
    assert_ne!(
        prepared.basis_hash().unwrap(),
        changed.basis_hash().unwrap()
    );
}
#[test]
fn prepared_basis_rejects_mismatched_items_selections_and_bounds() {
    // AC-0172/AC-0175: exact copied span, association, role, source identity and
    // bounded visited-prefix accounting are mandatory, even for uncited items.
    let task = fixture();
    let mutations: [fn(&mut TaskSourceBasisV2); 11] = [
        |b| b.schema_version = 1,
        |b| b.graph_snapshot_id = "snapshot:invented".into(),
        |b| b.evidence[0].evidence_id = "gap".into(),
        |b| b.evidence[0].origin = TaskEvidenceKind::WorkingTreeUnverified,
        |b| {
            if let TaskEvidenceKind::CapturedPrimarySource {
                registered_source_id,
                ..
            } = &mut b.evidence[0].origin
            {
                *registered_source_id = "foreign".into()
            }
        },
        |b| {
            if let TaskEvidenceKind::CapturedPrimarySource { captured, .. } =
                &mut b.evidence[0].origin
            {
                captured.byte_end -= 1
            }
        },
        |b| {
            if let TaskEvidenceKind::CapturedPrimarySource {
                receipt_inventory_index,
                ..
            } = &mut b.evidence[0].origin
            {
                *receipt_inventory_index = 1024
            }
        },
        |b| b.selection.captured_validation_bytes -= 1,
        |b| b.selection.metadata_preselected_not_read = 1,
        |b| b.selection.limits.acquisition_attempts = 65,
        |b| b.selected_facts.reverse(),
    ];
    for mutation in mutations {
        let mut basis = task.source_basis().clone();
        mutation(&mut basis);
        assert!(PreparedAgentTask::new(task.task().clone(), basis).is_err());
    }
    let mut changed = task.task().clone();
    changed.candidates[0].summary = "x".repeat(128 * 1024 + 1);
    assert!(PreparedAgentTask::new(changed, task.source_basis().clone()).is_err());
    let mut basis = task.source_basis().clone();
    basis.graph_snapshot_id = "x".repeat(MAX_SOURCE_BASIS_BYTES + 1);
    assert!(basis.fingerprint().is_err());
}
#[test]
fn prepared_selection_reports_omissions_and_unread_metadata_without_fake_facts() {
    // AC-0172: a missing citation is attempted and omitted; a metadata-only tail
    // is not promoted into the durable visited selection. Slot-less fallback is allowed.
    let original = fixture();
    let mut task = original.task().clone();
    let mut basis = original.source_basis().clone();
    task.evidence.remove(1);
    task.source_evidence_ids.retain(|s| s != "gap");
    basis.evidence.remove(1);
    basis.selection.supplied_evidence = 2;
    basis.selection.omissions.push(SelectionOmission {
        request_index: 1,
        fact: TaskFactKey::Node {
            id: task.gap_id.clone(),
        },
        reason: SelectionOmissionReason::MissingCitation,
    });
    basis
        .selected_facts
        .retain(|s| !matches!(s.fact, TaskFactKey::Edge { .. }));
    assert!(PreparedAgentTask::new(task.clone(), basis.clone()).is_ok());
    basis.selected_facts.push(TaskFactSelection {
        fact: TaskFactKey::Node {
            id: "unvisited".into(),
        },
        fact_digest: format!("node-v1:{}", "a".repeat(64)),
        binding: None,
    });
    basis.selected_facts.sort_by(|a, b| a.fact.cmp(&b.fact));
    assert!(PreparedAgentTask::new(task, basis).is_err());
}

#[test]
fn prepared_admission_rejects_duplicate_memberships_and_invalid_legacy_spans() {
    // AC-0172/AC-0175: reject these before preview/provider execution, rather
    // than discovering an invalid immutable task only when its result is staged.
    let original = fixture();
    let mutations: [fn(&mut AgentTask); 4] = [
        |task| {
            task.source_evidence_ids
                .push(task.source_evidence_ids[0].clone())
        },
        |task| task.candidates[0].evidence_ids.push("target".into()),
        |task| task.evidence[1].source.byte_end = task.evidence[1].source.byte_start,
        |task| task.action_id = "x".repeat(8193),
    ];
    for mutation in mutations {
        let mut task = original.task().clone();
        mutation(&mut task);
        assert!(PreparedAgentTask::new(task, original.source_basis().clone()).is_err());
    }
}
