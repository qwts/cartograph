use super::*;
use crate::anthropic::{AnthropicProvider, ClaudeLane};
use crate::{AnalysisTier, EgressPolicy, OllamaProvider, PayloadSpan};
use serde_json::{Value, json};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};

struct FramedRequest {
    headers: String,
    body: Vec<u8>,
}

// Read the actual complete HTTP request, not whatever one TCP read happens to
// return. Fixtures reject unexpected framing and cap both headers and body.
fn read_request(stream: &mut impl Read) -> io::Result<FramedRequest> {
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        if headers.len() == 16 * 1024 {
            return Err(io::Error::other("fixture headers too large"));
        }
        let mut byte = [0_u8];
        stream.read_exact(&mut byte)?;
        headers.push(byte[0]);
    }
    let headers = String::from_utf8(headers).map_err(io::Error::other)?;
    let length = headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>())
        })
        .ok_or_else(|| io::Error::other("fixture missing content length"))?
        .map_err(io::Error::other)?;
    if length > 512 * 1024 {
        return Err(io::Error::other("fixture request too large"));
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body)?;
    Ok(FramedRequest { headers, body })
}

fn set_timeouts(stream: &TcpStream) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
}

fn framed(body: &[u8]) -> Vec<u8> {
    let mut response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
    response.extend_from_slice(body);
    response
}

fn http(response: Vec<u8>) -> (String, JoinHandle<FramedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        set_timeouts(&stream);
        let request = read_request(&mut stream).unwrap();
        // Limit failures intentionally close the socket before its full body.
        let _ = stream.write_all(&response);
        request
    });
    (format!("http://{address}/"), server)
}

fn ollama_body(text: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "model":"test-model", "message":{"role":"assistant", "content":text},
        "done":true, "done_reason":"stop", "prompt_eval_count":11, "eval_count":7,
    }))
    .unwrap()
}

fn anthropic_body(text: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "type":"message", "role":"assistant", "model":ClaudeLane::Opus.model_id(), "id":"msg_fixture",
        "content":[{"type":"text", "text":text}], "stop_reason":"end_turn",
        "usage":{"input_tokens":23,"output_tokens":7,"cache_read_input_tokens":2,
            "cache_creation_input_tokens":3,"service_tier":"standard"}
    })).unwrap()
}

fn action() -> CompletionAction {
    CompletionAction {
        action_id: "investigation-1:step-1".into(),
        tier: AnalysisTier::Agentic,
        payload: CompletionPayload {
            system: "Return exactly one JSON action.".into(),
            prompt: "Investigate the selected rules.".into(),
            spans: vec![PayloadSpan {
                id: "evidence-1".into(),
                repo: "src_fixture".into(),
                path: "src/rules.ts".into(),
                byte_start: 10,
                byte_end: 40,
                commit_sha: "captured".into(),
                text: "if (blocked) return false;".into(),
            }],
        },
    }
}

fn authorize(provider: &dyn LlmProvider, limits: CompletionLimits) -> AuthorizedBoundedCompletion {
    let firewall = EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Agentic]));
    let prepared = firewall
        .prepare_bounded(provider, &action(), &limits)
        .unwrap();
    let grant = ConsentGrant::from_preview(&prepared.preview().egress);
    firewall
        .authorize_bounded(provider, &prepared, Some(&grant))
        .unwrap()
}

fn proceed() -> CallDirective {
    CallDirective::Continue
}
fn control() -> CompletionControl<'static> {
    CompletionControl {
        deadline: Instant::now() + Duration::from_secs(10),
        check: &proceed,
    }
}

fn assert_no_request(listener: &TcpListener) {
    listener.set_nonblocking(true).unwrap();
    assert!(
        listener
            .accept()
            .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock)
    );
}

