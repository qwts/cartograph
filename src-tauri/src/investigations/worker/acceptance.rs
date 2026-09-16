//! Manual real-provider evidence, not a scripted quality score or native UI test.
//! The independent oracle is deliberately absent from all request construction.

use super::*;
use std::collections::BTreeSet;
use std::path::Path;

fn required(name: &str) -> String {
    let value = std::env::var(name).unwrap_or_else(|_| panic!("Set {name}; see MT-H4-02"));
    assert!(
        !value.trim().is_empty() && value.len() <= 4096,
        "Invalid {name}"
    );
    value
}

fn write_report(directory: &Path, report: &Value) {
    std::fs::write(
        directory.join("run.json"),
        serde_json::to_vec_pretty(report).unwrap(),
    )
    .unwrap();
}

fn source_revision() -> String {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(args)
            .output()
            .expect("Git is required to record the tested source revision")
    };
    let status = git(&["status", "--porcelain"]);
    assert!(
        status.status.success() && status.stdout.is_empty(),
        "Real-provider acceptance requires a clean checkout so the source revision identifies the tested code"
    );
    let revision = git(&["rev-parse", "--verify", "HEAD"]);
    assert!(revision.status.success());
    let revision = String::from_utf8(revision.stdout)
        .unwrap()
        .trim()
        .to_owned();
    assert!(revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()));
    revision
}

fn ordered_events(jobs: &crate::jobs::JobStore, id: &str) -> Vec<InvestigationEvent> {
    let mut items = Vec::new();
    let mut after = 0;
    loop {
        let page = jobs.investigation_events(id, after).unwrap();
        items.extend(page.items);
        assert!(items.len() <= 96, "Event journal exceeded the contract");
        if !page.has_more {
            return items;
        }
        assert!(
            page.next_sequence > after,
            "Event pagination did not advance"
        );
        after = page.next_sequence;
    }
}

/// Read the actual durable records even on a failed/unknown model invocation.
/// Source text stays in the retained capture; the report includes read status and
/// hash, not transient prompts, raw model replies or source excerpts.
fn task_evidence(fixture: &Fixture, id: &str, needs_history: bool) -> (Value, bool) {
    let state = fixture.app.state::<AppState>();
    let (detail, result, ledger, events) = {
        let jobs = state.jobs.lock().unwrap();
        (
            jobs.investigation(id).unwrap(),
            jobs.investigation_result(id).unwrap(),
            jobs.investigation_ledger(id).unwrap(),
            ordered_events(&jobs, id),
        )
    };
    let ordered = events.first().is_some_and(|event| event.sequence == 1)
        && events
            .windows(2)
            .all(|pair| pair[0].sequence + 1 == pair[1].sequence)
        && events
            .last()
            .is_some_and(|event| event.sequence == detail.summary.last_event_sequence);
    let tool_completed = |tool| {
        events.iter().any(|event| {
            event.kind == InvestigationEventKind::ToolCompleted && event.tool == Some(tool)
        })
    };
    let queried = tool_completed(InvestigationTool::QueryContext);
    let read = tool_completed(InvestigationTool::ReadEvidence);
    let saved_finish = events
        .iter()
        .any(|event| event.kind == InvestigationEventKind::ResultPersisted);
    let weak = result.as_ref().is_some_and(|result| {
        !result.findings.is_empty()
            && result.findings.iter().all(|finding| {
                finding.tier == Tier::Agentic
                    && finding.confidence_tier == ConfidenceTier::InferredWeak
            })
    });
    let history = ledger.as_ref().is_some_and(|ledger| {
        ledger.supplied_history_hash.is_some() && ledger.supplied_history_bytes > 0
    });
    let mut captured_reads = Vec::new();
    let mut captured_available = true;
    if let (Some(result), Some(ledger)) = (&result, &ledger) {
        let cited = result
            .findings
            .iter()
            .flat_map(|finding| &finding.citation_ids)
            .collect::<BTreeSet<_>>();
        for citation in &ledger.citations {
            if !cited.contains(&citation.citation_id)
                || !matches!(
                    citation.origin,
                    InvestigationEvidenceOrigin::CapturedPrimarySource { .. }
                )
            {
                continue;
            }
            let read =
                crate::investigations::history::read(&state, id, ledger, &citation.citation_id)
                    .unwrap();
            let text_hash = read.text.as_ref().map(|text| content_hash(text.as_bytes()));
            captured_available &= read.status == InvestigationCitationStatus::Available
                && text_hash.is_some()
                && text_hash == citation.text_hash;
            captured_reads.push(json!({
                "citation_id": citation.citation_id,
                "status": read.status,
                "observed_text_hash": text_hash,
            }));
        }
    }
    captured_available &= !captured_reads.is_empty();
    // This is a fresh database connection, not a claim that the native app was
    // restarted. Full restart/forgetting and semantic review remain manual.
    let reopened = crate::jobs::JobStore::open(fixture.app_data.join("state.db")).unwrap();
    let reopen_equal = reopened.investigation(id).unwrap() == detail
        && reopened.investigation_result(id).unwrap() == result
        && reopened.investigation_ledger(id).unwrap() == ledger
        && ordered_events(&reopened, id) == events;
    let graph_unchanged =
        serde_json::to_string(&state.graph.lock().unwrap().read_snapshot().unwrap()).unwrap()
            == fixture.graph_before;
    let checks = json!({
        "completed": detail.summary.status == InvestigationStatus::Completed,
        "events_ordered": ordered,
        "query_completed": queried,
        "read_completed": read,
        "finish_persisted": saved_finish,
        "findings_t3_weak": weak,
        "captured_citations_available_without_checkout": captured_available,
        "coordinator_reopen_equal": reopen_equal,
        "graph_unchanged": graph_unchanged,
        "parent_history_supplied_when_required": !needs_history || history,
    });
    let passed = checks
        .as_object()
        .unwrap()
        .values()
        .all(|check| check == &Value::Bool(true));
    (
        json!({
            "detail": detail, "result": result, "ledger": ledger, "events": events,
            "captured_citation_reads": captured_reads, "checks": checks,
            "mechanical_pass": passed,
            "independent_citation_review": "pending",
        }),
        passed,
    )
}

