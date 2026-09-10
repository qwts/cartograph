//! Conservative source-expression capture before rule facts are persisted.
//!
//! This is AST-aware storage policy, not execution or behavioral interpretation.
//! It inspects decoded literals through shared `core-redact` recognizers, keeps
//! boolean/null types, removes comments, and withholds unsupported expressions
//! without copying their source into a fallback field (SPEC-02, AC-0124).

use crate::FileCx;
use core_graph::rules::{
    ExpressionCapture, KnownLiteral, LiteralEvidence, LiteralKind, RedactionReason,
    SanitizedDisplay, SourceExpression, SourceRedaction,
};
use core_prov::EvidenceRef;
use core_redact::{SecretKind, is_sensitive_name, is_token_shaped, recognized_secrets};
use tree_sitter::Node as TsNode;

const MAX_EXPRESSION_BYTES: usize = 16 * 1024;
const MAX_EXPRESSION_NODES: usize = 1024;
const MAX_CONTEXT_ANCESTORS: usize = 128;
const WITHHELD: &str = "[REDACTED]";
const UNSUPPORTED: &str = "[UNSUPPORTED EXPRESSION]";

struct Replacement {
    start: usize,
    end: usize,
    text: &'static str,
}

/// Capture a source expression using deterministic, literal-aware storage policy.
///
/// Original evidence offsets are retained. An unsupported descendant withholds
/// the whole display rather than falling back to raw text. No source is run,
/// no model is called, and no consumer meaning is assigned to the expression.
pub(super) fn sanitize_expression(
    cx: &FileCx<'_>,
    node: TsNode<'_>,
) -> (SourceExpression, Vec<SourceRedaction>) {
    let source = evidence(cx, node);
    let mut result = SourceExpression {
        source: source.clone(),
        syntax_kind: node.kind().into(),
        display: SanitizedDisplay::from_sanitized(UNSUPPORTED.into()),
        capture: ExpressionCapture::Unsupported,
        literal: None,
    };
    let Some(bytes) = cx.source.get(node.byte_range()) else {
        return withheld_expression(result, source, RedactionReason::UnsupportedSyntax);
    };
    let Ok(raw) = std::str::from_utf8(bytes) else {
        return withheld_expression(result, source, RedactionReason::UnsupportedDecoding);
    };
    if node.has_error() || node.is_missing() || bytes.len() > MAX_EXPRESSION_BYTES {
        return withheld_expression(result, source, RedactionReason::UnsupportedSyntax);
    }

    let mut replacements = vec![];
    let mut redactions = vec![];
    let mut stack = vec![node];
    let mut visited = 0;
    while let Some(current) = stack.pop() {
        visited += 1;
        if visited > MAX_EXPRESSION_NODES {
            return withheld_expression(result, source, RedactionReason::UnsupportedSyntax);
        }
        match current.kind() {
            "comment" => {
                replacements.push(Replacement {
                    start: current.start_byte(),
                    end: current.end_byte(),
                    text: " ",
                });
                redactions.push(SourceRedaction {
                    source: evidence(cx, current),
                    reason: RedactionReason::RemovedComment,
                });
                continue;
            }
            "true" | "false" | "null" => {
                if current == node {
                    result.literal = Some(LiteralEvidence::Known {
                        value: match current.kind() {
                            "true" => KnownLiteral::Boolean(true),
                            "false" => KnownLiteral::Boolean(false),
                            _ => KnownLiteral::Null,
                        },
                    });
                }
                continue;
            }
            "string" | "number" => {
                let kind = if current.kind() == "string" {
                    LiteralKind::String
                } else {
                    LiteralKind::Number
                };
                let decoded = if kind == LiteralKind::String {
                    decode_string(cx.text(&current)).map(KnownLiteral::String)
                } else {
                    decode_number(cx.text(&current)).map(KnownLiteral::Number)
                };
                let Ok(decoded) = decoded else {
                    if current == node {
                        result.literal = Some(LiteralEvidence::Withheld {
                            kind,
                            reasons: vec![RedactionReason::UnsupportedDecoding],
                        });
                    }
                    return withheld_expression(
                        result,
                        source,
                        RedactionReason::UnsupportedDecoding,
                    );
                };
                let inspected = match &decoded {
                    KnownLiteral::String(value) => value.as_str(),
                    _ => cx.text(&current),
                };
                let mut reasons = secret_reasons(inspected);
                match sensitive_context(cx, current) {
                    Ok(true) if !reasons.contains(&RedactionReason::CredentialValue) => {
                        reasons.push(RedactionReason::CredentialValue);
                    }
                    Ok(_) => {}
                    Err(()) => {
                        if current == node {
                            result.literal = Some(LiteralEvidence::Withheld {
                                kind,
                                reasons: vec![RedactionReason::UnsupportedSyntax],
                            });
                        }
                        return withheld_expression(
                            result,
                            source,
                            RedactionReason::UnsupportedSyntax,
                        );
                    }
                }
                if reasons.is_empty() {
                    if current == node {
                        result.literal = Some(LiteralEvidence::Known { value: decoded });
                    }
                } else {
                    replacements.push(Replacement {
                        start: current.start_byte(),
                        end: current.end_byte(),
                        text: WITHHELD,
                    });
                    for reason in &reasons {
                        redactions.push(SourceRedaction {
                            source: evidence(cx, current),
                            reason: *reason,
                        });
                    }
                    if current == node {
                        result.literal = Some(LiteralEvidence::Withheld { kind, reasons });
                    }
                }
                continue;
            }
            "identifier" | "property_identifier" | "shorthand_property_identifier" => {
                let text = cx.text(&current);
                // Escaped/unicode identifier interpretation is outside this
                // policy. Do not let encoded property keys bypass name checks.
                if text.is_empty()
                    || !text
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$'))
                {
                    return withheld_expression(result, source, RedactionReason::UnsupportedSyntax);
                }
                let reasons = recognized_secrets(text);
                if !reasons.is_empty() {
                    replacements.push(Replacement {
                        start: current.start_byte(),
                        end: current.end_byte(),
                        text: WITHHELD,
                    });
                    for reason in reasons {
                        redactions.push(SourceRedaction {
                            source: evidence(cx, current),
                            reason: redaction_reason(reason),
                        });
                    }
                }
                continue;
            }
            "this" | "super" => continue,
            // Templates (including chunks/interpolation), regexes, JSX,
            // callable bodies, and TS type syntax are intentionally absent.
            // Their display is withheld wholesale until a safe policy exists.
            kind if current.is_named() && supported_container(kind) => {}
            kind if !current.is_named() && supported_token(kind) => continue,
            _ => {
                return withheld_expression(result, source, RedactionReason::UnsupportedSyntax);
            }
        }
        let mut cursor = current.walk();
        stack.extend(current.children(&mut cursor));
    }

    replacements.sort_by_key(|replacement| replacement.start);
    redactions.sort_by_key(|redaction| (redaction.source.byte_start, redaction.source.byte_end));
    let mut display = String::new();
    let mut copied_to = 0;
    for replacement in replacements {
        let start = replacement.start.saturating_sub(node.start_byte());
        let end = replacement.end.saturating_sub(node.start_byte());
        let Some(prefix) = raw.get(copied_to..start) else {
            return withheld_expression(result, source, RedactionReason::UnsupportedSyntax);
        };
        display.push_str(prefix);
        display.push_str(replacement.text);
        copied_to = end;
    }
    let Some(tail) = raw.get(copied_to..) else {
        return withheld_expression(result, source, RedactionReason::UnsupportedSyntax);
    };
    display.push_str(tail);
    result.display = SanitizedDisplay::from_sanitized(display);
    result.capture = if redactions.is_empty() {
        ExpressionCapture::CompleteSyntax
    } else {
        ExpressionCapture::Redacted
    };
    (result, redactions)
}

