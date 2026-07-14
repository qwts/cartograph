//! Pluggable model providers behind a local-first egress firewall.
//!
//! All completion calls are prepared by [`EgressFirewall`]. The firewall
//! redacts secrets, hard-fails cloud providers unless the analysis tier is
//! enabled, and binds one explicit consent grant to the exact redacted
//! span-level payload. Provider-facing completion requests cannot be
//! constructed outside this crate, so callers cannot bypass the gate.

pub mod catalog;

use regex::Regex;
use reqwest::Url;
use reqwest::blocking::{Client, ClientBuilder};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::net::IpAddr;
use std::sync::OnceLock;
use std::time::Duration;

/// Dense embedding vector returned by a provider.
pub type Embedding = Vec<f32>;

/// Whether a provider executes locally or sends data off-device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Locality {
    /// The payload stays on this machine.
    Local,
    /// The payload leaves the machine and requires explicit consent.
    Cloud,
}

/// Model-backed analysis tiers governed independently by egress policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AnalysisTier {
    /// T2 semantic resolution.
    Semantic,
    /// T3 bounded agentic resolution.
    Agentic,
}

/// Capabilities exposed by a model provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCaps {
    /// Dense text embeddings.
    pub embeddings: bool,
    /// Text/chat completion.
    pub chat: bool,
    /// Tool-calling completion.
    pub tool_use: bool,
}

/// One exact source span included in a model payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadSpan {
    /// Stable identifier referenced by the prompt and model citations.
    pub id: String,
    /// Repository identity.
    pub repo: String,
    /// Repository-relative path.
    pub path: String,
    /// Inclusive byte offset in the source file.
    pub byte_start: u64,
    /// Exclusive byte offset in the source file.
    pub byte_end: u64,
    /// Commit from which the span was read.
    pub commit_sha: String,
    /// Exact text sent after secret redaction.
    pub text: String,
}

/// Structured completion payload shown to the user and sent to the provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionPayload {
    /// System instructions.
    pub system: String,
    /// Task instructions; evidence text lives only in `spans`.
    pub prompt: String,
    /// Exact evidence spans leaving the device.
    pub spans: Vec<PayloadSpan>,
}

/// A model-backed action submitted to the egress firewall.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionAction {
    /// Stable identifier for this one action.
    pub action_id: String,
    /// Tier requesting model access.
    pub tier: AnalysisTier,
    /// Structured payload.
    pub payload: CompletionPayload,
}

/// Exact redacted payload preview used by the consent UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressPreview {
    /// Provider receiving the payload.
    pub provider_id: String,
    /// Provider locality.
    pub locality: Locality,
    /// Tier requesting egress.
    pub tier: AnalysisTier,
    /// Stable action identifier.
    pub action_id: String,
    /// Exact redacted structured payload.
    pub payload: CompletionPayload,
    /// BLAKE3 hash binding consent to this provider, action, and payload.
    pub payload_hash: String,
    /// Number of secret-shaped values replaced before preview or dispatch.
    pub redaction_count: usize,
}

/// Explicit user grant for exactly one previewed cloud action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentGrant {
    /// Provider approved by the user.
    pub provider_id: String,
    /// Tier approved by the user.
    pub tier: AnalysisTier,
    /// Action approved by the user.
    pub action_id: String,
    /// Hash copied from the preview the user saw.
    pub payload_hash: String,
}

impl ConsentGrant {
    /// Create a grant from the exact preview a user accepted.
    pub fn from_preview(preview: &EgressPreview) -> Self {
        Self {
            provider_id: preview.provider_id.clone(),
            tier: preview.tier,
            action_id: preview.action_id.clone(),
            payload_hash: preview.payload_hash.clone(),
        }
    }

    fn matches(&self, preview: &EgressPreview) -> bool {
        self.provider_id == preview.provider_id
            && self.tier == preview.tier
            && self.action_id == preview.action_id
            && self.payload_hash == preview.payload_hash
    }
}

/// Per-tier cloud egress policy. The default contains no allowed tiers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EgressPolicy {
    cloud_tiers: BTreeSet<AnalysisTier>,
}

