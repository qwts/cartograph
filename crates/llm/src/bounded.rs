//! SPEC-10 bounded, separately authorized completions. These APIs do not change
//! legacy completion wire formats or consent hashes. Prepared inputs are transient;
//! the host journals consent consumption and invocation start before dispatch.

use crate::{
    AnalysisTier, CompletionAction, CompletionPayload, ConsentGrant, EgressFirewall, EgressPreview,
    LlmProvider, Locality,
};
use reqwest::blocking::{Client, ClientBuilder, RequestBuilder};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

/// Immutable protocol identity, including fixed transport behavior.
pub const PROTOCOL_VERSION: &str = "bounded-json-completion@1";
/// Connection ceiling of reusable bounded clients. A shorter total deadline also
/// shortens connection establishment; clients never get rebuilt for an invocation.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Serializable declared limits. Deserialize through [`CompletionLimits`] before
/// trusting caller-supplied values. All byte counts exclude HTTP/TLS framing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionLimitValues {
    pub input_bytes: u64,
    pub request_bytes: u64,
    pub response_bytes: u64,
    pub output_text_bytes: u64,
    pub max_output_tokens: u32,
    pub request_timeout_ms: u64,
}

impl Default for CompletionLimitValues {
    fn default() -> Self {
        Self {
            input_bytes: 128 * 1024,
            request_bytes: 512 * 1024,
            response_bytes: 256 * 1024,
            output_text_bytes: 32 * 1024,
            max_output_tokens: 2048,
            request_timeout_ms: 180_000,
        }
    }
}

/// Validated positive limits, each independently no greater than SPEC-10's cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(transparent)]
pub struct CompletionLimits(CompletionLimitValues);

impl CompletionLimits {
    pub fn new(values: CompletionLimitValues) -> Result<Self, BoundedCallError> {
        let ceiling = CompletionLimitValues::default();
        let pairs = [
            (values.input_bytes, ceiling.input_bytes),
            (values.request_bytes, ceiling.request_bytes),
            (values.response_bytes, ceiling.response_bytes),
            (values.output_text_bytes, ceiling.output_text_bytes),
            (
                u64::from(values.max_output_tokens),
                u64::from(ceiling.max_output_tokens),
            ),
            (values.request_timeout_ms, ceiling.request_timeout_ms),
        ];
        if pairs.iter().any(|(value, max)| *value == 0 || value > max) {
            return Err(not_sent(FailureCode::InvalidLimits));
        }
        Ok(Self(values))
    }

    pub fn values(&self) -> CompletionLimitValues {
        self.0
    }
}

impl<'de> Deserialize<'de> for CompletionLimits {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(CompletionLimitValues::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

/// Configured identity, not proof of the model weights that actually executed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderProfile {
    pub protocol_version: String,
    pub provider_id: String,
    pub locality: Locality,
    pub endpoint_id: String,
    pub requested_model: String,
}

impl ProviderProfile {
    fn validate(&self) -> Result<(), BoundedCallError> {
        if self.protocol_version != PROTOCOL_VERSION
            || !identifier(&self.provider_id, 1024)
            || !identifier(&self.requested_model, 256)
            || self.endpoint_id.len() > 4096
        {
            return Err(not_sent(FailureCode::InvalidProfile));
        }
        let url = reqwest::Url::parse(&self.endpoint_id)
            .map_err(|_| not_sent(FailureCode::InvalidProfile))?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.scheme(), "http" | "https")
            || (self.locality == Locality::Local && (url.scheme() != "http" || !loopback(&url)))
            // The only HTTP cloud endpoint is the existing offline fixture seam.
            || (self.locality == Locality::Cloud && url.scheme() == "http" && !loopback(&url))
        {
            return Err(not_sent(FailureCode::InvalidProfile));
        }
        Ok(())
    }
}

/// Exact preview; this includes source text and must stay transient. The embedded
/// preview reuses the consent renderer and grant shape, with a new hash domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoundedEgressPreview {
    pub egress: EgressPreview,
    pub profile: ProviderProfile,
    pub limits: CompletionLimits,
}

/// Only the firewall constructs this validated, redacted input. No deserializer.
pub struct PreparedBoundedCompletion {
    preview: BoundedEgressPreview,
}

impl std::fmt::Debug for PreparedBoundedCompletion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedBoundedCompletion")
            .field("payload_hash", &self.preview.egress.payload_hash)
            .finish_non_exhaustive()
    }
}