fn evidence(cx: &FileCx<'_>, node: TsNode<'_>) -> EvidenceRef {
    EvidenceRef {
        repo: cx.id.repo.into(),
        path: cx.path.into(),
        byte_start: node.start_byte() as u64,
        byte_end: node.end_byte() as u64,
        commit_sha: cx.id.commit.into(),
    }
}

fn withheld_expression(
    mut expression: SourceExpression,
    source: EvidenceRef,
    reason: RedactionReason,
) -> (SourceExpression, Vec<SourceRedaction>) {
    expression.display = SanitizedDisplay::from_sanitized(UNSUPPORTED.into());
    expression.capture = ExpressionCapture::Unsupported;
    (expression, vec![SourceRedaction { source, reason }])
}

fn redaction_reason(kind: SecretKind) -> RedactionReason {
    match kind {
        SecretKind::PrivateKey => RedactionReason::PrivateKey,
        SecretKind::BearerToken => RedactionReason::BearerToken,
        SecretKind::ProviderToken => RedactionReason::ProviderToken,
        SecretKind::AwsAccessKey => RedactionReason::AwsAccessKey,
        SecretKind::CredentialAssignment => RedactionReason::CredentialValue,
    }
}

fn secret_reasons(value: &str) -> Vec<RedactionReason> {
    let mut reasons: Vec<_> = recognized_secrets(value)
        .into_iter()
        .map(redaction_reason)
        .collect();
    if is_token_shaped(value) {
        reasons.push(RedactionReason::TokenShaped);
    }
    reasons
}

