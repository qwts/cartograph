//! Shared `tsconfig.json`/`jsconfig.json` compiler-options parsing — the
//! parse-once seam between the toolchain detector (#215) and Node/ESM
//! module resolution (#213). Both consumers quote the same parse: the
//! toolchain stores these facts on the `Tool` node, and the TS adapter's
//! bare-specifier resolution consumes `baseUrl`/`paths` citing the same
//! config file as evidence — one parser, so the two can never disagree.
//!
//! tsconfig is JSONC in the wild (comments, trailing commas). [`parse`]
//! sanitizes those before strict JSON parsing; an unparseable config
//! degrades to empty facts — never guessed settings.

use std::collections::BTreeMap;

/// The compiler options captured as facts. Everything else in the config
/// is ignored (allowlist — the redaction posture of AC-0097).
pub const SETTING_KEYS: &[&str] = &[
    "target",
    "module",
    "moduleResolution",
    "strict",
    "jsx",
    "baseUrl",
    "paths",
];

/// Facts recovered from one tsconfig/jsconfig text.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TsconfigFacts {
    /// Allowlisted `compilerOptions` (plus top-level `extends`), verbatim.
    pub settings: BTreeMap<String, serde_json::Value>,
    /// `compilerOptions.baseUrl`, config-directory-relative.
    pub base_url: Option<String>,
    /// `compilerOptions.paths` patterns in declaration order:
    /// `("@/*", ["src/*"])`. Empty when absent or unparseable.
    pub paths: Vec<(String, Vec<String>)>,
    /// Byte span of the `"compilerOptions"` key (whole file when absent) —
    /// the declaring evidence for settings facts.
    pub span: (u64, u64),
    /// Byte span of the `"paths"` key (falls back to [`Self::span`]) — the
    /// declaring evidence for alias resolutions.
    pub paths_span: (u64, u64),
}

/// First occurrence of `needle` as a byte span; whole text when absent.
fn span_of(text: &str, needle: &str) -> (u64, u64) {
    match text.find(needle) {
        Some(start) => (start as u64, (start + needle.len()) as u64),
        None => (0, text.len() as u64),
    }
}

/// Strip JSONC comments, string-aware.
fn strip_comments(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_string {
            out.push(c);
            if c == '\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
                i += 1;
            }
            '/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            '/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Remove trailing commas from comment-free JSON text, string-aware.
fn strip_trailing_commas(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_string {
            out.push(c);
            if c == '\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
        }
        if c == ',' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'}' || bytes[j] == b']') {
                i += 1; // drop the trailing comma
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Strip JSONC comments and trailing commas, string-aware, preserving
/// everything else byte-for-byte where possible (spans are computed on the
/// original text, so the sanitized copy is only ever parsed, never cited).
/// Comments are removed FIRST so a trailing comma followed by a comment
/// (`"baseUrl": ".", // why`) is still recognized as trailing (#220 review).
fn sanitize_jsonc(text: &str) -> String {
    strip_trailing_commas(&strip_comments(text))
}

/// Parse one tsconfig/jsconfig text into facts. Tolerates JSONC; fails
/// closed to empty facts when the config still doesn't parse.
pub fn parse(text: &str) -> TsconfigFacts {
    let mut facts = TsconfigFacts {
        span: span_of(text, "\"compilerOptions\""),
        ..Default::default()
    };
    facts.paths_span = facts.span;
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&sanitize_jsonc(text)) else {
        return facts;
    };
    for key in SETTING_KEYS {
        let value = &json["compilerOptions"][*key];
        if !value.is_null() {
            facts.settings.insert((*key).to_string(), value.clone());
        }
    }
    if let Some(extends) = json.get("extends").filter(|v| !v.is_null()) {
        facts.settings.insert("extends".into(), extends.clone());
    }
    facts.base_url = json["compilerOptions"]["baseUrl"]
        .as_str()
        .map(str::to_string);
    if let Some(paths) = json["compilerOptions"]["paths"].as_object() {
        facts.paths_span = span_of(text, "\"paths\"");
        for (pattern, targets) in paths {
            let targets: Vec<String> = targets
                .as_array()
                .map(|list| {
                    list.iter()
                        .filter_map(|t| t.as_str())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            facts.paths.push((pattern.clone(), targets));
        }
    }
    facts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_compiler_options_and_paths() {
        let facts = parse(
            r##"{
  "compilerOptions": {
    "target": "ES2022",
    "moduleResolution": "nodenext",
    "strict": true,
    "baseUrl": ".",
    "paths": { "@/*": ["src/*"], "#lib": ["lib/index.ts"] }
  }
}"##,
        );
        assert_eq!(facts.settings["strict"], serde_json::json!(true));
        assert_eq!(facts.base_url.as_deref(), Some("."));
        assert_eq!(
            facts.paths,
            vec![
                ("#lib".to_string(), vec!["lib/index.ts".to_string()]),
                ("@/*".to_string(), vec!["src/*".to_string()]),
            ]
        );
    }

    #[test]
    fn tolerates_jsonc_comments_and_trailing_commas() {
        // Real-world tsconfigs are JSONC; the parse must not fail closed on
        // a comment. Spans still index the original text.
        let text = r#"{
  // project config
  "compilerOptions": {
    "strict": true, /* always */
    "paths": {
      "@/*": ["src/*"],
    },
  },
}"#;
        let facts = parse(text);
        assert_eq!(facts.settings["strict"], serde_json::json!(true));
        assert_eq!(
            facts.paths,
            vec![("@/*".to_string(), vec!["src/*".to_string()])]
        );
        let (start, end) = facts.paths_span;
        assert_eq!(&text[start as usize..end as usize], "\"paths\"");
    }

    #[test]
    fn trailing_comma_before_a_comment_is_still_trailing() {
        // #220 review: `"baseUrl": ".", // why` before the closing brace is
        // a common tsconfig pattern — comments are stripped first, so the
        // comma is recognized as trailing and the config still parses.
        let facts = parse(
            "{\n  \"compilerOptions\": {\n    \"baseUrl\": \".\", // why\n    \"paths\": { \"@/*\": [\"src/*\"] }, /* done */\n  },\n}",
        );
        assert_eq!(facts.base_url.as_deref(), Some("."));
        assert_eq!(
            facts.paths,
            vec![("@/*".to_string(), vec!["src/*".to_string()])]
        );
    }

    #[test]
    fn unparseable_config_fails_closed_to_empty_facts() {
        let facts = parse("{ not json at all");
        assert!(facts.settings.is_empty());
        assert!(facts.paths.is_empty());
        assert_eq!(facts.base_url, None);
    }

    #[test]
    fn comment_slashes_inside_strings_survive() {
        let facts = parse(r#"{ "compilerOptions": { "baseUrl": "./src//x" } }"#);
        assert_eq!(facts.base_url.as_deref(), Some("./src//x"));
    }
}
