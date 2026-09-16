use super::*;
use adapters_lang_ts::captured;
use core_prov::{ConfidenceTier, Tier};
use llm::bounded::{AuthorizedBoundedCompletion, ProviderProfile, ReportedUsage};
use llm::{Embedding, ProviderCaps, ProviderError};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

const ORIGINAL: &str = "// private trailing-source marker is never output\nexport function enabled(ok: boolean) { const limited = ok === false; if (limited) return false; }\n";

#[derive(Clone, Copy)]
enum Mode {
    Finish,
    CancelFinish,
    CancelTool,
    Malformed,
    Unknown,
}

/// One deterministic provider action per actual host invocation. The fixture
/// learns exact fact/citation/range IDs only from its admitted prompt; no
/// independent expected answer is placed in that prompt or target source.
struct Scripted {
    calls: AtomicUsize,
    mode: Mode,
    cancel: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    copied: Mutex<Vec<String>>,
}
impl Scripted {
    fn new(mode: Mode) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            mode,
            cancel: Mutex::new(None),
            copied: Mutex::new(vec![]),
        }
    }
}
impl LlmProvider for Scripted {
    fn id(&self) -> &str {
        "local-scripted"
    }
    fn locality(&self) -> Locality {
        Locality::Local
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: false,
            chat: true,
            tool_use: false,
        }
    }
    fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        Err(ProviderError::Unsupported("fixture embeddings"))
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        Ok(ProviderProfile {
            protocol_version: llm::bounded::PROTOCOL_VERSION.into(),
            provider_id: self.id().into(),
            locality: Locality::Local,
            endpoint_id: "http://127.0.0.1:11434/api/chat".into(),
            requested_model: "scripted-model".into(),
        })
    }
    fn complete_bounded(
        &self,
        request: &AuthorizedBoundedCompletion,
        control: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        assert_eq!((control.check)(), CallDirective::Continue);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if matches!(self.mode, Mode::Unknown) {
            return Err(BoundedCallError {
                code: FailureCode::Transport,
                outcome: InvocationOutcome::Unknown,
            });
        }
        let prompt: Value = serde_json::from_str(&request.payload().prompt).unwrap();
        let action = if call == 0 {
            json!({ "type": "query_context", "query": { "scope": {"type":"all"}, "kind":"node",
                "labels":["BusinessRule"], "max_facts":1, "max_bytes":32768, "cursor":null } })
        } else if call == 1 {
            let option = &prompt["context_pages"][0]["evidence_options"][0];
            let range = option["ranges"]
                .as_array()
                .unwrap()
                .iter()
                .find(|range| range["role"] == "definition_initializer")
                .unwrap();
            json!({ "type":"read_evidence", "fact":option["fact"], "role":range["role"], "index":range["index"] })
        } else {
            assert_eq!(call, 2, "no hidden repair, retry or extra model step");
            let citations = prompt["input_ledger"]["citations"].as_array().unwrap();
            let citation = citations
                .iter()
                .find(|citation| citation["origin"]["kind"] == "captured_primary_source")
                .unwrap();
            let span = request
                .payload()
                .spans
                .iter()
                .find(|span| span.id == citation["citation_id"])
                .unwrap();
            self.copied.lock().unwrap().push(span.text.clone());
            json!({ "type":"finish", "findings":[{ "claim_kind":"inferred_interpretation",
                "title":"A guarded path was observed", "statement":"The inspected callable can exit conditionally; external policy remains unresolved.",
                "citation_ids":[citation["citation_id"]], "limitations":["Runtime inputs and broader feature coverage remain unknown."] }],
                "knowledge_completeness":"partial", "limitations":["This scoped result establishes no complete business policy."] })
        };
        if (matches!(self.mode, Mode::CancelFinish) && call == 2)
            || (matches!(self.mode, Mode::CancelTool) && call == 1)
        {
            self.cancel.lock().unwrap().as_ref().unwrap()();
            // The response was already in flight: emulate its valid return
            // after cancellation instead of fabricating remote termination.
        }
        let text = if matches!(self.mode, Mode::Malformed) {
            "```json\n{}\n```".into()
        } else {
            serde_json::to_string(&action).unwrap()
        };
        Ok(BoundedCompletion {
            text,
            requested_model: "scripted-model".into(),
            response_model: Some("scripted-model-observed".into()),
            provider_request_id: None,
            usage: Some(ReportedUsage {
                input_tokens: Some(100),
                output_tokens: Some(40),
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            }),
            request_bytes: 100,
            response_bytes: 100,
        })
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    app_data: PathBuf,
    app: tauri::App<tauri::test::MockRuntime>,
    request: StartInvestigationRequest,
    id: String,
    execution: JobExecution,
    live: Arc<LiveTask>,
    graph_before: String,
    start_milliseconds: u128,
}