fn supported_container(kind: &str) -> bool {
    matches!(
        kind,
        "parenthesized_expression"
            | "binary_expression"
            | "unary_expression"
            | "ternary_expression"
            | "member_expression"
            | "subscript_expression"
            | "call_expression"
            | "new_expression"
            | "arguments"
            | "array"
            | "object"
            | "pair"
            | "computed_property_name"
            | "spread_element"
            | "await_expression"
            | "sequence_expression"
            | "assignment_expression"
            | "augmented_assignment_expression"
            | "update_expression"
            | "optional_chain"
    )
}

fn supported_token(kind: &str) -> bool {
    matches!(
        kind,
        "(" | ")"
            | "["
            | "]"
            | "{"
            | "}"
            | "."
            | ","
            | ":"
            | "?"
            | "?."
            | "+"
            | "-"
            | "*"
            | "/"
            | "%"
            | "**"
            | "!"
            | "~"
            | "&"
            | "|"
            | "^"
            | "&&"
            | "||"
            | "??"
            | "=="
            | "==="
            | "!="
            | "!=="
            | "<"
            | ">"
            | "<="
            | ">="
            | "<<"
            | ">>"
            | ">>>"
            | "in"
            | "instanceof"
            | "typeof"
            | "void"
            | "delete"
            | "await"
            | "new"
            | "..."
            | "="
            | "+="
            | "-="
            | "*="
            | "/="
            | "%="
            | "**="
            | "&="
            | "|="
            | "^="
            | "<<="
            | ">>="
            | ">>>="
            | "&&="
            | "||="
            | "??="
            | "++"
            | "--"
    )
}

fn sensitive_context(cx: &FileCx<'_>, mut child: TsNode<'_>) -> Result<bool, ()> {
    for _ in 0..MAX_CONTEXT_ANCESTORS {
        let Some(parent) = child.parent() else {
            return Ok(false);
        };
        match parent.kind() {
            "function_declaration"
            | "function_expression"
            | "arrow_function"
            | "method_definition" => {
                return Ok(false);
            }
            "variable_declarator" => {
                if parent.child_by_field_name("value") == Some(child)
                    && let Some(key) = parent.child_by_field_name("name")
                    && sensitive_key(cx, key)?
                {
                    return Ok(true);
                }
            }
            "pair" => {
                if parent.child_by_field_name("value") == Some(child)
                    && let Some(key) = parent.child_by_field_name("key")
                    && sensitive_key(cx, key)?
                {
                    return Ok(true);
                }
            }
            "assignment_expression" | "augmented_assignment_expression" | "binary_expression" => {
                let left = parent.child_by_field_name("left");
                let right = parent.child_by_field_name("right");
                if right == Some(child)
                    && let Some(key) = left
                    && sensitive_key(cx, key)?
                {
                    return Ok(true);
                }
                if parent.kind() == "binary_expression"
                    && left == Some(child)
                    && let Some(key) = right
                    && sensitive_key(cx, key)?
                {
                    return Ok(true);
                }
            }
            _ => {}
        }
        child = parent;
    }
    // Deep ancestry cannot be inspected within the budget: withhold values
    // conservatively rather than presuming there is no sensitive context.
    Err(())
}

