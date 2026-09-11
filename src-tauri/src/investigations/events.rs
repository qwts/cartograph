use crate::AppState;
use agents::investigation::InvestigationDetail;
use serde::Serialize;
use tauri::{Emitter, Manager};

/// Invalidation only: event pages and status reads remain the durable authority.
pub(crate) fn emit_changed<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    detail: &InvestigationDetail,
) {
    #[derive(Serialize)]
    struct Invalidation<'a> {
        investigation_id: &'a str,
        revision: u64,
        last_event_sequence: u64,
    }
    let summary = &detail.summary;
    let _ = app.emit(
        "investigation://changed",
        Invalidation {
            investigation_id: &summary.investigation_id,
            revision: summary.revision,
            last_event_sequence: summary.last_event_sequence,
        },
    );
    // Job cleanup is allowed; an absent row must not erase investigation history
    // or turn successful durable result admission into a failure.
    let state = app.state::<AppState>();
    let job = state
        .jobs
        .lock()
        .ok()
        .and_then(|jobs| jobs.get(summary.job_id).ok());
    if let Some(job) = job {
        crate::emit_job(app, &job);
    }
}
