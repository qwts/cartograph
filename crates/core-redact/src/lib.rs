//! Pure deterministic redaction shared by source recovery and higher tiers.
//!
//! Recognizers are shared, but callers retain distinct policies (SPEC-02,
//! ADR-0020). [`redact_text`] preserves the existing LLM payload substitutions;
//! [`is_token_shaped`] preserves the toolchain's conservative string detector.
//! Neither function parses source code or establishes that an input is safe.
//! The dependent AST policy must preserve literal types, inspect decoded values,
//! and record withheld source before expressions enter persistent rule facts.
//!
//! This crate performs no network, provider, filesystem, environment, or model
//! operations. Moving these helpers is groundwork for AC-0124, not completion
//! of the source-expression storage boundary.

use regex::Regex;
use std::sync::OnceLock;

const SENSITIVE_NAME_PATTERN: &str = r"api[_-]?key|access[_-]?token|auth[_-]?token|password|secret";

/// Category recognized by the shared deterministic secret patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKind {
    /// A private-key block.
    PrivateKey,
    /// A bearer authorization token.
    BearerToken,
    /// A provider-specific credential token.
    ProviderToken,
    /// An AWS access-key identifier.
    AwsAccessKey,
    /// A credential-assignment value in free text.
    CredentialAssignment,
}

fn patterns() -> &'static [(Regex, &'static str, SecretKind)] {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str, SecretKind)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            (
                Regex::new(
                    r"(?is)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
                )
                .unwrap(),
                "[REDACTED PRIVATE KEY]",
                SecretKind::PrivateKey,
            ),
            (
                Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{8,}").unwrap(),
                "Bearer [REDACTED]",
                SecretKind::BearerToken,
            ),
            (
                Regex::new(r"\b(?:github_pat_|gh[pousr]_|sk-)[A-Za-z0-9_-]{8,}").unwrap(),
                "[REDACTED TOKEN]",
                SecretKind::ProviderToken,
            ),
            (
                Regex::new(r"\bAKIA[0-9A-Z]{16}\b").unwrap(),
                "[REDACTED AWS ACCESS KEY]",
                SecretKind::AwsAccessKey,
            ),
            (
                Regex::new(&format!(
                    r"(?i)\b({SENSITIVE_NAME_PATTERN})\b{}",
                    r#"(\s*[:=]\s*[\"']?)([^\s\"',;}\]\[]{4,})"#,
                ))
                .unwrap(),
                "$1$2[REDACTED]",
                SecretKind::CredentialAssignment,
            ),
        ]
    })
}

/// Recognize categories in a decoded value without exposing matched text.
///
/// Each category appears at most once, in the free-text pattern order. Unlike
/// [`redact_text`], this function inspects the original input for every pattern,
/// so overlapping categories can both be reported. It does not apply the
/// distinct [`is_token_shaped`] policy or establish that an unmatched value is safe.
pub fn recognized_secrets(input: &str) -> Vec<SecretKind> {
    patterns()
        .iter()
        .filter_map(|(pattern, _, kind)| pattern.is_match(input).then_some(*kind))
        .collect()
}

/// Whether an entire source binding/property name is a recognized credential key.
///
/// Uses the same name alternatives as free-text credential assignment, anchored
/// to the whole name. Callers must establish the AST relationship to a value;
/// the function makes no inference about similarly named functions or variables.
pub fn is_sensitive_name(name: &str) -> bool {
    static NAME: OnceLock<Regex> = OnceLock::new();
    NAME.get_or_init(|| Regex::new(&format!(r"(?i)^(?:{SENSITIVE_NAME_PATTERN})$")).unwrap())
        .is_match(name)
}

/// Replace recognized secrets in free text and return the replacement count.
///
/// Patterns run in their established order, so the returned bytes and count
/// remain compatible with existing LLM previews and consent hashes. Existing
/// replacement markers are not counted again. This is a free-text policy, not
/// a source sanitizer: for example, `password=false` becomes
/// `password=[REDACTED]`. An AST-aware caller must retain that boolean's type
/// and value rather than applying this function blindly to an expression.
pub fn redact_text(input: &str) -> (String, usize) {
    let mut output = input.to_string();
    let mut count = 0;
    for (pattern, replacement, _) in patterns() {
        count += pattern.find_iter(&output).count();
        output = pattern.replace_all(&output, *replacement).into_owned();
    }
    (output, count)
}