impl Fixture {
    fn new(provider: &dyn LlmProvider) -> Self {
        Self::with_question(
            provider,
            tempfile::tempdir().unwrap(),
            "Inspect the available guard evidence and its limits.",
        )
    }

    fn with_question(
        provider: &dyn LlmProvider,
        directory: tempfile::TempDir,
        question: &str,
    ) -> Self {
        let app_data = directory.path().join("private");
        std::fs::create_dir(&app_data).unwrap();
        let app_data = crate::paths::canonicalize(app_data).unwrap();
        let target = directory.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("rule.ts"), ORIGINAL).unwrap();
        let state = crate::registered_source_tests::app_state(&app_data);
        let source = state
            .sources
            .lock()
            .unwrap()
            .register_local(&target)
            .unwrap();
        let input = state
            .primary_sources
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
        state
            .primary_sources
            .persist(&input, &source, &receipts)
            .unwrap();
        let bindings = crate::primary_source::matching_bindings(&extraction, &receipts);
        assert!(!bindings.is_empty());
        crate::load_into_graph_with_bindings(
            &mut state.graph.lock().unwrap(),
            &extraction,
            &source.repo_key,
            source.root(),
            "workdir",
            &bindings,
        )
        .unwrap();
        drop(input);
        // Every later read must use retained parser bytes, not this checkout.
        std::fs::remove_dir_all(target).unwrap();
        let graph_before =
            serde_json::to_string(&state.graph.lock().unwrap().read_snapshot().unwrap()).unwrap();
        let request = StartInvestigationRequest {
            schema_version: 1,
            request_nonce: "worker-fixture".into(),
            specialist_id: SpecialistId::DomainAnalyst,
            question: question.into(),
            scope: InvestigationScope::All,
            provider_mode: InvestigationProviderMode::Local,
            limit_profile: "investigation-v1".into(),
            expected_graph_revision: None,
            conversation_id: None,
            parent_id: None,
        };
        let start_time = Instant::now();
        let started = state
            .jobs
            .lock()
            .unwrap()
            .start_investigation(
                &request,
                Some(&descriptor(provider, InvestigationProviderMode::Local).unwrap()),
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
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(state);
        Self {
            _directory: directory,
            app_data,
            app,
            request,
            id,
            execution,
            live,
            graph_before,
            start_milliseconds,
        }
    }

    fn run(&self, provider: Arc<dyn LlmProvider>) {
        run(
            self.app.handle().clone(),
            self.id.clone(),
            self.request.clone(),
            self.execution.clone(),
            provider,
            self.live.clone(),
            Instant::now(),
        );
    }

    fn cancel_inside_call(&self, provider: &Scripted, clear_job: bool) {
        let app = self.app.handle().clone();
        let id = self.id.clone();
        *provider.cancel.lock().unwrap() = Some(Box::new(move || {
            let state = app.state::<AppState>();
            let mut jobs = state.jobs.lock().unwrap();
            assert!(jobs.investigation(&id).unwrap().summary.invocation_pending);
            jobs.cancel_investigation(&id).unwrap();
            if clear_job {
                jobs.clear_finished().unwrap();
            }
        }));
    }
}

