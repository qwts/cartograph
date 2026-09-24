//! Literal bundler `resolve.alias` keys (#463, AC-0220): Vite and webpack
//! configs are read as data, never executed, so a bundler-only alias
//! spelled like a package name (`components` → `./app/components`) is the
//! repository's own specifier rather than a Confirmed external package.
//!
//! Only literal keys whose target is provably a filesystem path count: a
//! relative string literal, `path.resolve`/`path.join` over `__dirname`
//! and string literals, or `fileURLToPath(new URL('…', import.meta.url))`.
//! A key whose target is a bare package name (`path` → `path-browserify`)
//! or anything non-literal (a variable, a computed key, a regex `find`)
//! is skipped and keeps today's classification (ADR-0031).

use tree_sitter::{Node as TsNode, Parser};

/// One literal alias declared by a bundler config.
#[derive(Debug, Clone)]
pub(crate) struct BundlerAlias {
    /// Repo-relative directory of the config (`""` at the root).
    pub(crate) dir: String,
    /// Repo-relative path of the config file (the cited evidence).
    pub(crate) config_path: String,
    /// The alias key, without webpack's exact-match `$` suffix.
    pub(crate) key: String,
    /// webpack `key$`: matches the bare key only, never `key/sub`.
    pub(crate) exact: bool,
    /// Repo-relative replacement path, when every part of it is literal
    /// and it stays inside the repository; `None` for a path-shaped target
    /// T0 cannot place (an absolute path, a non-literal segment).
    pub(crate) target: Option<String>,
    /// Declaring span of the key in the config text.
    pub(crate) span: (u64, u64),
}

impl BundlerAlias {
    /// Whether `spec` is addressed by this alias: the key itself, or (for
    /// a non-exact key) the key followed by a `/` subpath — the matching
    /// rule both Vite's alias plugin and webpack apply to string keys.
    pub(crate) fn matches(&self, spec: &str) -> bool {
        spec == self.key
            || (!self.exact
                && spec
                    .strip_prefix(&self.key)
                    .is_some_and(|rest| rest.starts_with('/')))
    }
}

/// Whether a file name is a Vite or webpack config.
pub(crate) fn is_bundler_config(name: &str) -> bool {
    const EXTENSIONS: &[&str] = &["js", "mjs", "cjs", "ts", "mts", "cts"];
    ["vite.config.", "webpack.config."].iter().any(|stem| {
        name.strip_prefix(stem)
            .is_some_and(|ext| EXTENSIONS.contains(&ext))
    })
}

/// Every literal `resolve.alias` entry in one config, in declaration order.
pub(crate) fn parse(text: &str, dir: &str, config_path: &str) -> Vec<BundlerAlias> {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
        .is_err()
    {
        return Vec::new();
    }
    let Some(tree) = parser.parse(text, None) else {
        return Vec::new();
    };
    let cx = Cx {
        text,
        dir,
        config_path,
    };
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "pair"
            && cx.key(&node).as_deref() == Some("alias")
            && is_resolve_object(&cx, &node)
            && let Some(value) = node.child_by_field_name("value")
        {
            cx.aliases(&value, &mut out);
        }
        // Reverse push keeps a depth-first walk in source order.
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    out
}

/// Whether the `alias` pair sits directly in the object of a `resolve` key.
fn is_resolve_object(cx: &Cx, pair: &TsNode) -> bool {
    pair.parent()
        .filter(|object| object.kind() == "object")
        .and_then(|object| object.parent())
        .filter(|parent| parent.kind() == "pair")
        .is_some_and(|parent| cx.key(&parent).as_deref() == Some("resolve"))
}

struct Cx<'a> {
    text: &'a str,
    dir: &'a str,
    config_path: &'a str,
}