impl EgressPolicy {
    /// Default-deny local-only policy.
    pub fn local_only() -> Self {
        Self::default()
    }

    /// Permit cloud access for the listed tiers. Every action still requires
    /// its own matching [`ConsentGrant`].
    pub fn allow_cloud_for(tiers: impl IntoIterator<Item = AnalysisTier>) -> Self {
        Self {
            cloud_tiers: tiers.into_iter().collect(),
        }
    }

    /// Whether cloud access is configured for a tier.
    pub fn cloud_allowed(&self, tier: AnalysisTier) -> bool {
        self.cloud_tiers.contains(&tier)
    }
}

/// Completion output returned by a provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Completion {
    /// Provider-returned text.
    pub text: String,
}

/// Provider-facing request. Its fields are private so only the firewall can
/// construct an authorized, redacted request.
pub struct ProviderCompletionRequest {
    payload: CompletionPayload,
}

impl ProviderCompletionRequest {
    /// Redacted system instructions.
    pub fn system(&self) -> &str {
        &self.payload.system
    }

    /// Redacted task instructions.
    pub fn prompt(&self) -> &str {
        &self.payload.prompt
    }

    /// Exact redacted evidence spans.
    pub fn spans(&self) -> &[PayloadSpan] {
        &self.payload.spans
    }

    fn user_message(&self) -> Result<String, ProviderError> {
        Ok(format!(
            "{}\n\nEvidence spans (JSON):\n{}",
            self.payload.prompt,
            serde_json::to_string_pretty(&self.payload.spans)?
        ))
    }
}

