//! Read-only selected-history assessment. Never changes immutable task meaning.

use super::*;
use serde::Serialize;
use tauri::Manager;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BasisComparison {
    Unchanged,
    Changed,
    LegacyUnverified,
    OperationalFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Availability {
    Available,
    Unavailable,
    Invalid,
    OperationalFailure,
    ValidationByteLimit,
    WorkingTreeUnverified,
}

impl From<PinnedReadError> for Availability {
    fn from(error: PinnedReadError) -> Self {
        match error {
            PinnedReadError::Unavailable => Self::Unavailable,
            PinnedReadError::Invalid => Self::Invalid,
            PinnedReadError::Operational => Self::OperationalFailure,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct EvidenceAvailability {
    pub evidence_id: String,
    pub status: Availability,
}

#[derive(Debug, Serialize)]
pub(crate) struct BasisAssessment {
    pub schema_version: u32,
    pub proposal_id: String,
    pub current_graph_snapshot_id: Option<String>,
    pub graph_comparison: BasisComparison,
    pub association_comparison: BasisComparison,
    pub evidence: Vec<EvidenceAvailability>,
    pub unverified_evidence: usize,
    pub captured_validation_bytes: u64,
    pub max_captured_validation_bytes: u64,
}

fn checked_availability(
    state: &AppState,
    basis: &TaskSourceBasisV2,
    staged: &agents::StagedProposal,
    origin: &TaskEvidenceOrigin,
    guard: &RetentionGuard,
    charged: &mut u64,
) -> Availability {
    let TaskEvidenceKind::CapturedPrimarySource {
        receipt_id,
        receipt_inventory_index,
        captured,
        ..
    } = &origin.origin
    else {
        return Availability::WorkingTreeUnverified;
    };
    let Some(selected) = basis
        .selected_facts
        .iter()
        .find(|selected| selected.fact == origin.fact)
    else {
        return Availability::Invalid;
    };
    let Some(binding) = &selected.binding else {
        return Availability::Invalid;
    };
    let Some(evidence) = staged
        .basis
        .evidence
        .iter()
        .find(|item| item.id == origin.evidence_id)
    else {
        return Availability::Invalid;
    };
    let receipt = match state.primary_sources.task_receipt(
        guard,
        &graph_fact(&origin.fact),
        &selected.fact_digest,
        &binding.repo_key,
        receipt_id,
    ) {
        Ok(receipt) => receipt,
        Err(error) => return error.into(),
    };
    let Some(range) = receipt.ranges().get(*receipt_inventory_index as usize) else {
        return Availability::Invalid;
    };
    // Compare the exact historical occurrence, not an arbitrary equal byte span.
    let same_role = matches!(
        (serde_json::to_value(range.role), serde_json::to_value(origin.role)),
        (Ok(left), Ok(right)) if left == right
    );
    let same_capture = matches!(
        (serde_json::to_value(&range.captured), serde_json::to_value(captured)),
        (Ok(left), Ok(right)) if left == right
    );
    if !same_role
        || !same_capture
        || range.index != origin.index
        || range.evidence != evidence.source
    {
        return Availability::Invalid;
    }
    let Some(next_charge) = charged
        .checked_add(range.captured.file.byte_len)
        .filter(|next| *next <= basis.selection.limits.captured_validation_bytes)
    else {
        return Availability::ValidationByteLimit;
    };
    *charged = next_charge;
    match state
        .primary_sources
        .task_text(guard, &receipt, *receipt_inventory_index as usize)
    {
        Ok(text)
            if text.len() == evidence.text_bytes
                && core_prov::content_hash(text.as_bytes()) == evidence.text_hash =>
        {
            Availability::Available
        }
        Ok(_) => Availability::Invalid,
        Err(error) => error.into(),
    }
}

pub(crate) fn assess(state: &AppState, proposal_id: &str) -> Result<BasisAssessment, String> {
    let staged = state
        .proposals
        .lock()
        .map_err(|_| "Proposal history is unavailable.")?
        .get(proposal_id)
        .map_err(|_| "Proposal history is unavailable or invalid.")?
        .ok_or("The selected staged proposal is unavailable.")?;
    let mut result = BasisAssessment {
        schema_version: 1,
        proposal_id: staged.proposal_id.clone(),
        current_graph_snapshot_id: None,
        graph_comparison: BasisComparison::LegacyUnverified,
        association_comparison: BasisComparison::LegacyUnverified,
        evidence: Vec::new(),
        unverified_evidence: 0,
        captured_validation_bytes: 0,
        max_captured_validation_bytes: SelectionLimits::default().captured_validation_bytes,
    };
    let Some(basis) = &staged.source_basis else {
        result.unverified_evidence = staged.basis.evidence.len();
        result.evidence = staged
            .basis
            .evidence
            .iter()
            .map(|item| EvidenceAvailability {
                evidence_id: item.id.clone(),
                status: Availability::WorkingTreeUnverified,
            })
            .collect();
        return Ok(result);
    };
    let mut sources = BTreeMap::new();
    for origin in &basis.evidence {
        if let TaskEvidenceKind::CapturedPrimarySource {
            registered_source_id,
            ..
        } = &origin.origin
        {
            sources
                .entry(registered_source_id.clone())
                .or_insert_with(|| {
                    state
                        .sources
                        .lock()
                        .map_err(|_| PinnedReadError::Operational)?
                        .get_by_id(registered_source_id)
                        .map_err(|_| PinnedReadError::Operational)?
                        .ok_or(PinnedReadError::Unavailable)
                });
        }
    }
    // No source roots or managed readiness are required for historical bytes.
    // Keep all leases through the current graph/association observation.
    let guards: BTreeMap<_, _> = sources
        .iter()
        .map(|(id, source)| {
            let guard = source
                .as_ref()
                .map_err(|error| *error)
                .and_then(|_| state.primary_sources.task_guard(id));
            (id.clone(), guard)
        })
        .collect();
    for origin in &basis.evidence {
        let status = match &origin.origin {
            TaskEvidenceKind::WorkingTreeUnverified => {
                result.unverified_evidence += 1;
                Availability::WorkingTreeUnverified
            }
            TaskEvidenceKind::CapturedPrimarySource {
                registered_source_id,
                ..
            } => {
                let expected_repo = basis
                    .selected_facts
                    .iter()
                    .find(|selected| selected.fact == origin.fact)
                    .and_then(|selected| selected.binding.as_ref())
                    .map(|binding| &binding.repo_key);
                let registration_matches = sources
                    .get(registered_source_id)
                    .and_then(|value| value.as_ref().ok())
                    .is_some_and(|source| Some(&source.repo_key) == expected_repo);
                match guards.get(registered_source_id) {
                    Some(Ok(guard)) if registration_matches => checked_availability(
                        state,
                        basis,
                        &staged,
                        origin,
                        guard,
                        &mut result.captured_validation_bytes,
                    ),
                    Some(Ok(_)) => Availability::Invalid,
                    Some(Err(error)) => (*error).into(),
                    None => Availability::OperationalFailure,
                }
            }
        };
        result.evidence.push(EvidenceAvailability {
            evidence_id: origin.evidence_id.clone(),
            status,
        });
    }
    let keys: Vec<_> = basis
        .selected_facts
        .iter()
        .map(|selected| graph_fact(&selected.fact))
        .collect();
    let observed = state
        .graph
        .lock()
        .map_err(|_| ())
        .and_then(|graph| graph.read_source_selection_snapshot(&keys).map_err(|_| ()));
    match observed {
        Ok(observed) => {
            let snapshot = context_hub::ContextSnapshot::new(observed.graph.0, observed.graph.1);
            match snapshot {
                Ok(snapshot) => {
                    result.graph_comparison = if snapshot.id() == basis.graph_snapshot_id {
                        BasisComparison::Unchanged
                    } else {
                        BasisComparison::Changed
                    };
                    result.current_graph_snapshot_id = Some(snapshot.id().into());
                }
                Err(_) => result.graph_comparison = BasisComparison::OperationalFailure,
            }
            result.association_comparison = BasisComparison::Unchanged;
            for value in &observed.selections {
                if matches!(value.state, FactSourceState::Invalid { .. }) {
                    result.association_comparison = BasisComparison::OperationalFailure;
                    break;
                }
                let current = selection(value).ok();
                let expected = basis
                    .selected_facts
                    .iter()
                    .find(|selected| selected.fact == task_fact(&value.fact));
                if current.as_ref() != expected {
                    result.association_comparison = BasisComparison::Changed;
                }
            }
        }
        Err(()) => {
            result.graph_comparison = BasisComparison::OperationalFailure;
            result.association_comparison = BasisComparison::OperationalFailure;
        }
    }
    drop(guards);
    Ok(result)
}

#[tauri::command]
pub(crate) async fn assess_staged_basis(
    proposal_id: String,
    app: tauri::AppHandle,
) -> Result<BasisAssessment, String> {
    // Invalid caller IDs must not cause an unbounded SQL/string lookup.
    if proposal_id.len() > 128 {
        return Err("Invalid staged proposal identity.".into());
    }
    crate::off_ui_thread(move || assess(&app.state::<AppState>(), &proposal_id)).await
}