// AC-0185: actual provider requests carry finite generation limits; complete
// response metadata remains distinct from caller reservations and body measures.
#[test]
fn bounded_providers_send_exact_generation_limits_and_consented_payload() {
    let values = CompletionLimitValues {
        max_output_tokens: 37,
        ..Default::default()
    };
    let limits = CompletionLimits::new(values).unwrap();
    let local_body = ollama_body(r#"{"action":"query_context"}"#);
    let (endpoint, server) = http(framed(&local_body));
    let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    let output = local
        .complete_bounded(&authorize(&local, limits), &control())
        .unwrap();
    let request = server.join().unwrap();
    let wire: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(wire["options"]["num_predict"], 37);
    assert_eq!(wire["format"], "json");
    assert_eq!(wire["stream"], false);
    assert!(
        wire["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("if (blocked) return false;")
    );
    assert!(
        request
            .headers
            .to_ascii_lowercase()
            .contains("accept-encoding: identity")
    );
    assert_eq!(output.request_bytes, request.body.len() as u64);
    assert_eq!(output.response_bytes, local_body.len() as u64);
    assert_eq!(output.usage.unwrap().output_tokens, Some(7));
    assert_eq!(output.response_model.as_deref(), Some("test-model"));

    let cloud_body = anthropic_body(r#"{"action":"finish"}"#);
    let (endpoint, server) = http(framed(&cloud_body));
    let cloud =
        AnthropicProvider::with_endpoint(ClaudeLane::Opus, "fixture-key", endpoint).unwrap();
    let output = cloud
        .complete_bounded(&authorize(&cloud, limits), &control())
        .unwrap();
    let request = server.join().unwrap();
    let wire: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(wire["max_tokens"], 37);
    assert_eq!(wire["model"], ClaudeLane::Opus.model_id());
    assert!(wire.get("fallbacks").is_none());
    assert!(wire.get("tools").is_none());
    assert!(
        request
            .headers
            .to_ascii_lowercase()
            .contains("anthropic-version: 2023-06-01")
    );
    assert_eq!(output.provider_request_id.as_deref(), Some("msg_fixture"));
    assert_eq!(output.request_bytes, request.body.len() as u64);
    assert_eq!(output.response_bytes, cloud_body.len() as u64);
    let usage = output.usage.unwrap();
    assert_eq!(usage.input_tokens, Some(23));
    assert_eq!(usage.cache_read_input_tokens, Some(2));
}

// AC-0185: cap the entire response entity, including ignored fields and chunked
// data, before JSON. Test exact boundary and multibyte content too.
#[test]
fn bounded_response_reader_enforces_body_cap_before_decode() {
    let valid = ollama_body("évidence");
    let cap = valid.len() as u64;
    let mut ignored: Value = serde_json::from_slice(&valid).unwrap();
    ignored["ignored"] = json!("x".repeat(4096));
    let large = serde_json::to_vec(&ignored).unwrap();
    let mut chunked =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
    chunked.extend_from_slice(format!("{:x}\r\n", large.len()).as_bytes());
    chunked.extend_from_slice(&large);
    chunked.extend_from_slice(b"\r\n0\r\n\r\n");
    let mut no_length = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
    no_length.extend_from_slice(&large);
    let declared = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        cap + 1
    )
    .into_bytes();
    for response in [framed(&large), chunked, no_length, declared] {
        let (endpoint, server) = http(response);
        let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
        let limits = CompletionLimits::new(CompletionLimitValues {
            response_bytes: cap,
            ..Default::default()
        })
        .unwrap();
        let error = local
            .complete_bounded(&authorize(&local, limits), &control())
            .unwrap_err();
        assert_eq!(error, rejected(FailureCode::ResponseTooLarge));
        server.join().unwrap();
    }
    let (endpoint, server) = http(framed(&valid));
    let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    let limits = CompletionLimits::new(CompletionLimitValues {
        response_bytes: cap,
        output_text_bytes: "évidence".len() as u64,
        ..Default::default()
    })
    .unwrap();
    let output = local
        .complete_bounded(&authorize(&local, limits), &control())
        .unwrap();
    assert_eq!(output.text, "évidence");
    assert_eq!(output.response_bytes, cap);
    server.join().unwrap();
}

// AC-0185: compression cannot turn a capped encoded body into uncapped decoded
// memory, even if another workspace crate enables a Reqwest compression feature.
#[test]
fn bounded_transport_rejects_content_encoding_and_oversized_action_text() {
    let mut encoded = b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 2\r\nConnection: close\r\n\r\n".to_vec();
    encoded.extend_from_slice(b"{}");
    let (endpoint, server) = http(encoded);
    let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    let error = local
        .complete_bounded(&authorize(&local, CompletionLimits::default()), &control())
        .unwrap_err();
    assert_eq!(error, rejected(FailureCode::UnsupportedEncoding));
    server.join().unwrap();
    let (endpoint, server) = http(framed(&ollama_body("éé")));
    let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    let limits = CompletionLimits::new(CompletionLimitValues {
        output_text_bytes: 3,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        local
            .complete_bounded(&authorize(&local, limits), &control())
            .unwrap_err(),
        rejected(FailureCode::OutputTooLarge)
    );
    server.join().unwrap();
}

// AC-0185: a syntactically valid JSON action is still unusable if the provider
// reports truncation/refusal, a native tool, a malformed envelope or unknown stop.
#[test]
fn bounded_completion_rejects_truncated_refused_and_unknown_envelopes() {
    let mut local: Value = serde_json::from_slice(&ollama_body("{}")).unwrap();
    let mut cloud: Value = serde_json::from_slice(&anthropic_body("{}")).unwrap();
    let mut cases = Vec::new();
    local["done_reason"] = json!("length");
    cases.push((false, local.clone(), FailureCode::IncompleteResponse));
    local["done_reason"] = json!("unexpected");
    cases.push((false, local.clone(), FailureCode::IncompleteResponse));
    local["done_reason"] = json!("stop");
    local["message"]["tool_calls"] = json!([{"function":{"name":"shell"}}]);
    cases.push((false, local, FailureCode::InvalidResponse));
    cloud["stop_reason"] = json!("max_tokens");
    cases.push((true, cloud.clone(), FailureCode::IncompleteResponse));
    cloud["stop_reason"] = json!("end_turn");
    cloud["stop_details"] = json!({"type":"refusal","explanation":"PRIVATE SOURCE"});
    cases.push((true, cloud.clone(), FailureCode::Refused));
    cloud["stop_details"] = Value::Null;
    cloud["content"] =
        json!([{"type":"tool_use","name":"shell","input":{"command":"PRIVATE SOURCE"}}]);
    cases.push((true, cloud, FailureCode::InvalidResponse));
    cases.push((
        false,
        json!({"message":{"content":"{}"}}),
        FailureCode::InvalidResponse,
    ));
    for (cloud, body, expected) in cases {
        let (endpoint, server) = http(framed(&serde_json::to_vec(&body).unwrap()));
        let provider: Box<dyn LlmProvider> = if cloud {
            Box::new(
                AnthropicProvider::with_endpoint(ClaudeLane::Opus, "fixture-key", endpoint)
                    .unwrap(),
            )
        } else {
            Box::new(OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap())
        };
        let error = provider
            .complete_bounded(
                &authorize(provider.as_ref(), CompletionLimits::default()),
                &control(),
            )
            .unwrap_err();
        assert_eq!(error, rejected(expected));
        assert!(!format!("{error:?} {error}").contains("PRIVATE SOURCE"));
        server.join().unwrap();
    }
}

// AC-0185: ignored JSON is still subject to a structural preflight, invalid UTF-8
// and impossible reported usage fail closed, and absent token counts stay absent.
#[test]
fn bounded_provider_json_preflight_and_usage_are_explicit() {
    let mut nested = ollama_body("{}");
    nested.pop();
    nested.extend_from_slice(b",\"ignored\":");
    nested.extend_from_slice("[".repeat(65).as_bytes());
    nested.push(b'0');
    nested.extend_from_slice("]".repeat(65).as_bytes());
    nested.push(b'}');
    let mut negative: Value = serde_json::from_slice(&ollama_body("{}")).unwrap();
    negative["eval_count"] = json!(-1);
    let mut excess = negative.clone();
    excess["eval_count"] = json!(2049);
    for body in [
        nested,
        serde_json::to_vec(&negative).unwrap(),
        serde_json::to_vec(&excess).unwrap(),
        b"PRIVATE SOURCE \xff".to_vec(),
    ] {
        let (endpoint, server) = http(framed(&body));
        let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
        let error = local
            .complete_bounded(&authorize(&local, CompletionLimits::default()), &control())
            .unwrap_err();
        assert_eq!(error, rejected(FailureCode::InvalidResponse));
        assert!(!format!("{error:?} {error}").contains("PRIVATE SOURCE"));
        server.join().unwrap();
    }
    let mut missing: Value = serde_json::from_slice(&ollama_body("{}")).unwrap();
    missing.as_object_mut().unwrap().remove("eval_count");
    missing.as_object_mut().unwrap().remove("prompt_eval_count");
    let (endpoint, server) = http(framed(&serde_json::to_vec(&missing).unwrap()));
    let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    assert!(
        local
            .complete_bounded(&authorize(&local, CompletionLimits::default()), &control())
            .unwrap()
            .usage
            .is_none()
    );
    server.join().unwrap();
}

// AC-0185: a single total timeout covers headers plus body; receiving an early
// body fragment must not restart the deadline. The server barrier controls stalls.
#[test]
fn bounded_completion_deadline_covers_headers_and_body() {
    for headers_first in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let (release, stalled) = mpsc::channel();
        let (seen, observed) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            set_timeouts(&stream);
            read_request(&mut stream).unwrap();
            if headers_first {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{")
                    .unwrap();
            }
            seen.send(()).unwrap();
            if headers_first {
                // Keep each read active well inside the request timeout. A
                // timeout which restarts for each fragment cannot pass this case.
                for _ in 0..30 {
                    match stalled.recv_timeout(Duration::from_millis(50)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if stream.write_all(b" ").is_err() {
                                break;
                            }
                        }
                    }
                }
            } else {
                stalled.recv_timeout(Duration::from_secs(5)).unwrap();
            }
        });
        let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
        // Remaining host budget is shorter than the provider's declared cap.
        let request = authorize(&local, CompletionLimits::default());
        let before = Instant::now();
        let error = local
            .complete_bounded(
                &request,
                &CompletionControl {
                    deadline: before + Duration::from_millis(300),
                    check: &proceed,
                },
            )
            .unwrap_err();
        observed.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(error, unknown(FailureCode::DeadlineExceeded));
        assert!(before.elapsed() < Duration::from_secs(3));
        let _ = release.send(());
        server.join().unwrap();
    }
}