/// Failures from provider construction, authorization, or invocation.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// Provider URL is malformed.
    #[error("invalid provider URL: {0}")]
    InvalidUrl(String),
    /// A provider declared local attempted to use a non-loopback endpoint.
    #[error("local provider endpoint must be loopback: {0}")]
    NonLoopback(String),
    /// HTTP transport or response decoding failed.
    #[error("provider request: {0}")]
    Request(#[from] reqwest::Error),
    /// JSON serialization or decoding failed.
    #[error("provider payload: {0}")]
    Payload(#[from] serde_json::Error),
    /// Provider returned a structurally invalid embedding batch.
    #[error("invalid embedding response: {0}")]
    InvalidEmbeddings(String),
    /// The provider does not implement the requested operation.
    #[error("provider does not support {0}")]
    Unsupported(&'static str),
    /// Default-deny policy blocked a cloud provider before invocation.
    #[error("cloud provider {provider} is disabled for {tier:?}")]
    EgressDenied {
        /// Blocked provider.
        provider: String,
        /// Blocked analysis tier.
        tier: AnalysisTier,
    },
    /// Cloud access is enabled but this exact action has not been accepted.
    #[error("explicit consent required for cloud action {0:?}")]
    ConsentRequired(Box<EgressPreview>),
    /// A grant was presented for a different provider, action, or payload.
    #[error("consent does not match the exact redacted payload")]
    ConsentMismatch,
    /// Provider returned an empty completion.
    #[error("provider returned an empty completion")]
    EmptyCompletion,
}

/// Model-provider SPI from SPEC-00 §8.2.
pub trait LlmProvider: Send + Sync {
    /// Stable provider identifier.
    fn id(&self) -> &str;
    /// Where the provider executes.
    fn locality(&self) -> Locality;
    /// Provider features.
    fn capabilities(&self) -> ProviderCaps;
    /// Embed a batch of texts in input order.
    fn embed(&self, batch: &[String]) -> Result<Vec<Embedding>, ProviderError>;
    /// Execute a completion already authorized and redacted by the firewall.
    fn complete(&self, _request: &ProviderCompletionRequest) -> Result<Completion, ProviderError> {
        Err(ProviderError::Unsupported("chat completion"))
    }
}

/// Default-deny completion firewall shared by semantic and agentic callers.
pub struct EgressFirewall {
    policy: EgressPolicy,
}

impl EgressFirewall {
    /// Construct a firewall from persisted/configured policy.
    pub fn new(policy: EgressPolicy) -> Self {
        Self { policy }
    }

    /// Build the exact redacted preview without invoking the provider.
    pub fn preview(
        &self,
        provider: &dyn LlmProvider,
        action: &CompletionAction,
    ) -> Result<EgressPreview, ProviderError> {
        if !provider.capabilities().chat {
            return Err(ProviderError::Unsupported("chat completion"));
        }
        if provider.locality() == Locality::Cloud && !self.policy.cloud_allowed(action.tier) {
            return Err(ProviderError::EgressDenied {
                provider: provider.id().to_string(),
                tier: action.tier,
            });
        }
        let (payload, redaction_count) = redact_payload(&action.payload);
        let hash_bytes = serde_json::to_vec(&(
            provider.id(),
            provider.locality(),
            action.tier,
            &action.action_id,
            &payload,
        ))?;
        Ok(EgressPreview {
            provider_id: provider.id().to_string(),
            locality: provider.locality(),
            tier: action.tier,
            action_id: action.action_id.clone(),
            payload,
            payload_hash: blake3::hash(&hash_bytes).to_hex().to_string(),
            redaction_count,
        })
    }

    /// Invoke a local provider immediately or a cloud provider only after a
    /// matching one-action grant. The provider sees only the redacted payload
    /// from the preview.
    pub fn complete(
        &self,
        provider: &dyn LlmProvider,
        action: &CompletionAction,
        consent: Option<&ConsentGrant>,
    ) -> Result<Completion, ProviderError> {
        let preview = self.preview(provider, action)?;
        if preview.locality == Locality::Cloud {
            let Some(grant) = consent else {
                return Err(ProviderError::ConsentRequired(Box::new(preview)));
            };
            if !grant.matches(&preview) {
                return Err(ProviderError::ConsentMismatch);
            }
        }
        let request = ProviderCompletionRequest {
            payload: preview.payload,
        };
        let completion = provider.complete(&request)?;
        if completion.text.trim().is_empty() {
            return Err(ProviderError::EmptyCompletion);
        }
        Ok(completion)
    }
}

/// Local Ollama provider using `/api/embed` and `/api/chat`.
pub struct OllamaProvider {
    base_url: Url,
    embedding_model: String,
    completion_model: String,
    provider_id: String,
    client: Client,
}

impl OllamaProvider {
    /// Default loopback endpoint used by Ollama.
    pub const DEFAULT_URL: &'static str = "http://127.0.0.1:11434/";
    /// Default local embedding model.
    pub const DEFAULT_EMBEDDING_MODEL: &'static str = "nomic-embed-text";
    /// Default local completion model. Cartograph never downloads it.
    pub const DEFAULT_COMPLETION_MODEL: &'static str = "qwen3:8b";
    /// Compatibility alias for the M7 embedding default.
    pub const DEFAULT_MODEL: &'static str = Self::DEFAULT_EMBEDDING_MODEL;

    /// Construct a loopback-only provider using one model for both operations.
    pub fn new(
        base_url: &str,
        model: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, ProviderError> {
        let model = model.into();
        Self::with_models(base_url, model.clone(), model, timeout)
    }

    /// Construct a loopback-only provider with separate embedding and chat
    /// models.
    pub fn with_models(
        base_url: &str,
        embedding_model: impl Into<String>,
        completion_model: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, ProviderError> {
        let mut base_url =
            Url::parse(base_url).map_err(|error| ProviderError::InvalidUrl(error.to_string()))?;
        let loopback = base_url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        });
        if base_url.scheme() != "http" || !loopback {
            return Err(ProviderError::NonLoopback(base_url.to_string()));
        }
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }
        let client = build_direct_loopback_client(Client::builder(), timeout)?;
        let embedding_model = embedding_model.into();
        let completion_model = completion_model.into();
        let provider_id = if embedding_model == completion_model {
            format!("ollama:{embedding_model}")
        } else {
            format!("ollama:{embedding_model}+{completion_model}")
        };
        Ok(Self {
            base_url,
            embedding_model,
            completion_model,
            provider_id,
            client,
        })
    }

    /// Construct the default local provider.
    pub fn local_default() -> Result<Self, ProviderError> {
        Self::with_models(
            Self::DEFAULT_URL,
            Self::DEFAULT_EMBEDDING_MODEL,
            Self::DEFAULT_COMPLETION_MODEL,
            Duration::from_secs(120),
        )
    }

    /// Probe the local endpoint for installed model references (`name:tag`).
    /// Loopback-only like every other call on this provider; the result
    /// feeds [`catalog::classify_health`] — Cartograph reports a missing
    /// model, it never pulls one itself.
    pub fn list_local_models(&self) -> Result<Vec<String>, ProviderError> {
        #[derive(Deserialize)]
        struct Tags {
            models: Vec<TagEntry>,
        }
        #[derive(Deserialize)]
        struct TagEntry {
            name: String,
        }
        let url = self
            .base_url
            .join("api/tags")
            .map_err(|error| ProviderError::InvalidUrl(error.to_string()))?;
        let tags: Tags = self.client.get(url).send()?.error_for_status()?.json()?;
        Ok(tags.models.into_iter().map(|entry| entry.name).collect())
    }
}

/// Ollama is declared Local, so its transport must never inherit either
/// environment/system proxies or a caller-added proxy. Otherwise loopback
/// evidence could leave the device without passing cloud consent.
fn build_direct_loopback_client(
    builder: ClientBuilder,
    timeout: Duration,
) -> Result<Client, ProviderError> {
    Ok(builder.no_proxy().timeout(timeout).build()?)
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a [String],
    truncate: bool,
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Embedding>,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    stream: bool,
    format: &'static str,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

impl LlmProvider for OllamaProvider {
    fn id(&self) -> &str {
        &self.provider_id
    }

    fn locality(&self) -> Locality {
        Locality::Local
    }

    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: true,
            chat: true,
            tool_use: false,
        }
    }

    fn embed(&self, batch: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        if batch.is_empty() {
            return Ok(Vec::new());
        }
        let endpoint = self
            .base_url
            .join("api/embed")
            .map_err(|error| ProviderError::InvalidUrl(error.to_string()))?;
        let response: EmbedResponse = self
            .client
            .post(endpoint)
            .json(&EmbedRequest {
                model: &self.embedding_model,
                input: batch,
                truncate: false,
            })
            .send()?
            .error_for_status()?
            .json()?;
        validate_embeddings(batch.len(), &response.embeddings)?;
        Ok(response.embeddings)
    }

    fn complete(&self, request: &ProviderCompletionRequest) -> Result<Completion, ProviderError> {
        let endpoint = self
            .base_url
            .join("api/chat")
            .map_err(|error| ProviderError::InvalidUrl(error.to_string()))?;
        let user = request.user_message()?;
        let response: ChatResponse = self
            .client
            .post(endpoint)
            .json(&ChatRequest {
                model: &self.completion_model,
                messages: vec![
                    ChatMessage {
                        role: "system",
                        content: request.system(),
                    },
                    ChatMessage {
                        role: "user",
                        content: &user,
                    },
                ],
                stream: false,
                format: "json",
            })
            .send()?
            .error_for_status()?
            .json()?;
        Ok(Completion {
            text: response.message.content,
        })
    }
}

