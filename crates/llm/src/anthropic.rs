//! Anthropic Claude API provider (#123): the opt-in cloud reasoning lane.
//!
//! First cloud implementation of [`LlmProvider`] (ADR-0004). Three pinned
//! lanes: Haiku for cheap pre-ranking/triage, Opus as the default T3
//! reasoning lane, and Fable as the opt-in hard-reasoning lane for the most
//! demanding escalations. Discipline unchanged from the local path:
//!
//! - Requests are only ever built from a firewall-authorized, redacted
//!   [`ProviderCompletionRequest`] — there is no other code path from graph
//!   data to the network.
//! - `embed` is [`ProviderError::Unsupported`]: embeddings stay local
//!   (Ollama + usearch); Claude is the reasoning provider.
//! - A safety refusal is a typed outcome ([`ProviderError::Refused`]), and
//!   the Fable lane opts into a server-side fallback to Opus so a benign
//!   false positive degrades gracefully instead of failing the escalation.
//! - [`disclosure`] renders exactly what the Settings consent panel must
//!   show before consent (provider/model, endpoint, pricing, retention).

use crate::{
    Completion, LlmProvider, Locality, ProviderCaps, ProviderCompletionRequest, ProviderError,
};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// The production Messages API endpoint. Not configurable in release code:
/// consented payloads can only ever go to Anthropic.
pub const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";
/// Server-side refusal-fallback beta (Fable lane).
const FALLBACK_BETA: &str = "server-side-fallback-2026-06-01";

/// The three cloud lanes. Model ids are pinned here the way the local
/// catalog pins Ollama models — changing one is a reviewed decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ClaudeLane {
    /// Cheap/fast: candidate pre-ranking, triage summaries.
    Haiku,
    /// Default reasoning lane for T3 proposals.
    Opus,
    /// Opt-in hard-reasoning lane for the most demanding escalations
    /// (long-horizon, multi-repo). Thinking is always on; requires 30-day
    /// data retention on the Anthropic org.
    Fable,
}

impl ClaudeLane {
    /// Exact API model id.
    pub fn model_id(self) -> &'static str {
        match self {
            // Dated snapshot, not the alias: pre-4.6 aliases are mutable
            // convenience pointers, and a pinned lane must not change model
            // behind a stable identity (review fix on #131).
            Self::Haiku => "claude-haiku-4-5-20251001",
            Self::Opus => "claude-opus-4-8",
            Self::Fable => "claude-fable-5",
        }
    }

    /// (input, output) USD per million tokens — consent-panel estimates.
    pub fn usd_per_mtok(self) -> (f64, f64) {
        match self {
            Self::Haiku => (1.0, 5.0),
            Self::Opus => (5.0, 25.0),
            Self::Fable => (10.0, 50.0),
        }
    }
}

/// Everything the fail-closed consent panel shows *before* consent
/// (design handoff §Settings; consumed by #112/#113/#118).
#[derive(Debug, Clone, Serialize)]
pub struct CloudDisclosure {
    pub provider: &'static str,
    pub model: &'static str,
    pub endpoint: &'static str,
    pub input_usd_per_mtok: f64,
    pub output_usd_per_mtok: f64,
    /// Lane-specific caveats the user must see (e.g. Fable retention).
    pub notes: Vec<&'static str>,
}

/// The consent disclosure for one lane.
pub fn disclosure(lane: ClaudeLane) -> CloudDisclosure {
    let (input, output) = lane.usd_per_mtok();
    let mut notes = vec!["payload is the exact redacted span set shown by the egress firewall"];
    if lane == ClaudeLane::Fable {
        notes.push("requires 30-day data retention on the Anthropic organization");
        notes.push("safety refusals fall back server-side to claude-opus-4-8");
    }
    CloudDisclosure {
        provider: "Anthropic",
        model: lane.model_id(),
        endpoint: API_URL,
        input_usd_per_mtok: input,
        output_usd_per_mtok: output,
        notes,
    }
}

/// Cloud provider for one Claude lane. Declared [`Locality::Cloud`], so the
/// egress firewall demands explicit consent bound to the exact payload
/// before [`LlmProvider::complete`] can ever run.
pub struct AnthropicProvider {
    lane: ClaudeLane,
    provider_id: String,
    api_key: String,
    endpoint: String,
    client: Client,
}

