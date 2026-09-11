//! Thin IPC boundary. Durable reads and filesystem/provider preparation stay
//! off the UI thread; callers supply IDs, never leases or source payloads.
use super::{HostError, emit_changed, history, runtime, worker};
use crate::{AppState, jobs::ClaimMode};
use agents::investigation::*;
use std::time::Instant;
use tauri::Manager;

#[tauri::command]
pub(crate) async fn investigation_specialists(
    app: tauri::AppHandle,
) -> Result<InvestigationCatalog, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<AppState>();
        Ok(InvestigationCatalog {
            schema_version: 1,
            specialists: [SpecialistId::DomainAnalyst, SpecialistId::EvidenceAuditor]
                .into_iter()
                .map(|id| id.definition())
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?,
            providers: [
                InvestigationProviderMode::Local,
                InvestigationProviderMode::Cloud,
            ]
            .into_iter()
            .map(|mode| state.investigations.descriptor(mode))
            .collect(),
            limits: InvestigationLimits::default(),
        })
    })
    .await
}

#[tauri::command]
pub(crate) async fn start_investigation(
    request: StartInvestigationRequest,
    app: tauri::AppHandle,
) -> Result<InvestigationSummary, String> {
    let started = Instant::now();
    crate::off_ui_thread(move || start(request, app, started)).await
}

fn start<R: tauri::Runtime>(
    request: StartInvestigationRequest,
    app: tauri::AppHandle<R>,
    started: Instant,
) -> Result<InvestigationSummary, String> {
    request.validate().map_err(|e| e.to_string())?;
    let state = app.state::<AppState>();
    // This resolves app-lifetime configuration only, without network activity.
    // An unavailable provider is passed through: durable deduplication must win
    // over a later configuration change for an already committed request.
    let provider = state.investigations.provider(request.provider_mode).ok();
    let descriptor = provider
        .as_ref()
        .and_then(|provider| runtime::descriptor(provider.as_ref(), request.provider_mode).ok());
    let committed = state
        .jobs
        .lock()
        .map_err(|e| e.to_string())?
        .start_investigation(&request, descriptor.as_ref())
        .map_err(|e| e.to_string())?;
    if !committed.created {
        return Ok(committed.detail.summary);
    }
    emit_changed(&app, &committed.detail);
    let id = committed.detail.summary.investigation_id.clone();
    let provider = provider.ok_or_else(|| HostError::Operational.to_string())?;
    let live = state
        .investigations
        .insert(&id)
        .map_err(|e| e.to_string())?;
    let claim = (|| {
        let plan = state
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .claim_plan(committed.detail.summary.job_id, ClaimMode::StartQueued)
            .map_err(|e| e.to_string())?;
        crate::claim_job(&state, &plan)
    })();
    let (_, execution) = match claim {
        Ok(claim) => claim,
        Err(error) => {
            state.investigations.remove(&id);
            // The nonce still recovers the committed identity. No second worker
            // or automatic re-claim is allowed after an uncertain start.
            return Err(error);
        }
    };
    // Keep the execution in the detached blocking worker even if the invoking
    // async waiter goes away. Persisted history is the response authority.
    let worker_app = app.clone();
    let worker_id = id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        worker::run(
            worker_app, worker_id, request, execution, provider, live, started,
        );
    });
    let detail = state
        .jobs
        .lock()
        .map_err(|e| e.to_string())?
        .investigation(&id)
        .map_err(|e| e.to_string())?;
    emit_changed(&app, &detail);
    Ok(detail.summary)
}