fn sensitive_key(cx: &FileCx<'_>, mut node: TsNode<'_>) -> Result<bool, ()> {
    for _ in 0..MAX_CONTEXT_ANCESTORS {
        match node.kind() {
            "identifier" | "property_identifier" => {
                let raw = cx.text(&node);
                let decoded = if raw.contains('\\') {
                    // Identifier escapes are decoded transiently for the key
                    // relationship. Their raw spelling is never added to facts.
                    decode_string(&format!("'{raw}'"))?
                } else {
                    raw.into()
                };
                return Ok(is_sensitive_name(&decoded));
            }
            "string" => return Ok(is_sensitive_name(&decode_string(cx.text(&node))?)),
            "member_expression" => node = node.child_by_field_name("property").ok_or(())?,
            "subscript_expression" => {
                let key = node.child_by_field_name("index").ok_or(())?;
                if key.kind() != "string" {
                    return Ok(false);
                }
                node = key;
            }
            "computed_property_name" | "parenthesized_expression" => {
                let mut cursor = node.walk();
                let key = node
                    .named_children(&mut cursor)
                    .find(|child| child.kind() != "comment")
                    .ok_or(())?;
                if node.kind() == "computed_property_name" && key.kind() != "string" {
                    return Ok(false);
                }
                node = key;
            }
            _ => return Ok(false),
        }
    }
    Err(())
}

fn decode_number(raw: &str) -> Result<serde_json::Number, ()> {
    if (raw.starts_with('0') && raw.as_bytes().get(1).is_some_and(u8::is_ascii_digit))
        || !raw.chars().all(|character| {
            character.is_ascii_digit() || matches!(character, '.' | 'e' | 'E' | '+' | '-')
        })
    {
        return Err(());
    }
    // Source numeric literals have JavaScript Number semantics. Unsupported
    // radix, separator, bigint, or nonfinite forms are never guessed.
    serde_json::Number::from_f64(raw.parse::<f64>().map_err(|_| ())?).ok_or(())
}

fn decode_string(raw: &str) -> Result<String, ()> {
    let quote = raw.chars().next().ok_or(())?;
    if !matches!(quote, '\'' | '"') || !raw.ends_with(quote) || raw.len() < 2 {
        return Err(());
    }
    let mut chars = raw[1..raw.len() - 1].chars().peekable();
    let mut decoded = String::new();
    while let Some(character) = chars.next() {
        if matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
            return Err(());
        }
        if character != '\\' {
            decoded.push(character);
            continue;
        }
        match chars.next().ok_or(())? {
            '\\' => decoded.push('\\'),
            '\'' => decoded.push('\''),
            '"' => decoded.push('"'),
            'n' => decoded.push('\n'),
            'r' => decoded.push('\r'),
            't' => decoded.push('\t'),
            'b' => decoded.push('\u{0008}'),
            'f' => decoded.push('\u{000c}'),
            'v' => decoded.push('\u{000b}'),
            '0' if !chars.peek().is_some_and(char::is_ascii_digit) => decoded.push('\0'),
            '\n' => {}
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
            }
            'x' => decoded.push(char::from_u32(read_hex(&mut chars, 2)?).ok_or(())?),
            'u' => {
                let code = if chars.peek() == Some(&'{') {
                    chars.next();
                    let mut digits = String::new();
                    loop {
                        match chars.next().ok_or(())? {
                            '}' if !digits.is_empty() => break,
                            digit if digit.is_ascii_hexdigit() && digits.len() < 6 => {
                                digits.push(digit)
                            }
                            _ => return Err(()),
                        }
                    }
                    u32::from_str_radix(&digits, 16).map_err(|_| ())?
                } else {
                    let first = read_hex(&mut chars, 4)?;
                    if (0xd800..=0xdbff).contains(&first) {
                        if chars.next() != Some('\\') || chars.next() != Some('u') {
                            return Err(());
                        }
                        let second = read_hex(&mut chars, 4)?;
                        if !(0xdc00..=0xdfff).contains(&second) {
                            return Err(());
                        }
                        0x10000 + ((first - 0xd800) << 10) + second - 0xdc00
                    } else {
                        first
                    }
                };
                decoded.push(char::from_u32(code).ok_or(())?);
            }
            // Legacy octal and other escape forms need separate proof. Never
            // return their original encoded bytes as a decoding fallback.
            _ => return Err(()),
        }
    }
    Ok(decoded)
}