#[test]
fn investigation_query_pages_include_evidence_inventory_and_preserve_continuation() {
    // AC-0182: the documented query must page the complete host response, not
    // fail because its core page left no room for retained-source inventories.
    struct Sink {
        usage: InvestigationUsage,
        publications: usize,
    }
    impl AcquisitionSink for Sink {
        fn usage(&self) -> &InvestigationUsage {
            &self.usage
        }
        fn reserve_validation(&mut self, _: u64) -> Result<(), HostError> {
            panic!("query metadata must not read source bytes")
        }
        fn publish_input(
            &mut self,
            ledger: &InvestigationInputLedger,
            usage: InvestigationUsage,
        ) -> Result<(), HostError> {
            ledger.validate()?;
            self.usage = usage;
            self.publications += 1;
            Ok(())
        }
    }
    let provider = Scripted::new(Mode::Finish);
    let fixture = Fixture::new(&provider);
    let state = fixture.app.state::<AppState>();
    let mut context = FrozenContext::prepare(&state, &fixture.request, &[]).unwrap();
    let expected = context
        .snapshot
        .query(context_hub::QueryRequest {
            max_facts: 32,
            max_bytes: 32768,
            ..Default::default()
        })
        .unwrap();
    let mut query = InvestigationQuery {
        scope: InvestigationScope::All,
        kind: None,
        labels: vec![],
        max_facts: 12,
        max_bytes: 16384,
        cursor: None,
    };
    let mut sink = Sink {
        usage: InvestigationUsage::default(),
        publications: 0,
    };
    let mut seen = Vec::new();
    loop {
        context.query(&state, query.clone(), &mut sink).unwrap();
        let raw = context.query_pages.last().unwrap();
        assert!(raw.len() <= query.max_bytes);
        let wrapper: Value = serde_json::from_str(raw).unwrap();
        let page: context_hub::QueryResponse =
            serde_json::from_value(wrapper["context"].clone()).unwrap();
        assert_eq!(
            page.facts.len(),
            wrapper["evidence_options"].as_array().unwrap().len()
        );
        if sink.publications == 1 {
            assert!(page.facts.len() < expected.facts.len());
            assert!(page.next_cursor.is_some());
        }
        seen.extend(page.facts);
        assert_eq!(context.ledger.selected_facts.len(), seen.len());
        assert_eq!(context.ledger.citations.len(), seen.len());
        let bindings = context
            .ledger
            .selected_facts
            .iter()
            .filter_map(|s| s.binding.as_ref())
            .map(|b| (&b.repo_key, &b.receipt_id))
            .collect::<std::collections::BTreeSet<_>>();
        let indexed = context
            .ledger
            .receipt_references
            .iter()
            .map(|r| (&r.repo_key, &r.receipt_id))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            indexed, bindings,
            "an omitted fact must not retain its receipt"
        );
        query.cursor = page.next_cursor.map(|c| InvestigationQueryCursor {
            snapshot_id: c.snapshot_id,
            selection_id: c.selection_id,
            offset: c.offset,
        });
        if query.cursor.is_none() {
            break;
        }
        assert!(sink.publications < 8);
    }
    assert_eq!(
        seen, expected.facts,
        "paging may neither skip nor duplicate facts"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);

    // One core fact fits 6000 bytes; its complete evidence inventory does not.
    // Keep failure explicit, without silently skipping it or changing history.
    let before = context.ledger.clone();
    let publications = sink.publications;
    let pages = context.query_pages.len();
    query = InvestigationQuery {
        scope: InvestigationScope::All,
        kind: Some(InvestigationFactKind::Node),
        labels: vec!["BusinessRule".into()],
        max_facts: 1,
        max_bytes: 6000,
        cursor: None,
    };
    assert_eq!(
        context.query(&state, query, &mut sink),
        Err(HostError::LimitExceeded)
    );
    assert_eq!(context.ledger, before);
    assert_eq!(context.query_pages.len(), pages);
    assert_eq!(sink.publications, publications);
}