// AC-0186/0187: pre-dispatch cancellation has no request. After dispatch the real
// worker waits for a bounded outcome; cancellation cannot erase a complete finish.
#[test]
fn bounded_completion_cancellation_preserves_complete_late_response() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let local = OllamaProvider::new(
        &format!("http://{}/", listener.local_addr().unwrap()),
        "test-model",
        Duration::from_secs(10),
    )
    .unwrap();
    let request = authorize(&local, CompletionLimits::default());
    for directive in [CallDirective::Cancelled, CallDirective::OwnershipLost] {
        let expected = if directive == CallDirective::Cancelled {
            FailureCode::Cancelled
        } else {
            FailureCode::OwnershipLost
        };
        let error = local
            .complete_bounded(
                &request,
                &CompletionControl {
                    deadline: Instant::now() + Duration::from_secs(5),
                    check: &|| directive,
                },
            )
            .unwrap_err();
        assert_eq!(error, not_sent(expected));
        assert_no_request(&listener);
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let cancelled = Arc::new(AtomicBool::new(false));
    let server_cancelled = Arc::clone(&cancelled);
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        set_timeouts(&stream);
        read_request(&mut stream).unwrap();
        server_cancelled.store(true, Ordering::SeqCst);
        stream
            .write_all(&framed(&ollama_body(r#"{"action":"finish"}"#)))
            .unwrap();
    });
    let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    let request = authorize(&local, CompletionLimits::default());
    let check = || {
        if cancelled.load(Ordering::SeqCst) {
            CallDirective::Cancelled
        } else {
            CallDirective::Continue
        }
    };
    let control = CompletionControl {
        deadline: Instant::now() + Duration::from_secs(5),
        check: &check,
    };
    let result = local.complete_bounded(&request, &control).unwrap();
    assert_eq!(result.text, r#"{"action":"finish"}"#);
    assert_eq!(
        local.complete_bounded(&request, &control).unwrap_err(),
        not_sent(FailureCode::Cancelled)
    );
    server.join().unwrap();
}

// AC-0185/0186: the new hash includes profile, endpoint, action, payload and
// limits. Legacy hashes stay on their old recipe; cloud admission remains closed.
#[test]
fn bounded_consent_binds_payload_model_endpoint_and_limits_without_changing_legacy() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let cloud = AnthropicProvider::with_endpoint(ClaudeLane::Opus, "fixture-key", endpoint.clone())
        .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Agentic]));
    let prepared = firewall
        .prepare_bounded(&cloud, &action(), &CompletionLimits::default())
        .unwrap();
    let grant = ConsentGrant::from_preview(&prepared.preview().egress);
    let legacy = firewall.preview(&cloud, &action()).unwrap();
    let old_bytes = serde_json::to_vec(&(
        cloud.id(),
        cloud.locality(),
        action().tier,
        action().action_id,
        legacy.payload.clone(),
    ))
    .unwrap();
    assert_eq!(
        legacy.payload_hash,
        blake3::hash(&old_bytes).to_hex().to_string()
    );
    assert_ne!(legacy.payload_hash, prepared.preview().egress.payload_hash);
    let old_grant = ConsentGrant::from_preview(&legacy);
    assert_eq!(
        firewall
            .authorize_bounded(&cloud, &prepared, Some(&old_grant))
            .err()
            .unwrap(),
        not_sent(FailureCode::ConsentMismatch)
    );
    let denied = EgressFirewall::new(EgressPolicy::local_only());
    assert_eq!(
        denied
            .authorize_bounded(&cloud, &prepared, Some(&grant))
            .err()
            .unwrap(),
        not_sent(FailureCode::EgressDenied)
    );
    assert_eq!(
        firewall
            .authorize_bounded(&cloud, &prepared, None)
            .err()
            .unwrap(),
        not_sent(FailureCode::ConsentRequired)
    );
    let changed_limits = CompletionLimits::new(CompletionLimitValues {
        max_output_tokens: 100,
        ..Default::default()
    })
    .unwrap();
    let changed = firewall
        .prepare_bounded(&cloud, &action(), &changed_limits)
        .unwrap();
    assert_eq!(
        firewall
            .authorize_bounded(&cloud, &changed, Some(&grant))
            .err()
            .unwrap(),
        not_sent(FailureCode::ConsentMismatch)
    );
    let mut next = action();
    next.action_id = "investigation-1:step-2".into();
    next.payload.prompt.push_str(" New tool result.");
    let changed = firewall
        .prepare_bounded(&cloud, &next, &CompletionLimits::default())
        .unwrap();
    assert_eq!(
        firewall
            .authorize_bounded(&cloud, &changed, Some(&grant))
            .err()
            .unwrap(),
        not_sent(FailureCode::ConsentMismatch)
    );
    let other =
        AnthropicProvider::with_endpoint(ClaudeLane::Haiku, "fixture-key", endpoint).unwrap();
    assert_eq!(
        firewall
            .authorize_bounded(&other, &prepared, Some(&grant))
            .err()
            .unwrap(),
        not_sent(FailureCode::ProfileChanged)
    );
    let other = AnthropicProvider::with_endpoint(
        ClaudeLane::Opus,
        "fixture-key",
        "http://127.0.0.1:1/".into(),
    )
    .unwrap();
    assert_eq!(
        firewall
            .authorize_bounded(&other, &prepared, Some(&grant))
            .err()
            .unwrap(),
        not_sent(FailureCode::ProfileChanged)
    );
    assert_no_request(&listener);
}