fn read_hex(chars: &mut impl Iterator<Item = char>, count: usize) -> Result<u32, ()> {
    let mut value = 0;
    for _ in 0..count {
        value = value * 16
            + chars
                .next()
                .and_then(|character| character.to_digit(16))
                .ok_or(())?;
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SourceId;
    use tree_sitter::Parser;

    fn capture(
        source: &str,
        target_kind: &str,
        field: Option<&str>,
    ) -> (SourceExpression, Vec<SourceRedaction>) {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let mut stack = vec![tree.root_node()];
        let target = loop {
            let node = stack.pop().expect("fixture target exists");
            if node.kind() == target_kind {
                break if let Some(field) = field {
                    node.child_by_field_name(field)
                        .expect("fixture field exists")
                } else {
                    node.named_child(0).expect("fixture expression exists")
                };
            }
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
        };
        let id = SourceId {
            repo: "example/rules",
            commit: "pinned-revision",
        };
        let cx = FileCx {
            source: source.as_bytes(),
            path: "src/rules.tsx",
            id: &id,
        };
        sanitize_expression(&cx, target)
    }

    fn returned(expression: &str) -> (SourceExpression, Vec<SourceRedaction>) {
        capture(
            &format!("function run() {{ return {expression}; }}"),
            "return_statement",
            None,
        )
    }

    #[test]
    fn safe_source_preserves_operators_boolean_and_string_literal_types() {
        // AC-0122/AC-0124: source redaction does not rewrite JS truthiness,
        // strict operators, or a literal boolean into business prose/text.
        let source = "options.enabled !== false && (!order.shippingLines || order.shippingLines.length === 0)";
        let (expression, redactions) = returned(source);
        assert_eq!(expression.display.as_str(), source);
        assert_eq!(expression.capture, ExpressionCapture::CompleteSyntax);
        assert!(redactions.is_empty());
        for (source, expected) in [
            ("const password = false;", KnownLiteral::Boolean(false)),
            ("const secret = null;", KnownLiteral::Null),
        ] {
            let (expression, redactions) = capture(source, "variable_declarator", Some("value"));
            assert_eq!(
                expression.literal,
                Some(LiteralEvidence::Known { value: expected })
            );
            assert!(redactions.is_empty());
        }
        let (expression, _) = returned("'not-ready'");
        assert_eq!(
            expression.literal,
            Some(LiteralEvidence::Known {
                value: KnownLiteral::String("not-ready".into())
            })
        );
    }

    #[test]
    fn encoded_tokens_are_withheld_before_display_and_literal_serialization() {
        // AC-0124: literal decoding precedes shared recognition; neither raw
        // escaped text nor decoded credentials are stored as a fallback.
        for encoded in [
            r"'\x73\x6b-abcdefgh1234'",
            r"'\u0073\u006b-abcdefgh1234'",
            r"'\u{73}\u{6b}-abcdefgh1234'",
            r"'\x67hp_abcdefgh5678'",
        ] {
            let (expression, redactions) = returned(encoded);
            assert_eq!(expression.capture, ExpressionCapture::Redacted);
            assert_eq!(expression.display.as_str(), WITHHELD);
            assert!(matches!(
                expression.literal,
                Some(LiteralEvidence::Withheld {
                    kind: LiteralKind::String,
                    ..
                })
            ));
            assert!(
                redactions
                    .iter()
                    .any(|redaction| redaction.reason == RedactionReason::ProviderToken)
            );
            let wire = serde_json::to_string(&(expression, redactions)).unwrap();
            assert!(!wire.contains("abcdefgh1234"));
            assert!(!wire.contains("abcdefgh5678"));
            assert!(!wire.contains("sk-"));
        }
    }

    #[test]
    fn proven_credential_context_withholds_short_values_without_name_guessing() {
        // AC-0124: AST-proven key/value relationships protect short values
        // outside free-text pattern thresholds; similar names are not proof.
        for source in [
            "const password = 'tiny';",
            r"const \u0070assword = 'tiny';",
            "const config = { password: 'tiny' };",
            "const config = { password: 'tiny', apiKey: 'abc' };",
            r"const config = { '\u0070assword': 'tiny' };",
            "const config = { ['password']: 'tiny' };",
            "const config = { nested: { authToken: 'tiny' } };",
            "const password = ['tiny'];",
        ] {
            let (expression, redactions) = capture(source, "variable_declarator", Some("value"));
            let wire = serde_json::to_string(&(expression, redactions)).unwrap();
            assert!(!wire.contains("tiny"));
            assert!(!wire.contains("abc"));
            assert!(wire.contains("credential_value"));
        }
        let (expression, _) = returned("input['password'] === 'tiny'");
        assert!(!expression.display.as_str().contains("tiny"));
        let (expression, _) = capture(
            "const passwordRequired = 'ordinary';",
            "variable_declarator",
            Some("value"),
        );
        assert_eq!(expression.display.as_str(), "'ordinary'");
    }

    #[test]
    fn comments_are_removed_and_source_references_keep_original_byte_offsets() {
        // AC-0124: comments inside source expressions cannot bypass literal
        // policy, and removing text must not rewrite the evidence reference.
        let raw = "ready /* password=comment-secret */ && false";
        let source = format!("// café\nfunction run() {{ return {raw}; }}");
        let (expression, redactions) = capture(&source, "return_statement", None);
        assert_eq!(expression.capture, ExpressionCapture::Redacted);
        assert!(!expression.display.as_str().contains("comment-secret"));
        let start = source.find(raw).unwrap() as u64;
        assert_eq!(expression.source.byte_start, start);
        assert_eq!(expression.source.byte_end, start + raw.len() as u64);
        assert_eq!(redactions.len(), 1);
        assert_eq!(redactions[0].reason, RedactionReason::RemovedComment);
        let wire = serde_json::to_string(&(expression, redactions)).unwrap();
        assert!(!wire.contains("comment-secret"));
    }

    #[test]
    fn unsupported_syntax_and_decoding_never_copy_raw_source_as_fallback() {
        // AC-0123/AC-0124: unsupported expressions stay explicit; regex,
        // templates, JSX, and undecodable values cannot enter raw properties.
        for source in [
            "`template-secret-${value}`",
            "/regex_secret_123456/",
            "<div data-key='jsx-secret' />",
            r"'\uD800undecodable-secret'",
            "(() => 'callback-secret')()",
            "0xffffffffffffffffffffn",
        ] {
            let (expression, redactions) = returned(source);
            assert_eq!(expression.capture, ExpressionCapture::Unsupported);
            assert_eq!(expression.display.as_str(), UNSUPPORTED);
            assert!(!redactions.is_empty());
            let wire = serde_json::to_string(&(expression, redactions)).unwrap();
            for value in [
                "template-secret",
                "regex_secret",
                "jsx-secret",
                "undecodable-secret",
                "callback-secret",
                "0xffff",
            ] {
                assert!(!wire.contains(value));
            }
        }
    }

    #[test]
    fn conservative_shape_withholding_is_typed_and_deterministic() {
        // AC-0124: long business identifiers can be withheld conservatively,
        // but string type and references survive without invented semantics.
        let raw = "'message.cannot-start-action-without-required-context'";
        let first = returned(raw);
        let second = returned(raw);
        assert_eq!(first, second);
        assert_eq!(first.0.capture, ExpressionCapture::Redacted);
        assert!(matches!(
            first.0.literal,
            Some(LiteralEvidence::Withheld {
                kind: LiteralKind::String,
                ..
            })
        ));
        assert!(
            first
                .1
                .iter()
                .any(|redaction| redaction.reason == RedactionReason::TokenShaped)
        );
        let (unicode, redactions) = returned(r"'\uD83D\uDE80'");
        assert_eq!(
            unicode.literal,
            Some(LiteralEvidence::Known {
                value: KnownLiteral::String("🚀".into())
            })
        );
        assert!(redactions.is_empty());
    }
}