#[tauri::command]
pub(crate) async fn list_investigations(
    cursor: Option<String>,
    app: tauri::AppHandle,
) -> Result<InvestigationPage, String> {
    crate::off_ui_thread(move || {
        app.state::<AppState>()
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .investigation_history(None, cursor.as_deref())
            .map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub(crate) async fn get_investigation(
    investigation_id: String,
    app: tauri::AppHandle,
) -> Result<InvestigationDetail, String> {
    crate::off_ui_thread(move || {
        app.state::<AppState>()
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .investigation(&investigation_id)
            .map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub(crate) async fn investigation_events(
    investigation_id: String,
    after_sequence: Option<u64>,
    app: tauri::AppHandle,
) -> Result<InvestigationEventPage, String> {
    crate::off_ui_thread(move || {
        app.state::<AppState>()
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .investigation_events(&investigation_id, after_sequence.unwrap_or(0))
            .map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub(crate) async fn investigation_result(
    investigation_id: String,
    app: tauri::AppHandle,
) -> Result<Option<InvestigationResult>, String> {
    crate::off_ui_thread(move || {
        app.state::<AppState>()
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .investigation_result(&investigation_id)
            .map_err(|e| e.to_string())
    })
    .await
}

fn pending(state: &AppState, id: &str) -> Result<Option<InvestigationConsent>, String> {
    let Some(live) = state.investigations.get(id).map_err(|e| e.to_string())? else {
        // A different or restarted process cannot reconstruct the raw preview.
        return Ok(None);
    };
    let preview = live
        .pending
        .lock()
        .map_err(|_| HostError::Operational.to_string())?
        .clone();
    let Some(preview) = preview else {
        return Ok(None);
    };
    let durable = state
        .jobs
        .lock()
        .map_err(|e| e.to_string())?
        .investigation_pending_step(id)
        .map_err(|e| e.to_string())?;
    Ok(durable
        .filter(|(revision, step, approved)| {
            !approved
                && *revision == preview.revision
                && id == preview.investigation_id
                && step.step_id == preview.step_id
                && step.payload_hash == preview.preview.payload_hash
        })
        .map(|_| preview))
}

#[tauri::command]
pub(crate) async fn investigation_consent(
    investigation_id: String,
    app: tauri::AppHandle,
) -> Result<Option<InvestigationConsent>, String> {
    crate::off_ui_thread(move || pending(&app.state::<AppState>(), &investigation_id)).await
}

fn consent_decision<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    id: &str,
    step_id: &str,
    revision: u64,
    payload_hash: &str,
    approved: bool,
) -> Result<InvestigationSummary, String> {
    let state = app.state::<AppState>();
    let preview = pending(&state, id)?.ok_or_else(|| {
        "This exact step preview is no longer available. Reload the investigation.".to_string()
    })?;
    if preview.step_id != step_id
        || preview.revision != revision
        || preview.preview.payload_hash != payload_hash
    {
        return Err("The investigation step changed. Reload its current preview.".into());
    }
    let detail = {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        if approved {
            jobs.approve_investigation(id, revision, step_id, payload_hash)
        } else {
            jobs.decline_investigation(id, revision, step_id, payload_hash)
        }
        .map_err(|e| e.to_string())?
    };
    state.investigations.wake(id);
    emit_changed(app, &detail);
    Ok(detail.summary)
}

#[tauri::command]
pub(crate) async fn approve_investigation_step(
    investigation_id: String,
    step_id: String,
    revision: u64,
    payload_hash: String,
    app: tauri::AppHandle,
) -> Result<InvestigationSummary, String> {
    crate::off_ui_thread(move || {
        consent_decision(
            &app,
            &investigation_id,
            &step_id,
            revision,
            &payload_hash,
            true,
        )
    })
    .await
}

#[tauri::command]
pub(crate) async fn decline_investigation_step(
    investigation_id: String,
    step_id: String,
    revision: u64,
    payload_hash: String,
    app: tauri::AppHandle,
) -> Result<InvestigationSummary, String> {
    crate::off_ui_thread(move || {
        consent_decision(
            &app,
            &investigation_id,
            &step_id,
            revision,
            &payload_hash,
            false,
        )
    })
    .await
}

#[tauri::command]
pub(crate) async fn cancel_investigation(
    investigation_id: String,
    app: tauri::AppHandle,
) -> Result<InvestigationSummary, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<AppState>();
        let detail = state
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .cancel_investigation(&investigation_id)
            .map_err(|e| e.to_string())?;
        state.investigations.wake(&investigation_id);
        emit_changed(&app, &detail);
        Ok(detail.summary)
    })
    .await
}

#[tauri::command]
pub(crate) async fn read_investigation_citation(
    investigation_id: String,
    citation_id: String,
    app: tauri::AppHandle,
) -> Result<InvestigationCitationRead, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<AppState>();
        let ledger = state
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .investigation_ledger(&investigation_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "This investigation has no admitted citations.".to_string())?;
        history::read(&state, &investigation_id, &ledger, &citation_id).map_err(|e| e.to_string())
    })
    .await
}