fn validate_embeddings(expected: usize, embeddings: &[Embedding]) -> Result<(), ProviderError> {
    if embeddings.len() != expected {
        return Err(ProviderError::InvalidEmbeddings(format!(
            "expected {expected} vectors, got {}",
            embeddings.len()
        )));
    }
    let Some(dimensions) = embeddings.first().map(Vec::len) else {
        return Ok(());
    };
    if dimensions == 0
        || embeddings.iter().any(|embedding| {
            embedding.len() != dimensions || embedding.iter().any(|value| !value.is_finite())
        })
    {
        return Err(ProviderError::InvalidEmbeddings(
            "vectors must be non-empty, finite, and uniformly sized".into(),
        ));
    }
    Ok(())
}

fn redact_payload(payload: &CompletionPayload) -> (CompletionPayload, usize) {
    let (system, mut count) = redact_text(&payload.system);
    let (prompt, prompt_count) = redact_text(&payload.prompt);
    count += prompt_count;
    let spans = payload
        .spans
        .iter()
        .map(|span| {
            let (text, span_count) = redact_text(&span.text);
            count += span_count;
            PayloadSpan {
                text,
                ..span.clone()
            }
        })
        .collect();
    (
        CompletionPayload {
            system,
            prompt,
            spans,
        },
        count,
    )
}

