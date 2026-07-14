//! Managed local SLM tier (#122): the versioned model catalog, health
//! probing with explicit fail-closed unavailability, and schema-enforced
//! structured output for local completions.
//!
//! Local-first (ADR-0004) means the on-device path deserves the same rigor
//! as the cloud one: every LLM-touching action maps to a **pinned** model in
//! a versioned catalog so provenance can attribute a proposal to
//! `provider/model@catalog-version`; a missing model is an **explicit
//! unavailable state** — never a silent failure and never a cloud fallback;
//! and local completions validate against the caller's schema with a
//! bounded retry, keeping the broker's propose-only invariants intact.

use crate::Completion;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Versioned identity of this catalog; recorded alongside the provider id
/// in provenance so re-runs are attributable to an exact model mapping.
pub const CATALOG_VERSION: &str = "slm-catalog@1";

/// LLM-touching actions the engine performs locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ModelAction {
    /// T2 similarity embeddings (usearch input).
    Embedding,
    /// Cheap/fast lane: candidate pre-ranking and triage summaries.
    Triage,
    /// T3 proposal generation (bounded broker, propose-only).
    Proposal,
}

/// One pinned catalog entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelSpec {
    /// Ollama model reference (name:tag).
    pub model: &'static str,
    /// What this lane is for — shown in Settings health (#112).
    pub purpose: &'static str,
}

/// The recommended local model per action. Changing any pin bumps
/// [`CATALOG_VERSION`] — the mapping is part of recorded provenance.
pub fn model_for(action: ModelAction) -> ModelSpec {
    match action {
        ModelAction::Embedding => ModelSpec {
            model: "nomic-embed-text",
            purpose: "T2 similarity embeddings",
        },
        ModelAction::Triage => ModelSpec {
            model: "qwen3:1.7b",
            purpose: "candidate pre-ranking and triage summaries",
        },
        ModelAction::Proposal => ModelSpec {
            model: "qwen3:8b",
            purpose: "T3 proposal generation (propose-only)",
        },
    }
}

/// One installed model as reported by the local endpoint. The digest is the
/// immutable artifact identity behind the mutable `name:tag` reference —
/// two pulls of the same tag can be different artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalModel {
    /// Model reference (`name:tag`).
    pub name: String,
    /// Content digest of the installed artifact.
    pub digest: String,
}

/// Provenance-ready identity for an action's lane **as observed**: the
/// catalog pin plus the installed artifact's digest, e.g.
/// `ollama:qwen3:8b@slm-catalog@1#sha256:abcd…`. Recording the digest is
/// what keeps two artifacts behind the same tag distinguishable in
/// provenance (review fix on #130).
pub fn provider_identity(action: ModelAction, digest: &str) -> String {
    format!(
        "ollama:{}@{CATALOG_VERSION}#{digest}",
        model_for(action).model
    )
}

/// Health of one catalog lane. `Missing`/`Unreachable` are explicit,
/// user-facing unavailable states: the escalation refuses to run rather
/// than silently degrading or falling back to cloud (fail closed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum ModelHealth {
    /// The pinned model is installed and the endpoint answers. Carries the
    /// installed artifact's digest so callers record exactly what ran.
    Available {
        /// Content digest of the installed artifact.
        digest: String,
    },
    /// The endpoint answers but the pinned model is not installed.
    Missing {
        /// The pinned model reference.
        model: String,
        /// What the user can do about it.
        remediation: String,
    },
    /// The local endpoint did not answer at all.
    Unreachable {
        /// Why the probe failed.
        detail: String,
        /// What the user can do about it.
        remediation: String,
    },
}

/// Classify one action's health from an installed-models probe result.
/// Pure so the classification is testable without a live Ollama; the probe
/// itself is [`crate::OllamaProvider::list_local_models`].
pub fn classify_health(
    action: ModelAction,
    installed: &Result<Vec<LocalModel>, crate::ProviderError>,
) -> ModelHealth {
    let pinned = model_for(action).model;
    match installed {
        Ok(models) => {
            // Ollama reports `name:tag`; an untagged pin matches any tag.
            let found = models.iter().find(|installed| {
                installed.name == pinned
                    || installed
                        .name
                        .split_once(':')
                        .is_some_and(|(name, _)| name == pinned)
            });
            match found {
                Some(model) => ModelHealth::Available {
                    digest: model.digest.clone(),
                },
                None => ModelHealth::Missing {
                    model: pinned.to_string(),
                    remediation: format!(
                        "run `ollama pull {pinned}` — Cartograph never downloads models itself"
                    ),
                },
            }
        }
        Err(error) => ModelHealth::Unreachable {
            detail: error.to_string(),
            remediation: "start Ollama (`ollama serve`) on 127.0.0.1:11434".to_string(),
        },
    }
}