fn auditor(fixture: &Fixture, provider: Arc<dyn LlmProvider>) -> (String, u128) {
    let state = fixture.app.state::<AppState>();
    let parent = state
        .jobs
        .lock()
        .unwrap()
        .investigation(&fixture.id)
        .unwrap();
    let request = StartInvestigationRequest {
        request_nonce: "local-acceptance-auditor".into(),
        specialist_id: SpecialistId::EvidenceAuditor,
        question: "Audit the selected parent's findings against available context and retained evidence. Query context, inspect source, and explain unsupported claims and scope limits.".into(),
        conversation_id: Some(parent.summary.conversation_id),
        parent_id: Some(fixture.id.clone()),
        ..fixture.request.clone()
    };
    let start_time = Instant::now();
    let started = state
        .jobs
        .lock()
        .unwrap()
        .start_investigation(
            &request,
            Some(&descriptor(provider.as_ref(), InvestigationProviderMode::Local).unwrap()),
        )
        .unwrap();
    let start_milliseconds = start_time.elapsed().as_millis();
    let id = started.detail.summary.investigation_id;
    let plan = state
        .jobs
        .lock()
        .unwrap()
        .claim_plan(
            started.detail.summary.job_id,
            crate::jobs::ClaimMode::StartQueued,
        )
        .unwrap();
    let reservation = state
        .job_execution_locks
        .try_reserve(plan.lock_target())
        .unwrap();
    let (_, execution) = state
        .jobs
        .lock()
        .unwrap()
        .claim_execution(&plan, reservation)
        .unwrap();
    let live = state.investigations.insert(&id).unwrap();
    run(
        fixture.app.handle().clone(),
        id.clone(),
        request,
        execution,
        provider,
        live,
        Instant::now(),
    );
    (id, start_milliseconds)
}