// AC-0185: invalid or oversized caller input is refused before dispatch, bounded
// serialization covers escaping expansion, and source metadata is redacted too.
#[test]
fn bounded_input_admission_and_serialization_fail_before_dispatch() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let local = OllamaProvider::new(
        &format!("http://{}/", listener.local_addr().unwrap()),
        "test-model",
        Duration::from_secs(10),
    )
    .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::local_only());
    let mut input = action();
    input.payload.prompt = "x".repeat(128 * 1024 + 1);
    assert_eq!(
        firewall
            .prepare_bounded(&local, &input, &CompletionLimits::default())
            .err()
            .unwrap(),
        not_sent(FailureCode::InputTooLarge)
    );
    let input = action();
    let limits = CompletionLimits::new(CompletionLimitValues {
        request_bytes: 16,
        ..Default::default()
    })
    .unwrap();
    let prepared = firewall.prepare_bounded(&local, &input, &limits).unwrap();
    let authorized = firewall.authorize_bounded(&local, &prepared, None).unwrap();
    assert_eq!(
        local.complete_bounded(&authorized, &control()).unwrap_err(),
        not_sent(FailureCode::RequestTooLarge)
    );
    let mut input = action();
    input.payload.spans.push(input.payload.spans[0].clone());
    assert_eq!(
        firewall
            .prepare_bounded(&local, &input, &CompletionLimits::default())
            .err()
            .unwrap(),
        not_sent(FailureCode::InvalidInput)
    );
    let mut input = action();
    input.payload.spans[0].repo = "password=super-secret".into();
    input.payload.prompt.push_str(" access_token=super-secret");
    let prepared = firewall
        .prepare_bounded(&local, &input, &CompletionLimits::default())
        .unwrap();
    assert!(
        !serde_json::to_string(prepared.preview())
            .unwrap()
            .contains("super-secret")
    );
    assert_no_request(&listener);
    let invalid = CompletionLimitValues {
        response_bytes: 0,
        ..Default::default()
    };
    assert_eq!(
        CompletionLimits::new(invalid).unwrap_err(),
        not_sent(FailureCode::InvalidLimits)
    );
    assert!(
        serde_json::from_value::<CompletionLimits>(serde_json::to_value(invalid).unwrap()).is_err()
    );
    let over = CompletionLimitValues {
        max_output_tokens: 2049,
        ..Default::default()
    };
    assert!(CompletionLimits::new(over).is_err());
}