fn redact_text(input: &str) -> (String, usize) {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        vec![
            (
                Regex::new(r"(?is)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----").unwrap(),
                "[REDACTED PRIVATE KEY]",
            ),
            (
                Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{8,}").unwrap(),
                "Bearer [REDACTED]",
            ),
            (
                Regex::new(r"\b(?:github_pat_|gh[pousr]_|sk-)[A-Za-z0-9_-]{8,}").unwrap(),
                "[REDACTED TOKEN]",
            ),
            (
                Regex::new(r"\bAKIA[0-9A-Z]{16}\b").unwrap(),
                "[REDACTED AWS ACCESS KEY]",
            ),
            (
                Regex::new(r#"(?i)\b(api[_-]?key|access[_-]?token|auth[_-]?token|password|secret)\b(\s*[:=]\s*[\"']?)([^\s\"',;}\]\[]{4,})"#).unwrap(),
                "$1$2[REDACTED]",
            ),
        ]
    });
    let mut output = input.to_string();
    let mut count = 0;
    for (pattern, replacement) in patterns {
        count += pattern.find_iter(&output).count();
        output = pattern.replace_all(&output, *replacement).into_owned();
    }
    (output, count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    fn action(text: &str) -> CompletionAction {
        CompletionAction {
            action_id: "resolve-gap-1".into(),
            tier: AnalysisTier::Agentic,
            payload: CompletionPayload {
                system: "Return JSON only".into(),
                prompt: "Resolve the explicit Gap".into(),
                spans: vec![PayloadSpan {
                    id: "source".into(),
                    repo: "acme/shop".into(),
                    path: "src/orders.ts".into(),
                    byte_start: 10,
                    byte_end: 42,
                    commit_sha: "abc123".into(),
                    text: text.into(),
                }],
            },
        }
    }

    struct FakeCloud {
        calls: AtomicUsize,
    }

    impl LlmProvider for FakeCloud {
        fn id(&self) -> &str {
            "cloud:test"
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
            Err(ProviderError::Unsupported("embeddings"))
        }

        fn complete(
            &self,
            request: &ProviderCompletionRequest,
        ) -> Result<Completion, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert!(!request.user_message()?.contains("super-secret"));
            Ok(Completion {
                text: r#"{"target_id":"chan:orders"}"#.into(),
            })
        }
    }

    #[test]
    fn local_provider_rejects_non_loopback_endpoints() {
        let error = OllamaProvider::new(
            "https://models.example.com",
            "embed",
            Duration::from_secs(1),
        )
        .err()
        .expect("cloud URL rejected");
        assert!(matches!(error, ProviderError::NonLoopback(_)));
    }

    #[test]
    fn ollama_embed_uses_batch_api_and_validates_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = vec![0_u8; 8192];
            let size = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..size]);
            assert!(request.starts_with("POST /api/embed HTTP/1.1"));
            assert!(request.contains("\"model\":\"test-model\""));
            assert!(request.contains("\"input\":[\"orders\",\"users\"]"));
            let body = r#"{"embeddings":[[1.0,0.0],[0.0,1.0]]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });

        let provider = OllamaProvider::new(
            &format!("http://{address}"),
            "test-model",
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(provider.id(), "ollama:test-model");
        let embeddings = provider.embed(&["orders".into(), "users".into()]).unwrap();
        assert_eq!(embeddings, [[1.0, 0.0], [0.0, 1.0]]);
        assert_eq!(provider.locality(), Locality::Local);
        assert!(provider.capabilities().embeddings);
        assert!(provider.capabilities().chat);
        server.join().unwrap();
    }

    #[test]
    fn ollama_client_disables_all_proxies() {
        // AC-0023: a Local provider stays physically local even if a system
        // or explicit reqwest proxy would otherwise capture loopback traffic.
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        let target_address = target.local_addr().unwrap();
        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_address = proxy.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = target.accept().unwrap();
            let mut request = vec![0_u8; 8192];
            let size = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..size]).starts_with("POST /api/embed"));
            let body = r#"{"embeddings":[[1.0,0.0]]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let client = build_direct_loopback_client(
            Client::builder().proxy(
                reqwest::Proxy::all(format!("http://{proxy_address}")).expect("valid fake proxy"),
            ),
            Duration::from_secs(2),
        )
        .unwrap();
        let provider = OllamaProvider {
            base_url: Url::parse(&format!("http://{target_address}/")).unwrap(),
            embedding_model: "test-model".into(),
            completion_model: "test-model".into(),
            provider_id: "ollama:test-model".into(),
            client,
        };
        assert_eq!(provider.embed(&["orders".into()]).unwrap(), [[1.0, 0.0]]);
        server.join().unwrap();

        proxy.set_nonblocking(true).unwrap();
        assert!(
            proxy
                .accept()
                .is_err_and(|error| error.kind() == std::io::ErrorKind::WouldBlock),
            "the fake proxy must receive no loopback connection"
        );
    }

    #[test]
    fn ollama_chat_uses_local_api_after_firewall_preparation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = vec![0_u8; 16384];
            let size = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..size]);
            assert!(request.starts_with("POST /api/chat HTTP/1.1"));
            assert!(request.contains("\"stream\":false"));
            assert!(request.contains("Resolve the explicit Gap"));
            assert!(request.contains("[REDACTED]"));
            assert!(!request.contains("super-secret"));
            let body = r#"{"message":{"role":"assistant","content":"{\"target_id\":\"chan:orders\"}"},"done":true}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let provider = OllamaProvider::new(
            &format!("http://{address}"),
            "test-model",
            Duration::from_secs(2),
        )
        .unwrap();
        let completion = EgressFirewall::new(EgressPolicy::local_only())
            .complete(&provider, &action("password=super-secret"), None)
            .unwrap();
        assert!(completion.text.contains("chan:orders"));
        server.join().unwrap();
    }

    #[test]
    fn local_only_policy_blocks_cloud_before_provider_invocation() {
        // AC-0023: local-only T3 cannot silently fall back to cloud.
        let provider = FakeCloud {
            calls: AtomicUsize::new(0),
        };
        let error = EgressFirewall::new(EgressPolicy::local_only())
            .complete(&provider, &action("safe"), None)
            .unwrap_err();
        assert!(matches!(error, ProviderError::EgressDenied { .. }));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cloud_consent_binds_exact_redacted_span_payload() {
        // AC-0024: the accepted preview is the payload sent, after redaction.
        let provider = FakeCloud {
            calls: AtomicUsize::new(0),
        };
        let firewall = EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Agentic]));
        let request = action("password=super-secret");
        let error = firewall.complete(&provider, &request, None).unwrap_err();
        let ProviderError::ConsentRequired(preview) = error else {
            panic!("expected consent preview");
        };
        assert_eq!(preview.payload.spans.len(), 1);
        assert_eq!(preview.payload.spans[0].text, "password=[REDACTED]");
        assert_eq!(preview.redaction_count, 1);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);

        let already_redacted = firewall
            .preview(&provider, &action("password=[REDACTED]"))
            .unwrap();
        assert_eq!(
            already_redacted.payload.spans[0].text,
            "password=[REDACTED]"
        );
        assert_eq!(already_redacted.redaction_count, 0);

        let grant = ConsentGrant::from_preview(&preview);
        firewall
            .complete(&provider, &request, Some(&grant))
            .unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

        // Changing only a secret value intentionally keeps the same outbound
        // redacted bytes. A real outbound content change invalidates consent.
        let changed = action("password=other-secret\nqueue=users");
        let error = firewall
            .complete(&provider, &changed, Some(&grant))
            .unwrap_err();
        assert!(matches!(error, ProviderError::ConsentMismatch));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn unavailable_local_provider_fails_without_fallback() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let provider = OllamaProvider::new(
            &format!("http://{address}"),
            "test-model",
            Duration::from_millis(200),
        )
        .unwrap();
        let error = provider.embed(&["orders".into()]).unwrap_err();
        assert!(matches!(error, ProviderError::Request(_)));
    }
}