impl AnthropicProvider {
    /// Construct a provider for `lane`. The API key comes from the caller
    /// (OS keychain at the app layer) — it is never persisted here.
    pub fn new(lane: ClaudeLane, api_key: impl Into<String>) -> Result<Self, ProviderError> {
        Self::with_endpoint(lane, api_key, API_URL.to_string())
    }

    /// Test seam: same provider against a local mock transport. Not
    /// reachable from release call sites, which pin [`API_URL`].
    #[doc(hidden)]
    pub fn with_endpoint(
        lane: ClaudeLane,
        api_key: impl Into<String>,
        endpoint: String,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            provider_id: format!("anthropic:{}", lane.model_id()),
            lane,
            api_key: api_key.into(),
            endpoint,
            client: Client::builder()
                .timeout(Duration::from_secs(600))
                .build()?,
        })
    }

    fn request_body(&self, request: &ProviderCompletionRequest) -> serde_json::Value {
        // Evidence spans travel verbatim — the firewall already showed the
        // user this exact payload; reshaping it here would break the
        // consent hash contract.
        let spans = request
            .spans()
            .iter()
            .map(|span| format!("[{}] {}", span.id, span.text))
            .collect::<Vec<_>>()
            .join("\n");
        let content = format!("{}\n\n{spans}", request.prompt());
        let mut body = serde_json::json!({
            "model": self.lane.model_id(),
            "max_tokens": 16000,
            "system": request.system(),
            "messages": [{ "role": "user", "content": content }],
        });
        if self.lane == ClaudeLane::Fable {
            // Thinking is always on for Fable (no `thinking` param), and a
            // policy refusal re-runs server-side on Opus. The beta opt-in
            // travels as the `anthropic-beta` header (see `complete`).
            body["fallbacks"] = serde_json::json!([{ "model": ClaudeLane::Opus.model_id() }]);
        }
        body
    }
}

#[derive(Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
    stop_reason: Option<String>,
    stop_details: Option<StopDetails>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct StopDetails {
    category: Option<String>,
}

impl LlmProvider for AnthropicProvider {
    fn id(&self) -> &str {
        &self.provider_id
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

    fn embed(&self, _batch: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        // Embeddings stay local (Ollama + usearch): the cloud lane exists
        // for reasoning, and bulk evidence never leaves for similarity.
        Err(ProviderError::Unsupported(
            "embeddings are local-only; Claude is the reasoning provider",
        ))
    }

    fn complete(&self, request: &ProviderCompletionRequest) -> Result<Completion, ProviderError> {
        let mut builder = self
            .client
            .post(&self.endpoint)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION);
        if self.lane == ClaudeLane::Fable {
            // The server-side fallback opt-in is a header, not a body field;
            // `fallbacks` without it is rejected (review fix on #131).
            builder = builder.header("anthropic-beta", FALLBACK_BETA);
        }
        let response = builder
            .json(&self.request_body(request))
            .send()?
            .error_for_status()?;
        let message: MessagesResponse = response.json()?;
        if message.stop_reason.as_deref() == Some("refusal") {
            return Err(ProviderError::Refused {
                provider: self.provider_id.clone(),
                category: message.stop_details.and_then(|details| details.category),
            });
        }
        let text: String = message
            .content
            .iter()
            .filter(|block| block.kind == "text")
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("");
        // The firewall rejects empty completions after this returns; the
        // provider's own contract is just faithful transport.
        Ok(Completion { text })
    }
}