#[test]
fn investigation_worker_queries_reads_captured_definition_and_persists_cited_finish() {
    // AC-0182/0183/0184/0188/0191: actual parser receipts, graph selection,
    // source retention, provider input and durable coordinator loop together.
    let provider = Arc::new(Scripted::new(Mode::Finish));
    let fixture = Fixture::new(provider.as_ref());
    fixture.run(provider.clone());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    assert_eq!(*provider.copied.lock().unwrap(), vec!["ok === false"]);
    let state = fixture.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&fixture.id).unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Completed);
    assert_eq!(detail.usage.model_invocations, 3);
    assert_eq!(detail.usage.tool_actions, 2);
    assert_eq!(detail.usage.evidence_requests, 1);
    assert_eq!(
        detail.usage.captured_validation_bytes,
        ORIGINAL.len() as u64
    );
    assert_eq!(detail.usage.reported_input_tokens, Some(300));
    let result = jobs.investigation_result(&fixture.id).unwrap().unwrap();
    assert_eq!(result.findings[0].tier, Tier::Agentic);
    assert_eq!(
        result.findings[0].confidence_tier,
        ConfidenceTier::InferredWeak
    );
    let ledger = jobs.investigation_ledger(&fixture.id).unwrap().unwrap();
    let citation = ledger
        .citations
        .iter()
        .find(|citation| citation.citation_id == result.findings[0].citation_ids[0])
        .unwrap();
    assert_eq!(
        citation.role,
        Some(agents::TaskRangeRole::DefinitionInitializer)
    );
    assert!(matches!(
        citation.origin,
        InvestigationEvidenceOrigin::CapturedPrimarySource { .. }
    ));
    let events = jobs.investigation_events(&fixture.id, 0).unwrap().items;
    let reserve = events
        .iter()
        .position(|event| event.kind == InvestigationEventKind::EvidenceValidationReserved)
        .unwrap();
    let read_done = events
        .iter()
        .position(|event| {
            event.kind == InvestigationEventKind::ToolCompleted
                && event.tool == Some(InvestigationTool::ReadEvidence)
        })
        .unwrap();
    assert!(reserve < read_done);
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].sequence + 1 == pair[1].sequence)
    );
    drop(jobs);
    assert_eq!(
        serde_json::to_string(&state.graph.lock().unwrap().read_snapshot().unwrap()).unwrap(),
        fixture.graph_before
    );
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("ok === false")
    );
    let reopened = crate::jobs::JobStore::open(fixture.app_data.join("state.db")).unwrap();
    assert_eq!(
        reopened.investigation_result(&fixture.id).unwrap(),
        Some(result)
    );
    assert!(state.investigations.get(&fixture.id).unwrap().is_none());
}

#[test]
fn investigation_worker_retains_late_finish_after_cancellation_and_job_cleanup() {
    // AC-0187/0191: the actual call returns after cancellation and deletion;
    // only its finish is admitted, against its unchanged original ledger.
    let provider = Arc::new(Scripted::new(Mode::CancelFinish));
    let fixture = Fixture::new(provider.as_ref());
    fixture.cancel_inside_call(provider.as_ref(), true);
    fixture.run(provider.clone());
    let state = fixture.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&fixture.id).unwrap();
    assert!(detail.summary.cancel_requested);
    assert!(detail.summary.has_result);
    assert!(!detail.summary.invocation_pending);
    assert!(jobs.get(detail.summary.job_id).is_err());
    assert!(jobs.investigation_result(&fixture.id).unwrap().is_some());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    assert_eq!(detail.usage.tool_actions, 2);
}

#[test]
fn investigation_worker_never_executes_a_late_tool_after_cancellation() {
    // AC-0187: a tool returned by an already-issued call is not new permission.
    let provider = Arc::new(Scripted::new(Mode::CancelTool));
    let fixture = Fixture::new(provider.as_ref());
    fixture.cancel_inside_call(provider.as_ref(), false);
    fixture.run(provider.clone());
    let state = fixture.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&fixture.id).unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Cancelled);
    assert_eq!(detail.usage.tool_actions, 1);
    assert_eq!(detail.usage.evidence_requests, 0);
    assert_eq!(detail.usage.evidence_items, 0);
    assert!(!detail.summary.has_result);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert!(provider.copied.lock().unwrap().is_empty());
}

#[test]
fn investigation_worker_rejects_malformed_actions_without_model_repair() {
    // AC-0184/0185: invalid complete output does not buy another invocation.
    let provider = Arc::new(Scripted::new(Mode::Malformed));
    let fixture = Fixture::new(provider.as_ref());
    fixture.run(provider.clone());
    let state = fixture.app.state::<AppState>();
    let detail = state
        .jobs
        .lock()
        .unwrap()
        .investigation(&fixture.id)
        .unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Failed);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(detail.usage.tool_actions, 0);
    assert!(!detail.summary.has_result);
}

#[test]
fn investigation_worker_records_unknown_transport_outcome_without_replay() {
    // AC-0185/0187: reservations survive unknown transport, with absent usage.
    let provider = Arc::new(Scripted::new(Mode::Unknown));
    let fixture = Fixture::new(provider.as_ref());
    fixture.run(provider.clone());
    let state = fixture.app.state::<AppState>();
    let detail = state
        .jobs
        .lock()
        .unwrap()
        .investigation(&fixture.id)
        .unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::OutcomeUnknown);
    assert_eq!(detail.usage.model_invocations, 1);
    assert_eq!(detail.usage.generated_token_reservations, 2048);
    assert_eq!(detail.usage.reported_input_tokens, None);
    assert!(!detail.summary.has_result);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[path = "acceptance.rs"]
mod acceptance;