impl PreparedBoundedCompletion {
    pub fn preview(&self) -> &BoundedEgressPreview {
        &self.preview
    }
    pub fn profile(&self) -> &ProviderProfile {
        &self.preview.profile
    }
    pub fn limits(&self) -> &CompletionLimits {
        &self.preview.limits
    }
}

/// Firewall-authorized transient input. Durable one-use consent consumption is
/// enforced by the coordinator, not by this in-memory provider capability.
pub struct AuthorizedBoundedCompletion {
    preview: BoundedEgressPreview,
}

impl std::fmt::Debug for AuthorizedBoundedCompletion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthorizedBoundedCompletion")
            .field("payload_hash", &self.preview.egress.payload_hash)
            .finish_non_exhaustive()
    }
}

impl AuthorizedBoundedCompletion {
    pub fn payload(&self) -> &CompletionPayload {
        &self.preview.egress.payload
    }
    pub fn profile(&self) -> &ProviderProfile {
        &self.preview.profile
    }
    pub fn limits(&self) -> &CompletionLimits {
        &self.preview.limits
    }
    pub fn payload_hash(&self) -> &str {
        &self.preview.egress.payload_hash
    }
    pub fn action_id(&self) -> &str {
        &self.preview.egress.action_id
    }
    pub fn tier(&self) -> AnalysisTier {
        self.preview.egress.tier
    }
}

/// Host checks are brief and source-free. They may inspect durable ownership;
/// no lock is held while waiting for HTTP. Blocking transport is cooperative.
pub struct CompletionControl<'a> {
    pub deadline: Instant,
    pub check: &'a dyn Fn() -> CallDirective,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallDirective {
    Continue,
    Cancelled,
    OwnershipLost,
}

/// Transport evidence only; it does not establish remote cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationOutcome {
    NotDispatched,
    ResponseReceived,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    Unsupported,
    InvalidLimits,
    InvalidInput,
    InvalidProfile,
    ProfileChanged,
    EgressDenied,
    ConsentRequired,
    ConsentMismatch,
    InputTooLarge,
    RequestTooLarge,
    ResponseTooLarge,
    OutputTooLarge,
    Cancelled,
    OwnershipLost,
    DeadlineExceeded,
    Transport,
    HttpStatus,
    UnsupportedEncoding,
    InvalidResponse,
    IncompleteResponse,
    Refused,
    EmptyCompletion,
}

/// Fixed diagnostics never retain transport URLs, body snippets, parser errors,
/// credentials, source text or provider-controlled refusal explanations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("bounded provider call: {code:?} ({outcome:?})")]
pub struct BoundedCallError {
    pub code: FailureCode,
    pub outcome: InvocationOutcome,
}

impl BoundedCallError {
    pub fn unsupported() -> Self {
        not_sent(FailureCode::Unsupported)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportedUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
}

/// Text is transient until the caller admits the strict action and source-replay
/// policy. Usage is provider-reported, not verified billing or token accounting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundedCompletion {
    pub text: String,
    pub requested_model: String,
    pub response_model: Option<String>,
    pub provider_request_id: Option<String>,
    pub usage: Option<ReportedUsage>,
    pub request_bytes: u64,
    pub response_bytes: u64,
}

