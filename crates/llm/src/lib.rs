//! Pluggable model-provider contracts with a local-first Ollama embedding
//! implementation (SPEC-00 §8.2, §11; ADR-0004).
//!
//! The default provider is pinned to a loopback HTTP endpoint. A caller cannot
//! accidentally turn the local provider into cloud egress by changing a URL;
//! cloud providers and explicit consent arrive with M8.

use reqwest::Url;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
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

/// Completion input reserved for the M8 agentic tier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionRequest {
    /// System instructions.
    pub system: String,
    /// User/model input.
    pub prompt: String,
}

/// Completion output reserved for the M8 agentic tier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Completion {
    /// Provider-returned text.
    pub text: String,
}

/// Failures from provider construction or invocation.
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
    /// Provider returned a structurally invalid embedding batch.
    #[error("invalid embedding response: {0}")]
    InvalidEmbeddings(String),
    /// The provider does not implement the requested operation.
    #[error("provider does not support {0}")]
    Unsupported(&'static str),
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
    /// Complete a prompt. M7 embedding-only providers fail explicitly.
    fn complete(&self, _request: &CompletionRequest) -> Result<Completion, ProviderError> {
        Err(ProviderError::Unsupported("chat completion"))
    }
}

/// Local Ollama provider using the current batch `/api/embed` endpoint.
pub struct OllamaProvider {
    base_url: Url,
    model: String,
    provider_id: String,
    client: Client,
}

impl OllamaProvider {
    /// Default loopback endpoint used by Ollama.
    pub const DEFAULT_URL: &'static str = "http://127.0.0.1:11434/";
    /// Default local embedding model. Ollama downloads models only when the
    /// user explicitly does so; Cartograph never silently pulls one.
    pub const DEFAULT_MODEL: &'static str = "nomic-embed-text";

    /// Construct a loopback-only provider.
    pub fn new(
        base_url: &str,
        model: impl Into<String>,
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
        let client = Client::builder().timeout(timeout).build()?;
        let model = model.into();
        Ok(Self {
            base_url,
            provider_id: format!("ollama:{model}"),
            model,
            client,
        })
    }

    /// Construct the default local provider.
    pub fn local_default() -> Result<Self, ProviderError> {
        Self::new(
            Self::DEFAULT_URL,
            Self::DEFAULT_MODEL,
            Duration::from_secs(120),
        )
    }
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
            chat: false,
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
                model: &self.model,
                input: batch,
                truncate: false,
            })
            .send()?
            .error_for_status()?
            .json()?;
        validate_embeddings(batch.len(), &response.embeddings)?;
        Ok(response.embeddings)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

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
        server.join().unwrap();
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