/// Whether a string matches the toolchain's conservative token-shaped policy.
///
/// The value must contain at least 40 UTF-8 bytes, no whitespace, and only
/// ASCII letters, digits, or `+ / _ - = .`. This intentionally includes some
/// long business identifiers; callers must disclose withholding rather than
/// treating a detected value as proven secret or silently interpreting it.
/// This predicate does not replace text and does not detect every secret.
pub fn is_token_shaped(value: &str) -> bool {
    value.len() >= 40
        && !value.contains(char::is_whitespace)
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '_' | '-' | '=' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_recognition_reports_categories_without_changing_replacement_policy() {
        // AC-0124 groundwork: AST policy receives categories, never reverse
        // engineers replacement text or maintains independent secret regexes.
        assert_eq!(
            recognized_secrets("Bearer sk-abcdefgh1234"),
            vec![SecretKind::BearerToken, SecretKind::ProviderToken]
        );
        assert_eq!(
            redact_text("Bearer sk-abcdefgh1234"),
            ("Bearer [REDACTED]".into(), 1)
        );
        assert_eq!(
            recognized_secrets("password=synthetic-value"),
            vec![SecretKind::CredentialAssignment]
        );
        assert!(recognized_secrets("[REDACTED TOKEN]").is_empty());
        assert!(recognized_secrets("ordinary business message").is_empty());
        assert!(recognized_secrets(&"a".repeat(40)).is_empty());
    }

    #[test]
    fn sensitive_names_reuse_assignment_keys_with_exact_boundaries() {
        // AC-0124 groundwork: a proven property named password is sensitive;
        // passwordRequired and setPassword do not imply a credential value.
        for name in [
            "password",
            "SECRET",
            "api-key",
            "api_key",
            "apiKey",
            "accessToken",
            "auth_token",
        ] {
            assert!(is_sensitive_name(name));
            assert_eq!(redact_text(&format!("{name}=synthetic-value")).1, 1);
        }
        for name in [
            "passwordRequired",
            "setPassword",
            "secrets",
            "password ",
            "not-secret",
        ] {
            assert!(!is_sensitive_name(name));
        }
    }

    #[test]
    fn free_text_redaction_preserves_existing_payload_bytes_and_counts() {
        // AC-0124 groundwork: relocation must preserve the existing egress
        // policy before the dependent AST storage policy uses these helpers.
        let cases = [
            (
                "-----BEGIN RSA PRIVATE KEY-----\nsynthetic key\n-----END RSA PRIVATE KEY-----",
                "[REDACTED PRIVATE KEY]",
            ),
            ("Bearer abcdefgh1234", "Bearer [REDACTED]"),
            ("github_pat_abcdefgh1234", "[REDACTED TOKEN]"),
            ("ghp_abcdefgh1234", "[REDACTED TOKEN]"),
            ("sk-abcdefgh1234", "[REDACTED TOKEN]"),
            ("AKIA0000000000000000", "[REDACTED AWS ACCESS KEY]"),
            ("password=super-secret", "password=[REDACTED]"),
            ("api-key: 'synthetic-value'", "api-key: '[REDACTED]'"),
            ("ACCESS_TOKEN=synthetic-value", "ACCESS_TOKEN=[REDACTED]"),
        ];
        for (input, expected) in cases {
            assert_eq!(redact_text(input), (expected.into(), 1));
        }

        // The bearer recognizer runs before the provider-token recognizer;
        // an overlapping token is replaced once, not counted twice.
        assert_eq!(
            redact_text("Bearer sk-abcdefgh1234\npassword=synthetic-value"),
            ("Bearer [REDACTED]\npassword=[REDACTED]".into(), 2)
        );
    }

    #[test]
    fn free_text_redaction_is_idempotent_without_changing_safe_text() {
        // AC-0124 groundwork: previewed output remains stable on re-use.
        let safe = "queue=orders\nif (options.enabled !== false) return 'not-ready';";
        assert_eq!(redact_text(safe), (safe.into(), 0));

        let first = redact_text(
            "-----BEGIN PRIVATE KEY-----\nsynthetic\n-----END PRIVATE KEY-----\nBearer abcdefgh1234\npassword=synthetic-value\nAKIA0000000000000000\nsk-abcdefgh1234",
        );
        assert_eq!(first.1, 5);
        assert_eq!(redact_text(&first.0), (first.0, 0));
    }

    #[test]
    fn free_text_assignment_policy_is_not_a_source_literal_policy() {
        // AC-0124 groundwork: compatibility is intentional; the dependent
        // AST sanitizer must preserve typed false rather than use this result.
        assert_eq!(
            redact_text("password=false"),
            ("password=[REDACTED]".into(), 1)
        );
        assert_eq!(redact_text("false"), ("false".into(), 0));
    }

    #[test]
    fn token_shape_preserves_toolchain_threshold_and_character_rules() {
        // AC-0124 groundwork and AC-0097 compatibility: no broadening or
        // weakening of the existing toolchain omission policy during reuse.
        assert!(!is_token_shaped(&"a".repeat(39)));
        assert!(is_token_shaped(&"a".repeat(40)));
        assert!(is_token_shaped(&format!("{}+/_-=.", "a".repeat(34))));
        for suffix in [" ", "\n", "\t", ":", "@", "é", "\u{2003}"] {
            assert!(!is_token_shaped(&format!("{}{suffix}", "a".repeat(40))));
        }
        assert!(!is_token_shaped(""));
        assert!(!is_token_shaped("false"));

        // Conservative false positives remain visible to the caller; do not
        // special-case a project's message names in shared recognition.
        assert!(is_token_shaped(
            "message.cannot-start-action-without-required-context"
        ));
    }
}