#[test]
#[ignore = "MT-H4-02: requires an installed local model and explicit fresh artifact directory"]
fn investigation_local_provider_acceptance() {
    // AC-0191: a real provider chooses every action through the production loop.
    // Compiling this test or passing its mechanical checks does not pass H4/H5.
    let url = required("CARTOGRAPH_ACCEPTANCE_URL");
    let model = required("CARTOGRAPH_ACCEPTANCE_MODEL");
    let digest = required("CARTOGRAPH_ACCEPTANCE_MODEL_DIGEST");
    let runtime = required("CARTOGRAPH_ACCEPTANCE_RUNTIME");
    let output = PathBuf::from(required("CARTOGRAPH_ACCEPTANCE_OUTPUT"));
    let source_revision = source_revision();
    assert!(
        output.is_absolute(),
        "The artifact directory must be absolute"
    );
    std::fs::create_dir(&output)
        .expect("The artifact directory must be new, with an existing parent");
    let mut report = json!({
        "schema_version": 1,
        "procedure": "MT-H4-02 production-module local-provider harness",
        "application_version": env!("CARGO_PKG_VERSION"),
        "source_revision": source_revision,
        "phase": "preparation",
        "mechanical_pass": false,
        "operator_supplied_identity": {
            "endpoint": url, "requested_model": model, "model_digest": digest, "runtime": runtime,
            "independently_verified": false,
        },
        "fixture": { "path": "rule.ts", "content_hash": content_hash(ORIGINAL.as_bytes()), "byte_len": ORIGINAL.len() },
        "independent_citation_review": "pending",
        "native_restart_check": "pending",
        "destructive_forgetting_check": "pending",
        "delivery_gate_pass": false,
    });
    write_report(&output, &report);
    let provider =
        Arc::new(llm::OllamaProvider::new(&url, &model, Duration::from_secs(180)).unwrap());
    // This calls the real bounded Ollama implementation. No mock, retry wrapper,
    // cloud fallback, shell or model download is installed by the harness.
    let mut directory = tempfile::tempdir_in(&output).unwrap();
    directory.disable_cleanup(true);
    report["retained_fixture_directory"] = json!(directory.path());
    write_report(&output, &report);
    let fixture = Fixture::with_question(
        provider.as_ref(),
        directory,
        "Inspect the available guard evidence and its limits. Query context, inspect retained source, and explain only behavior supported by citations.",
    );
    std::fs::write(output.join("rule.ts"), ORIGINAL).unwrap();
    report["analyst_id"] = json!(fixture.id);
    report["analyst_coordinator_start_milliseconds"] = json!(fixture.start_milliseconds);
    report["phase"] = json!("analyst_running");
    write_report(&output, &report);
    fixture.run(provider.clone());
    let (analyst, analyst_pass) = task_evidence(&fixture, &fixture.id, false);
    report["analyst"] = analyst;
    report["phase"] = json!("analyst_recorded");
    write_report(&output, &report);
    let mut auditor_pass = false;
    if analyst_pass {
        report["phase"] = json!("auditor_running");
        write_report(&output, &report);
        let (id, start_milliseconds) = auditor(&fixture, provider);
        let (evidence, passed) = task_evidence(&fixture, &id, true);
        report["auditor_id"] = json!(id);
        report["auditor_coordinator_start_milliseconds"] = json!(start_milliseconds);
        report["auditor"] = evidence;
        auditor_pass = passed;
    } else {
        report["auditor"] = json!({ "status": "skipped_after_failed_analyst_mechanical_checks" });
    }
    report["phase"] = json!("recorded");
    report["mechanical_pass"] = json!(analyst_pass && auditor_pass);
    write_report(&output, &report);
    assert!(
        analyst_pass && auditor_pass,
        "Real-provider mechanical acceptance failed; inspect run.json and retained state. No repair/replay was attempted."
    );
}

#[test]
fn investigation_acceptance_report_preserves_failed_and_unknown_outcomes() {
    // AC-0191: report calibration only; these scripted cases cannot replace the
    // ignored real-provider procedure or its independent citation review.
    for (mode, status, expected_pass) in [
        (Mode::Finish, InvestigationStatus::Completed, true),
        (Mode::Malformed, InvestigationStatus::Failed, false),
        (Mode::Unknown, InvestigationStatus::OutcomeUnknown, false),
    ] {
        let provider = Arc::new(Scripted::new(mode));
        let fixture = Fixture::new(provider.as_ref());
        fixture.run(provider);
        let (report, passed) = task_evidence(&fixture, &fixture.id, false);
        assert_eq!(passed, expected_pass);
        assert_eq!(report["detail"]["status"], json!(status));
        assert_eq!(report["independent_citation_review"], "pending");
        if matches!(mode, Mode::Unknown) {
            assert_eq!(
                report["detail"]["usage"]["reported_input_tokens"],
                Value::Null
            );
            assert_eq!(report["detail"]["usage"]["model_invocations"], 1);
        }
        let artifacts = tempfile::tempdir().unwrap();
        write_report(artifacts.path(), &report);
        let saved: Value =
            serde_json::from_slice(&std::fs::read(artifacts.path().join("run.json")).unwrap())
                .unwrap();
        assert_eq!(saved, report);
    }
}