impl EgressFirewall {
    /// Validate/redact and fingerprint before preview; never invokes a provider.
    pub fn prepare_bounded(
        &self,
        provider: &dyn LlmProvider,
        action: &CompletionAction,
        limits: &CompletionLimits,
    ) -> Result<PreparedBoundedCompletion, BoundedCallError> {
        let profile = provider.bounded_profile()?;
        validate_provider(provider, &profile)?;
        check_policy(self, &profile, action.tier)?;
        if !identifier(&action.action_id, 1024) || action.payload.spans.len() > 12 {
            return Err(not_sent(FailureCode::InvalidInput));
        }
        let mut ids = std::collections::BTreeSet::new();
        for span in &action.payload.spans {
            if !identifier(&span.id, 1024)
                || span.byte_start >= span.byte_end
                || !ids.insert(&span.id)
            {
                return Err(not_sent(FailureCode::InvalidInput));
            }
        }
        let strings = [&action.payload.system, &action.payload.prompt]
            .into_iter()
            .chain(action.payload.spans.iter().flat_map(|span| {
                [
                    &span.id,
                    &span.repo,
                    &span.path,
                    &span.commit_sha,
                    &span.text,
                ]
            }));
        let mut input_bytes = 0_u64;
        for value in strings {
            input_bytes = input_bytes
                .checked_add(value.len() as u64)
                .filter(|total| *total <= limits.0.input_bytes)
                .ok_or_else(|| not_sent(FailureCode::InputTooLarge))?;
        }
        // This walk aborts before an unbounded buffer, regex input or clone can
        // be allocated. Logical JSON includes all metadata, not just span text.
        encode(
            &action.payload,
            limits.0.input_bytes,
            FailureCode::InputTooLarge,
        )?;
        let (mut payload, mut redaction_count) = crate::redact_payload(&action.payload);
        // Legacy redaction is unchanged. The new protocol covers metadata too.
        for span in &mut payload.spans {
            for value in [
                &mut span.id,
                &mut span.repo,
                &mut span.path,
                &mut span.commit_sha,
            ] {
                let (redacted, count) = crate::redact_text(value);
                *value = redacted;
                redaction_count += count;
            }
        }
        let mut redacted_ids = std::collections::BTreeSet::new();
        if payload
            .spans
            .iter()
            .any(|span| !redacted_ids.insert(&span.id))
        {
            return Err(not_sent(FailureCode::InvalidInput));
        }
        encode(&payload, limits.0.input_bytes, FailureCode::InputTooLarge)?;
        let hash_input = encode(
            &(
                "cartograph-bounded-egress-v1",
                &profile,
                limits,
                action.tier,
                &action.action_id,
                &payload,
            ),
            512 * 1024,
            FailureCode::InputTooLarge,
        )?;
        let payload_hash = blake3::hash(&hash_input).to_hex().to_string();
        Ok(PreparedBoundedCompletion {
            preview: BoundedEgressPreview {
                egress: EgressPreview {
                    provider_id: profile.provider_id.clone(),
                    locality: profile.locality,
                    tier: action.tier,
                    action_id: action.action_id.clone(),
                    payload,
                    payload_hash,
                    redaction_count,
                },
                profile,
                limits: *limits,
            },
        })
    }

    /// Recheck current policy and exact identity after a consent wait. The host
    /// atomically consumes approval/journals the call after this returns.
    pub fn authorize_bounded(
        &self,
        provider: &dyn LlmProvider,
        prepared: &PreparedBoundedCompletion,
        consent: Option<&ConsentGrant>,
    ) -> Result<AuthorizedBoundedCompletion, BoundedCallError> {
        let profile = provider.bounded_profile()?;
        validate_provider(provider, &profile)?;
        if profile != prepared.preview.profile {
            return Err(not_sent(FailureCode::ProfileChanged));
        }
        check_policy(self, &profile, prepared.preview.egress.tier)?;
        if profile.locality == Locality::Cloud {
            let grant = consent.ok_or_else(|| not_sent(FailureCode::ConsentRequired))?;
            if !grant.matches(&prepared.preview.egress) {
                return Err(not_sent(FailureCode::ConsentMismatch));
            }
        }
        Ok(AuthorizedBoundedCompletion {
            preview: prepared.preview.clone(),
        })
    }
}

fn validate_provider(
    provider: &dyn LlmProvider,
    profile: &ProviderProfile,
) -> Result<(), BoundedCallError> {
    profile.validate()?;
    if !provider.capabilities().chat {
        return Err(BoundedCallError::unsupported());
    }
    if profile.provider_id != provider.id() || profile.locality != provider.locality() {
        return Err(not_sent(FailureCode::InvalidProfile));
    }
    Ok(())
}

fn check_policy(
    firewall: &EgressFirewall,
    profile: &ProviderProfile,
    tier: AnalysisTier,
) -> Result<(), BoundedCallError> {
    if profile.locality == Locality::Cloud && !firewall.policy.cloud_allowed(tier) {
        Err(not_sent(FailureCode::EgressDenied))
    } else {
        Ok(())
    }
}

fn identifier(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn loopback(url: &reqwest::Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    })
}

fn not_sent(code: FailureCode) -> BoundedCallError {
    BoundedCallError {
        code,
        outcome: InvocationOutcome::NotDispatched,
    }
}
fn rejected(code: FailureCode) -> BoundedCallError {
    BoundedCallError {
        code,
        outcome: InvocationOutcome::ResponseReceived,
    }
}
fn unknown(code: FailureCode) -> BoundedCallError {
    BoundedCallError {
        code,
        outcome: InvocationOutcome::Unknown,
    }
}