use crate::Embedding;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AnalysisTier, CompletionAction, CompletionPayload, EgressFirewall, EgressPolicy,
        PayloadSpan,
    };
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// One-shot local mock of the Messages API: accepts a single request,
    /// captures its body, answers with `response_json`.
    fn mock_api(response_json: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 65536];
            let read = stream.read(&mut buffer).unwrap();
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                response_json.len(),
                response_json
            );
            stream.write_all(reply.as_bytes()).unwrap();
            request
        });
        (endpoint, handle)
    }

    /// The one legal path to a cloud completion: the real firewall, with a
    /// consent grant bound to the exact payload.
    fn action() -> CompletionAction {
        CompletionAction {
            action_id: "propose-gap-target".into(),
            tier: AnalysisTier::Agentic,
            payload: CompletionPayload {
                system: "You are a bounded resolver.".into(),
                prompt: "Cite only listed candidates.".into(),
                spans: vec![PayloadSpan {
                    id: "span-1".into(),
                    repo: "local/app".into(),
                    path: "src/queue.ts".into(),
                    byte_start: 0,
                    byte_end: 24,
                    commit_sha: "workdir".into(),
                    text: "queueUrl = env.ORDERS".into(),
                }],
            },
        }
    }

    fn complete_with_consent(
        provider: &AnthropicProvider,
    ) -> Result<crate::Completion, ProviderError> {
        let firewall = EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Agentic]));
        let action = action();
        let preview = firewall.preview(provider, &action).expect("previewable");
        let grant = crate::ConsentGrant::from_preview(&preview);
        firewall.complete(provider, &action, Some(&grant))
    }

    #[test]
    fn completes_through_firewall_with_pinned_model_and_verbatim_spans() {
        let (endpoint, server) = mock_api(
            r#"{"content":[{"type":"text","text":"{\"target\":\"ch:orders\"}"}],"stop_reason":"end_turn"}"#,
        );
        let provider =
            AnthropicProvider::with_endpoint(ClaudeLane::Opus, "sk-test", endpoint).unwrap();
        let completion = complete_with_consent(&provider).unwrap();
        assert!(completion.text.contains("ch:orders"));

        let raw = server.join().unwrap();
        // Pinned model, auth header, and the exact consented span text.
        assert!(raw.contains("claude-opus-4-8"));
        assert!(raw.contains("x-api-key: sk-test"));
        assert!(raw.contains("queueUrl = env.ORDERS"));
        // Opus lane sends no fallback beta.
        assert!(!raw.contains("server-side-fallback"));
    }

    #[test]
    fn cloud_completion_without_consent_fails_closed() {
        // No mock server: the request must be refused before any transport.
        let provider = AnthropicProvider::new(ClaudeLane::Opus, "sk-test").unwrap();
        let firewall = EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Agentic]));
        let error = firewall
            .complete(&provider, &action(), None)
            .expect_err("cloud without consent is impossible");
        assert!(matches!(error, ProviderError::ConsentRequired(_)));
    }

    #[test]
    fn fable_lane_opts_into_server_side_fallback_and_refusals_are_typed() {
        let (endpoint, server) = mock_api(
            r#"{"content":[],"stop_reason":"refusal","stop_details":{"category":"cyber"}}"#,
        );
        let provider =
            AnthropicProvider::with_endpoint(ClaudeLane::Fable, "sk-test", endpoint).unwrap();
        let error = complete_with_consent(&provider).expect_err("refusal is typed");
        match error {
            ProviderError::Refused { provider, category } => {
                assert_eq!(provider, "anthropic:claude-fable-5");
                assert_eq!(category.as_deref(), Some("cyber"));
            }
            other => panic!("expected Refused, got {other:?}"),
        }
        let raw = server.join().unwrap();
        assert!(raw.contains("claude-fable-5"));
        // The beta opt-in travels as a header — `fallbacks` in the body is
        // rejected without it.
        assert!(raw.contains(&format!("anthropic-beta: {FALLBACK_BETA}")));
        assert!(raw.contains("claude-opus-4-8")); // fallback target in body
        assert!(!raw.contains("\"betas\"")); // never a body field
        assert!(!raw.contains("\"thinking\"")); // always-on: param omitted
    }

    #[test]
    fn embeddings_are_refused_and_disclosure_is_complete() {
        let provider = AnthropicProvider::new(ClaudeLane::Haiku, "sk-test").unwrap();
        assert!(matches!(
            provider.embed(&["text".into()]),
            Err(ProviderError::Unsupported(_))
        ));
        assert_eq!(provider.locality(), Locality::Cloud);

        for lane in [ClaudeLane::Haiku, ClaudeLane::Opus, ClaudeLane::Fable] {
            let disclosure = disclosure(lane);
            assert_eq!(disclosure.provider, "Anthropic");
            assert_eq!(disclosure.endpoint, API_URL);
            assert!(disclosure.input_usd_per_mtok > 0.0);
        }
        // The Fable lane discloses its retention requirement before consent.
        assert!(
            disclosure(ClaudeLane::Fable)
                .notes
                .iter()
                .any(|note| note.contains("30-day data retention"))
        );
    }
}
