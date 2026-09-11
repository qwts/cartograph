//! Historical inspection uses the saved ledger, original receipt occurrence and
//! retained bytes. It has no current graph, source-registry or checkout fallback.

use super::HostError;
use crate::{
    AppState,
    primary_source::{PinnedReadError, PrimarySourceStore},
};
use agents::TaskCaptureSpanRef;
use agents::investigation::{
    InvestigationCitation, InvestigationCitationRead, InvestigationCitationStatus,
    InvestigationEvidenceOrigin, InvestigationInputLedger,
};

/// The caller must load `ledger` by this investigation ID through the validated
/// durable store. The external command accepts investigation/citation IDs only;
/// neither a caller-supplied ledger nor a current-fact source viewer is authority.
pub(super) fn read(
    state: &AppState,
    investigation_id: &str,
    ledger: &InvestigationInputLedger,
    citation_id: &str,
) -> Result<InvestigationCitationRead, HostError> {
    read_saved(
        &state.primary_sources,
        investigation_id,
        ledger,
        citation_id,
    )
}

fn read_saved(
    store: &PrimarySourceStore,
    investigation_id: &str,
    ledger: &InvestigationInputLedger,
    citation_id: &str,
) -> Result<InvestigationCitationRead, HostError> {
    if !valid_id(investigation_id) || !valid_id(citation_id) {
        return Err(HostError::InvalidInput);
    }
    ledger.validate()?;
    let citation = ledger
        .citations
        .iter()
        .find(|citation| citation.citation_id == citation_id)
        .ok_or(HostError::InvalidInput)?;
    let (status, text) = match &citation.origin {
        InvestigationEvidenceOrigin::GraphMetadata => {
            (InvestigationCitationStatus::MetadataOnly, None)
        }
        InvestigationEvidenceOrigin::WorkingTreeUnverified => {
            // Original source metadata is historical; its old bytes were never
            // retained. No read of a current or same-named checkout is allowed.
            (InvestigationCitationStatus::WorkingTreeUnverified, None)
        }
        InvestigationEvidenceOrigin::CapturedPrimarySource { .. } => {
            match captured_text(store, citation) {
                Ok(text) => (InvestigationCitationStatus::Available, Some(text)),
                Err(PinnedReadError::Unavailable) => {
                    (InvestigationCitationStatus::Unavailable, None)
                }
                Err(PinnedReadError::Invalid) => (InvestigationCitationStatus::Invalid, None),
                Err(PinnedReadError::Operational) => {
                    (InvestigationCitationStatus::OperationalFailure, None)
                }
            }
        }
    };
    Ok(InvestigationCitationRead {
        investigation_id: investigation_id.into(),
        citation_id: citation_id.into(),
        status,
        citation: citation.clone(),
        text,
    })
}