struct LimitedWriter {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}
impl Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(io::Error::other("bounded serialization limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode(
    value: &impl Serialize,
    limit: u64,
    code: FailureCode,
) -> Result<Vec<u8>, BoundedCallError> {
    let mut writer = LimitedWriter {
        bytes: Vec::new(),
        limit: usize::try_from(limit).map_err(|_| not_sent(FailureCode::InvalidLimits))?,
        exceeded: false,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| {
        not_sent(if writer.exceeded {
            code
        } else {
            FailureCode::InvalidInput
        })
    })?;
    Ok(writer.bytes)
}

/// Construct once with the provider and retain at application/coordinator scope.
pub(super) fn build_client(builder: ClientBuilder, local: bool) -> Result<Client, reqwest::Error> {
    let builder = builder
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .no_gzip()
        .no_brotli()
        .no_zstd()
        .no_deflate()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(Duration::from_secs(180));
    let builder = if local { builder.no_proxy() } else { builder };
    builder.build()
}

pub(super) fn ollama_profile(
    id: &str,
    base: &reqwest::Url,
    model: &str,
) -> Result<ProviderProfile, BoundedCallError> {
    let endpoint = base
        .join("api/chat")
        .map_err(|_| not_sent(FailureCode::InvalidProfile))?;
    let profile = ProviderProfile {
        protocol_version: PROTOCOL_VERSION.into(),
        provider_id: id.into(),
        locality: Locality::Local,
        endpoint_id: endpoint.to_string(),
        requested_model: model.into(),
    };
    profile.validate()?;
    Ok(profile)
}

fn check_control(control: &CompletionControl<'_>, sent: bool) -> Result<(), BoundedCallError> {
    let code = match (control.check)() {
        CallDirective::Cancelled => Some(FailureCode::Cancelled),
        CallDirective::OwnershipLost => Some(FailureCode::OwnershipLost),
        CallDirective::Continue if Instant::now() >= control.deadline => {
            Some(FailureCode::DeadlineExceeded)
        }
        CallDirective::Continue => None,
    };
    match code {
        Some(code) => Err(if sent { unknown(code) } else { not_sent(code) }),
        None => Ok(()),
    }
}

fn check_request(
    profile: &ProviderProfile,
    request: &AuthorizedBoundedCompletion,
    control: &CompletionControl<'_>,
) -> Result<(), BoundedCallError> {
    profile.validate()?;
    if profile != request.profile() {
        return Err(not_sent(FailureCode::ProfileChanged));
    }
    check_control(control, false)
}

fn user_message(request: &AuthorizedBoundedCompletion) -> Result<String, BoundedCallError> {
    let spans = encode(
        &request.payload().spans,
        request.limits().0.input_bytes,
        FailureCode::InputTooLarge,
    )?;
    let spans = String::from_utf8(spans).map_err(|_| not_sent(FailureCode::InvalidInput))?;
    Ok(format!(
        "{}\n\nEvidence spans (JSON):\n{}",
        request.payload().prompt,
        spans
    ))
}

fn acquire(
    builder: RequestBuilder,
    body: Vec<u8>,
    request: &AuthorizedBoundedCompletion,
    control: &CompletionControl<'_>,
) -> Result<(Vec<u8>, u64), BoundedCallError> {
    check_control(control, false)?;
    let remaining = control.deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(not_sent(FailureCode::DeadlineExceeded));
    }
    let timeout = Duration::from_millis(request.limits().0.request_timeout_ms).min(remaining);
    let call_deadline = Instant::now() + timeout;
    let request_bytes = body.len() as u64;
    let (client, built) = builder
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .timeout(timeout)
        .body(body)
        .build_split();
    let mut built = built.map_err(|_| not_sent(FailureCode::InvalidInput))?;
    check_control(control, false)?;
    *built.timeout_mut() =
        Some(timeout.min(control.deadline.saturating_duration_since(Instant::now())));
    let mut response = client.execute(built).map_err(|error| {
        unknown(if error.is_timeout() {
            FailureCode::DeadlineExceeded
        } else {
            FailureCode::Transport
        })
    })?;
    if !response.status().is_success() {
        return Err(rejected(FailureCode::HttpStatus));
    }
    if response
        .headers()
        .get_all(reqwest::header::CONTENT_ENCODING)
        .iter()
        .any(|value| value.as_bytes() != b"identity")
    {
        return Err(rejected(FailureCode::UnsupportedEncoding));
    }
    let cap = request.limits().0.response_bytes as usize;
    if response
        .content_length()
        .is_some_and(|len| len > cap as u64)
    {
        return Err(rejected(FailureCode::ResponseTooLarge));
    }
    let mut body = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        // Always probe one byte beyond the cap; Content-Length is not authority.
        let read_limit = chunk.len().min(cap.saturating_sub(body.len()) + 1);
        let count = response.read(&mut chunk[..read_limit]).map_err(|error| {
            let timed_out = matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ) || error
                .get_ref()
                .and_then(|e| e.downcast_ref::<reqwest::Error>())
                .is_some_and(reqwest::Error::is_timeout)
                || Instant::now() >= call_deadline;
            unknown(if timed_out {
                FailureCode::DeadlineExceeded
            } else {
                FailureCode::Transport
            })
        })?;
        // A complete response wins a simultaneous cancellation. Admission and
        // persistence still run, but the host may never execute its next tool.
        if count == 0 {
            break;
        }
        if count > cap.saturating_sub(body.len()) {
            return Err(rejected(FailureCode::ResponseTooLarge));
        }
        body.extend_from_slice(&chunk[..count]);
        // Once issued, finish this bounded response or its total timeout. A
        // cancellation observed between the last data chunk and EOF must not
        // erase an otherwise complete result. The coordinator stops further work.
    }
    Ok((body, request_bytes))
}