/// Structured-output failures for local completions.
#[derive(Debug, thiserror::Error)]
pub enum StructuredError {
    /// The provider itself failed.
    #[error(transparent)]
    Provider(#[from] crate::ProviderError),
    /// Every attempt produced output that did not validate against the
    /// schema. The last parse error is preserved for display.
    #[error("no schema-valid output after {attempts} attempt(s): {last_error}")]
    Invalid {
        /// Attempts consumed.
        attempts: u32,
        /// Final parse failure.
        last_error: String,
    },
}

/// Extract the first top-level JSON object from `text` — local models wrap
/// JSON in prose or code fences; the schema, not the wrapping, is the
/// contract.
fn first_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in text[start..].char_indices() {
        if in_string {
            match ch {
                '\\' if !escaped => escaped = true,
                '"' if !escaped => in_string = false,
                _ => escaped = false,
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=start + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Run `complete` until its output validates as `T`, up to `max_attempts`.
/// The closure receives the 1-based attempt number so callers can tighten
/// the prompt on retries. Bounded: exhausting attempts is a typed error,
/// never a silently dropped or half-parsed proposal.
pub fn complete_structured<T, F>(max_attempts: u32, mut complete: F) -> Result<T, StructuredError>
where
    T: DeserializeOwned,
    F: FnMut(u32) -> Result<Completion, crate::ProviderError>,
{
    let mut last_error = "no attempts were made".to_string();
    for attempt in 1..=max_attempts {
        let completion = complete(attempt)?;
        let Some(json) = first_json_object(&completion.text) else {
            last_error = "no JSON object in completion".to_string();
            continue;
        };
        match serde_json::from_str::<T>(json) {
            Ok(value) => return Ok(value),
            Err(error) => last_error = error.to_string(),
        }
    }
    Err(StructuredError::Invalid {
        attempts: max_attempts,
        last_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Proposal {
        target: String,
        confidence: f64,
    }

    fn completion(text: &str) -> Completion {
        Completion {
            text: text.to_string(),
        }
    }

    fn local(name: &str, digest: &str) -> LocalModel {
        LocalModel {
            name: name.into(),
            digest: digest.into(),
        }
    }

    #[test]
    fn every_action_has_a_digest_pinned_identity() {
        for action in [
            ModelAction::Embedding,
            ModelAction::Triage,
            ModelAction::Proposal,
        ] {
            let identity = provider_identity(action, "sha256:abc123");
            assert!(identity.starts_with("ollama:"));
            assert!(identity.contains(CATALOG_VERSION));
            // The observed artifact digest is part of the recorded identity:
            // two artifacts behind the same tag stay distinguishable.
            assert!(identity.ends_with("#sha256:abc123"));
        }
    }

    #[test]
    fn health_classification_is_explicit_never_silent() {
        let installed = Ok(vec![
            local("qwen3:8b", "sha256:aaa"),
            local("nomic-embed-text:latest", "sha256:bbb"),
        ]);
        // Available carries the installed digest so callers record exactly
        // which artifact ran.
        assert_eq!(
            classify_health(ModelAction::Proposal, &installed),
            ModelHealth::Available {
                digest: "sha256:aaa".into()
            }
        );
        // Untagged pin matches an installed tag — and reports that tag's
        // observed digest.
        assert_eq!(
            classify_health(ModelAction::Embedding, &installed),
            ModelHealth::Available {
                digest: "sha256:bbb".into()
            }
        );
        // Missing model: explicit state with remediation — not an error
        // swallowed, not a cloud fallback.
        match classify_health(ModelAction::Triage, &installed) {
            ModelHealth::Missing { model, remediation } => {
                assert_eq!(model, "qwen3:1.7b");
                assert!(remediation.contains("ollama pull"));
            }
            other => panic!("expected Missing, got {other:?}"),
        }
        // Unreachable endpoint: same discipline.
        let down: Result<Vec<LocalModel>, crate::ProviderError> =
            Err(crate::ProviderError::Unsupported("probe"));
        assert!(matches!(
            classify_health(ModelAction::Proposal, &down),
            ModelHealth::Unreachable { .. }
        ));
    }

    #[test]
    fn structured_output_retries_bounded_and_typed() {
        // Attempt 1: prose, no JSON. Attempt 2: fenced but schema-invalid.
        // Attempt 3: valid — wrapped in prose, which is fine.
        let outputs = [
            "I think the target is the orders queue.",
            "```json\n{\"target\": 42}\n```",
            "Here you go: {\"target\": \"ch:orders\", \"confidence\": 0.83} — cited.",
        ];
        let parsed: Proposal =
            complete_structured(3, |attempt| Ok(completion(outputs[(attempt - 1) as usize])))
                .expect("third attempt validates");
        assert_eq!(parsed.target, "ch:orders");

        // Exhaustion is a typed error carrying the last parse failure.
        let err = complete_structured::<Proposal, _>(2, |_| Ok(completion("no json here")))
            .expect_err("never validates");
        assert!(matches!(err, StructuredError::Invalid { attempts: 2, .. }));

        // Provider failures pass through untouched (no retry storm).
        let err = complete_structured::<Proposal, _>(3, |_| {
            Err(crate::ProviderError::Unsupported("chat completion"))
        })
        .expect_err("provider error propagates");
        assert!(matches!(err, StructuredError::Provider(_)));
    }

    #[test]
    fn json_extraction_handles_fences_prose_and_nesting() {
        assert_eq!(
            first_json_object("x {\"a\": {\"b\": \"}\"}} y"),
            Some("{\"a\": {\"b\": \"}\"}}")
        );
        assert_eq!(first_json_object("no json"), None);
    }
}