fn captured_text(
    store: &PrimarySourceStore,
    citation: &InvestigationCitation,
) -> Result<String, PinnedReadError> {
    let InvestigationEvidenceOrigin::CapturedPrimarySource {
        registered_source_id,
        receipt_id,
        receipt_inventory_index,
        captured,
        ..
    } = &citation.origin
    else {
        return Err(PinnedReadError::Invalid);
    };
    // The generic pure ledger treats source IDs as opaque. The host's retained
    // namespace has a stricter registered-ID shape; malformed IDs are invalid
    // metadata, not a failure to obtain a legitimate operational lock.
    if !registered_source_id
        .strip_prefix("src_")
        .is_some_and(|value| {
            value.len() == 32
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        return Err(PinnedReadError::Invalid);
    }
    let source = citation.source.as_ref().ok_or(PinnedReadError::Invalid)?;
    let wanted_role = citation.role.ok_or(PinnedReadError::Invalid)?;
    let wanted_index = citation.index.ok_or(PinnedReadError::Invalid)?;
    let expected_hash = citation
        .text_hash
        .as_ref()
        .ok_or(PinnedReadError::Invalid)?;
    let range_bytes = captured
        .byte_end
        .checked_sub(captured.byte_start)
        .filter(|length| *length > 0 && *length <= 8192)
        .ok_or(PinnedReadError::Invalid)?;
    if captured.file.byte_len > 16 * 1024 * 1024 {
        return Err(PinnedReadError::Invalid);
    }

    // No application/database mutex is held while acquiring this source-tagged
    // shared guard. Pinned methods revalidate the guard's store/source identity.
    let guard = store.task_guard(registered_source_id)?;
    let receipt = store.task_receipt(
        &guard,
        &crate::task_evidence::graph_fact(&citation.fact),
        &citation.fact_digest,
        &source.repo,
        receipt_id,
    )?;
    let inventory_index =
        usize::try_from(*receipt_inventory_index).map_err(|_| PinnedReadError::Invalid)?;
    let original = receipt
        .ranges()
        .get(inventory_index)
        .ok_or(PinnedReadError::Invalid)?;
    if super::evidence::role(original.role) != wanted_role
        || original.index != wanted_index
        || &original.evidence != source
        || !same_capture(&original.captured, captured)
    {
        return Err(PinnedReadError::Invalid);
    }
    // The saved inventory position, role/index, whole file identity and original
    // EvidenceRef have all matched before reading. This validates at most one
    // 16-MiB captured object and returns at most 8 KiB of strict UTF-8 source.
    let text = store.task_text(&guard, &receipt, inventory_index)?;
    if text.len() as u64 != range_bytes
        || core_prov::content_hash(text.as_bytes()) != *expected_hash
    {
        return Err(PinnedReadError::Invalid);
    }
    Ok(text)
}

fn same_capture(actual: &source_capture::CaptureSpanRef, saved: &TaskCaptureSpanRef) -> bool {
    actual.file.source_id.as_str() == saved.file.source_id
        && actual.file.capture_id == saved.file.capture_id
        && actual.file.path == saved.file.path
        && actual.file.digest == saved.file.digest
        && actual.file.byte_len == saved.file.byte_len
        && actual.byte_start == saved.byte_start
        && actual.byte_end == saved.byte_end
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_:".contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use adapters_lang_ts::captured::{self, RangeRole};
    use agents::investigation::*;
    use agents::{
        InputClosureStatus, PrimarySourceScope, TaskCaptureFileRef, TaskFactSelection,
        TaskSourceAssociation,
    };
    use context_hub::{ContextSnapshot, FactKind, QueryRequest, QueryScope};
    use source_capture::{CaptureStore, SourceId, StoreLimits};
    use std::path::PathBuf;

    const ORIGINAL: &str = "// café — original retained fixture\nexport function ready(ok: boolean) { const limit = ok === false; if (limit) return false; }\n";
    const INVESTIGATION: &str = "investigation-history-fixture";

    struct Fixture {
        _directory: tempfile::TempDir,
        app_data: PathBuf,
        source: crate::sources::RegisteredSource,
        store: PrimarySourceStore,
        ledger: InvestigationInputLedger,
        text: String,
    }

    fn fixture() -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let app_data = crate::paths::canonicalize(directory.path()).unwrap();
        let target = app_data.join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("rule.ts"), ORIGINAL).unwrap();
        let state_path = app_data.join("state.sqlite");
        let _findings = crate::findings::FindingStore::open(&state_path).unwrap();
        let mut registry = crate::sources::SourceRegistry::open(&state_path, &app_data).unwrap();
        let source = registry.register_local(&target).unwrap();
        let store = PrimarySourceStore::open(&app_data).unwrap();
        let input = store
            .prepare(&source, source.root(), &["client".into()])
            .unwrap();
        let capture = input.capture.as_ref().unwrap();
        let (extraction, receipts) = captured::extract_file(
            capture.file("rule.ts").unwrap(),
            &adapters_lang_ts::SourceId {
                repo: &source.repo_key,
                commit: "workdir",
            },
        )
        .unwrap();
        store.persist(&input, &source, &receipts).unwrap();
        let receipt = receipts
            .iter()
            .find(|receipt| {
                receipt
                    .ranges()
                    .iter()
                    .any(|range| range.role == RangeRole::DefinitionInitializer)
            })
            .unwrap();
        let (inventory_index, range) = receipt
            .ranges()
            .iter()
            .enumerate()
            .find(|(_, range)| range.role == RangeRole::DefinitionInitializer)
            .unwrap();
        let original = &ORIGINAL.as_bytes()
            [range.captured.byte_start as usize..range.captured.byte_end as usize];
        let text = std::str::from_utf8(original).unwrap().to_string();
        let fact = crate::task_evidence::task_fact(receipt.fact_key());
        let snapshot = ContextSnapshot::new(extraction.nodes, extraction.edges).unwrap();
        let page = snapshot
            .query(QueryRequest {
                scope: QueryScope::All,
                kind: Some(FactKind::Node),
                labels: vec!["BusinessRule".into()],
                max_facts: 32,
                max_bytes: 32 * 1024,
                cursor: None,
            })
            .unwrap();
        let page_bytes = serde_json::to_vec(&page).unwrap();
        let graph_citation = InvestigationCitation {
            citation_id: "fact-1".into(),
            fact: fact.clone(),
            fact_digest: receipt.fact_digest().into(),
            source: None,
            role: None,
            index: None,
            text_hash: None,
            origin: InvestigationEvidenceOrigin::GraphMetadata,
        };
        let citation = InvestigationCitation {
            citation_id: "evidence-1".into(),
            fact: fact.clone(),
            fact_digest: receipt.fact_digest().into(),
            source: Some(range.evidence.clone()),
            role: Some(super::super::evidence::role(range.role)),
            index: Some(range.index),
            text_hash: Some(core_prov::content_hash(original)),
            origin: InvestigationEvidenceOrigin::CapturedPrimarySource {
                registered_source_id: source.source_id.clone(),
                receipt_id: receipt.id().into(),
                receipt_inventory_index: inventory_index as u32,
                captured: TaskCaptureSpanRef {
                    file: TaskCaptureFileRef {
                        source_id: range.captured.file.source_id.as_str().into(),
                        capture_id: range.captured.file.capture_id.clone(),
                        path: range.captured.file.path.clone(),
                        digest: range.captured.file.digest.clone(),
                        byte_len: range.captured.file.byte_len,
                    },
                    byte_start: range.captured.byte_start,
                    byte_end: range.captured.byte_end,
                },
                scope: PrimarySourceScope::PrimarySourceOnly,
                input_closure: InputClosureStatus::InputClosureNotEstablished,
            },
        };
        let ledger = InvestigationInputLedger {
            schema_version: 1,
            graph_snapshot_id: snapshot.id().into(),
            scope_snapshot_id: snapshot.id().into(),
            revision: 2,
            selected_facts: vec![TaskFactSelection {
                fact,
                fact_digest: receipt.fact_digest().into(),
                binding: Some(TaskSourceAssociation {
                    repo_key: source.repo_key.clone(),
                    receipt_id: receipt.id().into(),
                    emitted_fact_digest: receipt.fact_digest().into(),
                }),
            }],
            receipt_references: vec![InvestigationReceiptReference {
                source_id: source.source_id.clone(),
                repo_key: source.repo_key.clone(),
                receipt_id: receipt.id().into(),
            }],
            citations: vec![graph_citation, citation],
            queries: vec![InvestigationQueryManifest {
                query: InvestigationQuery {
                    scope: InvestigationScope::All,
                    kind: Some(InvestigationFactKind::Node),
                    labels: vec!["BusinessRule".into()],
                    max_facts: 32,
                    max_bytes: 32 * 1024,
                    cursor: None,
                },
                response_hash: core_prov::content_hash(&page_bytes),
                response_bytes: page_bytes.len(),
                returned_facts: page.facts.len(),
                total_selected: page.total_selected,
                has_more: page.next_cursor.is_some(),
            }],
            supplied_history_hash: None,
            supplied_history_bytes: 0,
        };
        ledger.validate().unwrap();
        drop(input);
        Fixture {
            _directory: directory,
            app_data,
            source,
            store,
            ledger,
            text,
        }
    }

    fn read_fixture(
        fixture: &Fixture,
        ledger: &InvestigationInputLedger,
        citation: &str,
    ) -> InvestigationCitationRead {
        read_saved(&fixture.store, INVESTIGATION, ledger, citation).unwrap()
    }

    // AC-0189: actual TS v2 producer/receipt/capture bytes survive checkout
    // replacement and deletion, saved metadata decoding and a reopened store.
    #[test]
    fn investigation_historical_citations_retain_nested_source_after_restart() {
        let fixture = fixture();
        assert_eq!(fixture.text, "ok === false");
        std::fs::write(
            fixture.source.root().join("rule.ts"),
            "CURRENT CHECKOUT MUST NOT BE READ",
        )
        .unwrap();
        assert_eq!(
            read_fixture(&fixture, &fixture.ledger, "evidence-1").text,
            Some(fixture.text.clone())
        );
        let serialized = serde_json::to_string(&fixture.ledger).unwrap();
        std::fs::remove_dir_all(fixture.source.root()).unwrap();
        // Close every original store handle before reopening the same namespace.
        let Fixture {
            _directory,
            app_data,
            store,
            text,
            ..
        } = fixture;
        drop(store);
        let reopened = PrimarySourceStore::open(&app_data).unwrap();
        let saved: InvestigationInputLedger = decode_record(&serialized).unwrap();
        let observed = read_saved(&reopened, INVESTIGATION, &saved, "evidence-1").unwrap();
        assert_eq!(observed.status, InvestigationCitationStatus::Available);
        assert_eq!(observed.text, Some(text));
        assert_eq!(observed.citation, saved.citations[1]);
    }

    // AC-0189: matching a path or overlapping source is insufficient. Every
    // original occurrence and capture component must match, including root/init
    // occurrences which deliberately share source offsets.
    #[test]
    fn investigation_historical_citations_reject_rebound_occurrences_and_hashes() {
        let fixture = fixture();
        let mut changes = Vec::new();
        let mut changed = fixture.ledger.clone();
        changed.citations[1].text_hash = Some("0".repeat(64));
        changes.push(changed);
        let mut changed = fixture.ledger.clone();
        changed.citations[1].role = Some(agents::TaskRangeRole::DefinitionExpression);
        changes.push(changed);
        let mut changed = fixture.ledger.clone();
        changed.citations[1].index = Some(changed.citations[1].index.unwrap() + 1);
        changes.push(changed);
        let mut changed = fixture.ledger.clone();
        changed.citations[1]
            .source
            .as_mut()
            .unwrap()
            .commit_sha
            .push_str("-other");
        changes.push(changed);
        let mut changed = fixture.ledger.clone();
        if let InvestigationEvidenceOrigin::CapturedPrimarySource { captured, .. } =
            &mut changed.citations[1].origin
        {
            captured.file.capture_id = format!("capture-v1:{}", "0".repeat(64));
        }
        changes.push(changed);
        let mut changed = fixture.ledger.clone();
        if let InvestigationEvidenceOrigin::CapturedPrimarySource {
            receipt_inventory_index,
            ..
        } = &mut changed.citations[1].origin
        {
            *receipt_inventory_index += 1;
        }
        changes.push(changed);
        let mut changed = fixture.ledger.clone();
        let digest = format!("node-v1:{}", "0".repeat(64));
        changed.selected_facts[0].fact_digest = digest.clone();
        changed.selected_facts[0]
            .binding
            .as_mut()
            .unwrap()
            .emitted_fact_digest = digest.clone();
        for citation in &mut changed.citations {
            citation.fact_digest = digest.clone();
        }
        changes.push(changed);
        for changed in changes {
            changed.validate().unwrap();
            let observed = read_fixture(&fixture, &changed, "evidence-1");
            assert_eq!(observed.status, InvestigationCitationStatus::Invalid);
            assert!(observed.text.is_none());
        }
        assert_eq!(
            read_saved(&fixture.store, INVESTIGATION, &fixture.ledger, "unknown").unwrap_err(),
            HostError::InvalidInput
        );
    }

    // AC-0189: lock failure, forgotten content and metadata-only history retain
    // different meanings. No current checkout or graph viewer is consulted.
    #[test]
    fn investigation_historical_citations_distinguish_unavailable_operational_and_metadata() {
        let fixture = fixture();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(
                fixture
                    .app_data
                    .join("retained-source/locks")
                    .join(format!("{}.lock", fixture.source.source_id)),
            )
            .unwrap();
        file.try_lock().unwrap();
        assert_eq!(
            read_fixture(&fixture, &fixture.ledger, "evidence-1").status,
            InvestigationCitationStatus::OperationalFailure
        );
        let metadata = read_fixture(&fixture, &fixture.ledger, "fact-1");
        assert_eq!(metadata.status, InvestigationCitationStatus::MetadataOnly);
        assert!(metadata.text.is_none());
        let mut unverified = fixture.ledger.clone();
        unverified.selected_facts[0].binding = None;
        unverified.receipt_references.clear();
        unverified.citations[1].origin = InvestigationEvidenceOrigin::WorkingTreeUnverified;
        unverified.citations[1].role = Some(agents::TaskRangeRole::Provenance);
        unverified.citations[1].index = Some(0);
        let observed = read_fixture(&fixture, &unverified, "evidence-1");
        assert_eq!(
            observed.status,
            InvestigationCitationStatus::WorkingTreeUnverified
        );
        assert!(observed.text.is_none());
        file.unlock().unwrap();
        let mut captures = CaptureStore::open(
            fixture.app_data.join("retained-source/captures.sqlite"),
            StoreLimits::default(),
        )
        .unwrap();
        let source_id = SourceId::new(&fixture.source.source_id).unwrap();
        let inventory = captures.source_inventory(&source_id).unwrap();
        let ids = inventory
            .into_iter()
            .map(|capture| capture.capture_id)
            .collect::<Vec<_>>();
        assert!(captures.forget_source(&source_id, &ids).unwrap() > 0);
        assert!(fixture.source.root().join("rule.ts").is_file());
        let observed = read_fixture(&fixture, &fixture.ledger, "evidence-1");
        assert_eq!(observed.status, InvestigationCitationStatus::Unavailable);
        assert!(observed.text.is_none());
        assert_eq!(observed.citation, fixture.ledger.citations[1]);
    }
}