fn finish(
    text: String,
    model: Option<String>,
    id: Option<String>,
    usage: Option<ReportedUsage>,
    request: &AuthorizedBoundedCompletion,
    request_bytes: u64,
    response_bytes: usize,
) -> Result<BoundedCompletion, BoundedCallError> {
    if text.len() as u64 > request.limits().0.output_text_bytes {
        return Err(rejected(FailureCode::OutputTooLarge));
    }
    if text.trim().is_empty() {
        return Err(rejected(FailureCode::EmptyCompletion));
    }
    if model.as_ref().is_some_and(|model| !identifier(model, 256))
        || id.as_ref().is_some_and(|id| !identifier(id, 256))
        || usage.as_ref().is_some_and(|usage| {
            [
                usage.input_tokens,
                usage.output_tokens,
                usage.cache_read_input_tokens,
                usage.cache_creation_input_tokens,
            ]
            .into_iter()
            .flatten()
            .any(|tokens| tokens > 1_000_000_000)
                || usage
                    .output_tokens
                    .is_some_and(|tokens| tokens > u64::from(request.limits().0.max_output_tokens))
        })
    {
        return Err(rejected(FailureCode::InvalidResponse));
    }
    Ok(BoundedCompletion {
        text,
        requested_model: request.profile().requested_model.clone(),
        response_model: model,
        provider_request_id: id,
        usage,
        request_bytes,
        response_bytes: response_bytes as u64,
    })
}

// Check structural work before serde allocates response containers, including
// ignored provider fields. Escaped model action JSON is a string, not containers.
// The typed decoder remains authoritative for JSON syntax.
fn preflight_response(bytes: &[u8]) -> Result<(), BoundedCallError> {
    let mut quoted = false;
    let mut escaped = false;
    let mut depth = 0_usize;
    let mut containers = 0_usize;
    for byte in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'{' | b'[' => {
                depth += 1;
                containers += 1;
                if depth > 64 || containers > 4096 {
                    return Err(rejected(FailureCode::InvalidResponse));
                }
            }
            b'}' | b']' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| rejected(FailureCode::InvalidResponse))?;
            }
            _ => (),
        }
    }
    if quoted || depth != 0 {
        return Err(rejected(FailureCode::InvalidResponse));
    }
    Ok(())
}