// AC-0185: configured no-redirect/no-retry behavior is exercised against actual
// sockets; errors disclose neither Location nor a server-controlled error body.
#[test]
fn bounded_transport_never_follows_redirects_or_retries() {
    let destination = TcpListener::bind("127.0.0.1:0").unwrap();
    let location = format!(
        "http://{}/private-source",
        destination.local_addr().unwrap()
    );
    let response = format!(
        "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n"
    )
    .into_bytes();
    let (endpoint, server) = http(response);
    let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    assert_eq!(
        local
            .complete_bounded(&authorize(&local, CompletionLimits::default()), &control())
            .unwrap_err(),
        rejected(FailureCode::HttpStatus)
    );
    server.join().unwrap();
    assert_no_request(&destination);

    for response in [Some(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 14\r\nConnection: close\r\n\r\nPRIVATE SOURCE".to_vec()), None] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap(); set_timeouts(&stream);
            read_request(&mut stream).unwrap();
            if let Some(body) = response { stream.write_all(&body).unwrap(); }
            drop(stream);
            listener
        });
        let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
        let limits = CompletionLimits::new(CompletionLimitValues { request_timeout_ms: 250, ..Default::default() }).unwrap();
        let error = local.complete_bounded(&authorize(&local, limits), &control()).unwrap_err();
        assert!(matches!(error.code, FailureCode::HttpStatus | FailureCode::Transport));
        assert!(!format!("{error:?} {error}").contains("PRIVATE SOURCE"));
        assert_no_request(&server.join().unwrap());
    }
}

