//! Cloud-provider construction and consent-grant conversion — the two steps
//! that turn a settings choice and an approved preview into something that
//! can actually leave the device. Kept in its own module (rather than
//! `main.rs`) so CODEOWNERS can protect exactly this surface without also
//! gating every app-shell change that happens to touch `main.rs` (#457).

use llm::{ConsentGrant, EgressPreview, LlmProvider};

/// The provider for one escalation mode. Local is the pinned catalog SLM;
/// cloud is the Opus reasoning lane and needs an API key — its absence is
/// an explicit error, never a silent local fallback.
pub fn escalation_provider(mode: &str) -> Result<Box<dyn LlmProvider>, String> {
    match mode {
        "local" => Ok(Box::new(
            llm::OllamaProvider::local_default().map_err(|e| e.to_string())?,
        )),
        "cloud" => {
            let key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
                "no Anthropic API key configured (set ANTHROPIC_API_KEY) — cloud escalation \
                 stays closed"
                    .to_string()
            })?;
            Ok(Box::new(
                llm::anthropic::AnthropicProvider::new(llm::anthropic::ClaudeLane::Opus, key)
                    .map_err(|e| e.to_string())?,
            ))
        }
        other => Err(format!("unknown escalation mode '{other}' (local | cloud)")),
    }
}

/// Converts an approved payload preview into a consent grant, failing closed
/// if the caller's approved hash does not match this exact payload
/// (one-action consent, AC-0063).
pub fn consent_grant_for_approved_payload(
    preview: &EgressPreview,
    approved_payload_hash: Option<String>,
) -> Result<ConsentGrant, String> {
    let approved = approved_payload_hash
        .ok_or_else(|| "cloud escalation requires an approved payload hash".to_string())?;
    if approved != preview.payload_hash {
        return Err(
            "approved payload hash does not match the current payload — re-review the preview \
             before consenting"
                .to_string(),
        );
    }
    Ok(ConsentGrant::from_preview(preview))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_without_api_key_fails_closed() {
        // SAFETY (test-only): scoped to this process's env for the duration
        // of the assertion; no other test in this binary reads this var.
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
        let err = escalation_provider("cloud")
            .err()
            .expect("missing key rejected");
        assert!(err.contains("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn unknown_mode_is_rejected() {
        let err = escalation_provider("quantum")
            .err()
            .expect("unknown mode rejected");
        assert!(err.contains("unknown escalation mode"));
    }
}