impl Cx<'_> {
    fn text_of(&self, node: &TsNode) -> &str {
        &self.text[node.byte_range()]
    }

    /// The literal key of a `pair` (identifier or string); `None` for a
    /// computed key.
    fn key(&self, pair: &TsNode) -> Option<String> {
        let key = pair.child_by_field_name("key")?;
        match key.kind() {
            "property_identifier" => Some(self.text_of(&key).to_string()),
            "string" => self.string(&key),
            _ => None,
        }
    }

    /// The value of a plain string literal (no escapes, no template).
    fn string(&self, node: &TsNode) -> Option<String> {
        if node.kind() != "string" {
            return None;
        }
        let raw = self.text_of(node);
        let inner = raw.get(1..raw.len().checked_sub(1)?)?;
        (!inner.contains('\\')).then(|| inner.to_string())
    }

    fn aliases(&self, value: &TsNode, out: &mut Vec<BundlerAlias>) {
        let mut cursor = value.walk();
        match value.kind() {
            // `alias: { key: target, … }`
            "object" => {
                for pair in value.named_children(&mut cursor) {
                    if pair.kind() != "pair" {
                        continue;
                    }
                    let (Some(key), Some(key_node), Some(target)) = (
                        self.key(&pair),
                        pair.child_by_field_name("key"),
                        pair.child_by_field_name("value"),
                    ) else {
                        continue;
                    };
                    self.push(key, &key_node, &target, out);
                }
            }
            // `alias: [{ find: 'key', replacement: target }, …]` (Vite).
            "array" => {
                for entry in value.named_children(&mut cursor) {
                    if entry.kind() != "object" {
                        continue;
                    }
                    let mut find = None;
                    let mut replacement = None;
                    let mut inner = entry.walk();
                    for pair in entry.named_children(&mut inner) {
                        if pair.kind() != "pair" {
                            continue;
                        }
                        match self.key(&pair).as_deref() {
                            Some("find") => find = pair.child_by_field_name("value"),
                            Some("replacement") => {
                                replacement = pair.child_by_field_name("value");
                            }
                            _ => {}
                        }
                    }
                    // A regex `find` is non-literal: skipped.
                    let (Some(find), Some(replacement)) = (find, replacement) else {
                        continue;
                    };
                    if let Some(key) = self.string(&find) {
                        self.push(key, &find, &replacement, out);
                    }
                }
            }
            _ => {}
        }
    }

    fn push(&self, key: String, key_node: &TsNode, target: &TsNode, out: &mut Vec<BundlerAlias>) {
        let Some(target) = self.target(target) else {
            return;
        };
        let (key, exact) = match key.strip_suffix('$') {
            Some(stripped) => (stripped.to_string(), true),
            None => (key, false),
        };
        if key.is_empty() {
            return;
        }
        out.push(BundlerAlias {
            dir: self.dir.to_string(),
            config_path: self.config_path.to_string(),
            key,
            exact,
            target,
            span: (key_node.start_byte() as u64, key_node.end_byte() as u64),
        });
    }

    /// `Some(Some(path))`: a literal in-repo path; `Some(None)`: provably a
    /// filesystem path T0 cannot place; `None`: not provably a path at all
    /// (a package name, a variable) — the key is skipped.
    fn target(&self, node: &TsNode) -> Option<Option<String>> {
        match node.kind() {
            "string" => {
                let value = self.string(node)?;
                if value == "." || value.starts_with("./") || value.starts_with("../") {
                    Some(self.join(&[value.as_str()]))
                } else if value.starts_with('/') {
                    Some(None)
                } else {
                    None
                }
            }
            "call_expression" => {
                let callee = self.text_of(&node.child_by_field_name("function")?);
                let args = node.child_by_field_name("arguments")?;
                let mut cursor = args.walk();
                let args: Vec<_> = args.named_children(&mut cursor).collect();
                match callee {
                    "path.resolve" | "path.join" | "resolve" | "join" | "path.posix.resolve"
                    | "path.posix.join" => Some(self.path_call(&args)),
                    "fileURLToPath" | "url.fileURLToPath" => {
                        let url = args.first()?;
                        (url.kind() == "new_expression").then_some(())?;
                        let class = url.child_by_field_name("constructor")?;
                        (self.text_of(&class) == "URL").then_some(())?;
                        let url_args = url.child_by_field_name("arguments")?;
                        let mut cursor = url_args.walk();
                        let url_args: Vec<_> = url_args.named_children(&mut cursor).collect();
                        let [relative, base] = url_args.as_slice() else {
                            return Some(None);
                        };
                        if self.text_of(base) != "import.meta.url" {
                            return Some(None);
                        }
                        match self.string(relative) {
                            Some(value) if !value.starts_with('/') => {
                                Some(self.join(&[value.as_str()]))
                            }
                            _ => Some(None),
                        }
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// `path.resolve(__dirname, 'a', 'b')`: config-relative only when the
    /// first argument is the config's own directory and the rest are
    /// relative string literals.
    fn path_call(&self, args: &[TsNode]) -> Option<String> {
        let (first, rest) = args.split_first()?;
        if !matches!(self.text_of(first), "__dirname" | "import.meta.dirname") {
            return None;
        }
        let mut parts = Vec::new();
        for arg in rest {
            let value = self.string(arg)?;
            if value.starts_with('/') {
                return None;
            }
            parts.push(value);
        }
        let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
        self.join(&parts)
    }

    /// Join config-relative parts onto the config directory; `None` when
    /// the result climbs above the repository root.
    fn join(&self, parts: &[&str]) -> Option<String> {
        let mut segments: Vec<&str> = self.dir.split('/').filter(|s| !s.is_empty()).collect();
        for part in parts {
            for segment in part.split('/') {
                match segment {
                    "" | "." => {}
                    ".." => {
                        segments.pop()?;
                    }
                    other => segments.push(other),
                }
            }
        }
        Some(segments.join("/"))
    }
}