pub(super) fn complete_ollama(
    client: &Client,
    profile: &ProviderProfile,
    request: &AuthorizedBoundedCompletion,
    control: &CompletionControl<'_>,
) -> Result<BoundedCompletion, BoundedCallError> {
    check_request(profile, request, control)?;
    let user = user_message(request)?;
    let body = encode(
        &serde_json::json!({
            "model": profile.requested_model,
            "messages": [{"role":"system", "content":request.payload().system}, {"role":"user", "content":user}],
            "stream": false, "format": "json", "options": {"num_predict":request.limits().0.max_output_tokens}
        }),
        request.limits().0.request_bytes,
        FailureCode::RequestTooLarge,
    )?;
    let (bytes, request_bytes) =
        acquire(client.post(&profile.endpoint_id), body, request, control)?;
    #[derive(Deserialize)]
    struct Message {
        role: String,
        content: String,
        #[serde(default)]
        tool_calls: Vec<serde_json::Value>,
        #[serde(default)]
        images: Vec<serde_json::Value>,
    }
    #[derive(Deserialize)]
    struct Response {
        message: Message,
        done: bool,
        done_reason: String,
        model: Option<String>,
        prompt_eval_count: Option<u64>,
        eval_count: Option<u64>,
    }
    preflight_response(&bytes)?;
    let response: Response =
        serde_json::from_slice(&bytes).map_err(|_| rejected(FailureCode::InvalidResponse))?;
    if !response.done || response.done_reason != "stop" {
        return Err(rejected(FailureCode::IncompleteResponse));
    }
    if response.message.role != "assistant"
        || !response.message.tool_calls.is_empty()
        || !response.message.images.is_empty()
    {
        return Err(rejected(FailureCode::InvalidResponse));
    }
    let usage = (response.prompt_eval_count.is_some() || response.eval_count.is_some()).then_some(
        ReportedUsage {
            input_tokens: response.prompt_eval_count,
            output_tokens: response.eval_count,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        },
    );
    finish(
        response.message.content,
        response.model,
        None,
        usage,
        request,
        request_bytes,
        bytes.len(),
    )
}

pub(super) fn complete_anthropic(
    client: &Client,
    api_key: &str,
    profile: &ProviderProfile,
    request: &AuthorizedBoundedCompletion,
    control: &CompletionControl<'_>,
) -> Result<BoundedCompletion, BoundedCallError> {
    check_request(profile, request, control)?;
    if api_key.is_empty()
        || api_key.len() > 4096
        || reqwest::header::HeaderValue::from_str(api_key).is_err()
    {
        return Err(not_sent(FailureCode::InvalidProfile));
    }
    let user = user_message(request)?;
    let body = encode(
        &serde_json::json!({
            "model":profile.requested_model, "max_tokens":request.limits().0.max_output_tokens,
            "stream":false, "system":request.payload().system,
            "messages":[{"role":"user", "content":user}]
        }),
        request.limits().0.request_bytes,
        FailureCode::RequestTooLarge,
    )?;
    let builder = client
        .post(&profile.endpoint_id)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01");
    let (bytes, request_bytes) = acquire(builder, body, request, control)?;
    preflight_response(&bytes)?;
    #[derive(Deserialize)]
    struct Block {
        #[serde(rename = "type")]
        kind: String,
        text: Option<String>,
    }
    #[derive(Deserialize)]
    struct StopDetails {
        #[serde(rename = "type")]
        kind: Option<String>,
    }
    #[derive(Deserialize)]
    struct Usage {
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        cache_read_input_tokens: Option<u64>,
        cache_creation_input_tokens: Option<u64>,
    }
    #[derive(Deserialize)]
    struct Response {
        #[serde(rename = "type")]
        kind: String,
        role: String,
        content: Vec<Block>,
        stop_reason: String,
        stop_details: Option<StopDetails>,
        model: Option<String>,
        id: Option<String>,
        usage: Option<Usage>,
    }
    let response: Response =
        serde_json::from_slice(&bytes).map_err(|_| rejected(FailureCode::InvalidResponse))?;
    if response.stop_reason == "refusal"
        || response
            .stop_details
            .is_some_and(|details| details.kind.as_deref() == Some("refusal"))
    {
        return Err(rejected(FailureCode::Refused));
    }
    if response.stop_reason != "end_turn" {
        return Err(rejected(FailureCode::IncompleteResponse));
    }
    if response.kind != "message" || response.role != "assistant" || response.content.len() > 32 {
        return Err(rejected(FailureCode::InvalidResponse));
    }
    let mut text = String::new();
    for block in response.content {
        match block.kind.as_str() {
            "text" => {
                let part = block
                    .text
                    .ok_or_else(|| rejected(FailureCode::InvalidResponse))?;
                if part.len() as u64
                    > request
                        .limits()
                        .0
                        .output_text_bytes
                        .saturating_sub(text.len() as u64)
                {
                    return Err(rejected(FailureCode::OutputTooLarge));
                }
                text.push_str(&part);
            }
            // Ignored private thinking still counted in the complete body cap.
            "thinking" | "redacted_thinking" => (),
            _ => return Err(rejected(FailureCode::InvalidResponse)),
        }
    }
    let usage = response.usage.map(|usage| ReportedUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
    });
    finish(
        text,
        response.model,
        response.id,
        usage,
        request,
        request_bytes,
        bytes.len(),
    )
}

#[cfg(test)]
mod tests;