// AC-0185/0186: even a caller-added proxy cannot redirect a declared local
// bounded invocation off its direct loopback path.
#[test]
fn bounded_local_transport_ignores_caller_proxy() {
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    let (endpoint, server) = http(framed(&ollama_body("{}")));
    let mut local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    local.bounded_client = build_client(
        Client::builder()
            .proxy(reqwest::Proxy::all(format!("http://{}", proxy.local_addr().unwrap())).unwrap()),
        true,
    )
    .unwrap();
    assert_eq!(
        local
            .complete_bounded(&authorize(&local, CompletionLimits::default()), &control())
            .unwrap()
            .text,
        "{}"
    );
    server.join().unwrap();
    assert_no_request(&proxy);
}

// AC-0185: adding the bounded SPI cannot accidentally invoke a legacy-only
// implementation, even if its unbounded completion is otherwise supported.
#[test]
fn bounded_provider_spi_never_falls_back_to_legacy_complete() {
    struct Legacy(AtomicUsize);
    impl LlmProvider for Legacy {
        fn id(&self) -> &str {
            "legacy"
        }
        fn locality(&self) -> Locality {
            Locality::Local
        }
        fn capabilities(&self) -> crate::ProviderCaps {
            crate::ProviderCaps {
                embeddings: false,
                chat: true,
                tool_use: false,
            }
        }
        fn embed(&self, _: &[String]) -> Result<Vec<crate::Embedding>, crate::ProviderError> {
            Err(crate::ProviderError::Unsupported("embed"))
        }
        fn complete(
            &self,
            _: &crate::ProviderCompletionRequest,
        ) -> Result<crate::Completion, crate::ProviderError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(crate::Completion { text: "{}".into() })
        }
    }
    let provider = Legacy(AtomicUsize::new(0));
    let firewall = EgressFirewall::new(EgressPolicy::local_only());
    assert_eq!(
        firewall
            .prepare_bounded(&provider, &action(), &CompletionLimits::default())
            .err()
            .unwrap(),
        BoundedCallError::unsupported()
    );
    assert_eq!(provider.0.load(Ordering::SeqCst), 0);
}

fn https() -> (SocketAddr, JoinHandle<Option<FramedRequest>>) {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    let cert = CertificateDer::from(include_bytes!("fixtures/server.der").to_vec());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        include_bytes!("fixtures/server-key.der").to_vec(),
    ));
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        set_timeouts(&socket);
        let connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        let mut stream = rustls::StreamOwned::new(connection, socket);
        let request = read_request(&mut stream).ok()?;
        stream.write_all(&framed(&anthropic_body("{}"))).unwrap();
        stream.flush().unwrap();
        Some(request)
    });
    (address, server)
}

// AC-0185: this is a real TLS handshake and encrypted HTTP exchange with the
// actual Anthropic bounded method. Trust is test-local; production validation
// remains enabled. HTTP-only mocks cannot establish this requirement.
#[test]
fn bounded_cloud_transport_requires_verified_tls_and_hostname() {
    for (trust, hostname) in [
        (true, "localhost"),
        (false, "localhost"),
        (true, "wrong.invalid"),
    ] {
        let (address, server) = https();
        let mut builder = Client::builder()
            .tls_backend_rustls()
            .no_proxy()
            .resolve(hostname, address);
        if trust {
            let cert = reqwest::Certificate::from_der(include_bytes!("fixtures/ca.der")).unwrap();
            builder = builder.tls_certs_only([cert]);
        }
        let client = build_client(builder, false).unwrap();
        let endpoint = format!("https://{hostname}:{}/v1/messages", address.port());
        let cloud = AnthropicProvider::with_endpoint(ClaudeLane::Opus, "fixture-key", endpoint)
            .unwrap()
            .with_test_bounded_client(client);
        let result =
            cloud.complete_bounded(&authorize(&cloud, CompletionLimits::default()), &control());
        if trust && hostname == "localhost" {
            assert_eq!(result.unwrap().text, "{}");
            let observed = server.join().unwrap().unwrap();
            assert!(observed.headers.starts_with("POST /v1/messages HTTP/1.1"));
            assert_eq!(
                serde_json::from_slice::<Value>(&observed.body).unwrap()["max_tokens"],
                2048
            );
        } else {
            assert_eq!(result.unwrap_err(), unknown(FailureCode::Transport));
            assert!(
                server.join().unwrap().is_none(),
                "invalid TLS must not carry an HTTP request"
            );
        }
    }
}
