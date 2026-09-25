//! Binding-proven const-string resolution shared by the WebExtension
//! passes (US-0016). A member expression like `MessageType.Ping` resolves
//! to a literal only when the extractor can prove which definition is in
//! scope: the const object is declared in the same file, or it is imported
//! (possibly aliased) through a relative specifier that resolves to a repo
//! file exporting that object. A bare-package import, a parameter, or any
//! other unproven binding stays unresolved — fail closed, never a
//! same-name coincidence elsewhere in the repo (#149 review, AC-0072).

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node as TsNode, Query, QueryCursor};

use crate::chrome_messaging::unwrap_assertions;
use crate::{FileCx, literal_string, object_entries};

/// One file's import bindings: local name → (specifier, original name).
pub(crate) type ImportBindings = BTreeMap<String, (String, String)>;

/// Repo-wide, file-keyed definitions for binding-proven resolution.
#[derive(Default)]
pub(crate) struct ConstIndex {
    /// (file, `Obj.Member`) → literal value.
    defs: BTreeMap<(String, String), String>,
    /// (file, `Obj`) — the object is exported from that file.
    exported: BTreeMap<(String, String), ()>,
    /// file → import bindings.
    imports: BTreeMap<String, ImportBindings>,
}

impl ConstIndex {
    /// Record a const-object member defined in `file`.
    pub(crate) fn define(&mut self, file: &str, member: String, value: String, exported: bool) {
        if exported {
            let object = member.split('.').next().unwrap_or(&member).to_string();
            self.exported.insert((file.to_string(), object), ());
        }
        self.defs.insert((file.to_string(), member), value);
    }

    pub(crate) fn record_imports(&mut self, file: &str, bindings: ImportBindings) {
        self.imports.insert(file.to_string(), bindings);
    }

    /// Resolve `Obj.Prop` as seen from `file`: same-file definition, or an
    /// import-proven definition in the exact target module. `None` when the
    /// binding cannot be proven.
    pub(crate) fn resolve_member(&self, file: &str, member: &str) -> Option<String> {
        let (object, prop) = member.split_once('.')?;
        if prop.contains('.') {
            return None; // deeper paths are not a const-map member
        }
        if let Some(value) = self.defs.get(&(file.to_string(), member.to_string())) {
            return Some(value.clone());
        }
        let (spec, original) = self.imports.get(file)?.get(object)?;
        for candidate in module_candidates(file, spec) {
            let key = (candidate.clone(), format!("{original}.{prop}"));
            if self
                .exported
                .contains_key(&(candidate.clone(), original.clone()))
                && let Some(value) = self.defs.get(&key)
            {
                return Some(value.clone());
            }
        }
        None
    }

    /// Prove which repo file defines the imported binding `name` as seen
    /// from `file`, returning `(defining_file, original_name)`. Same-file
    /// callers should check their own definitions first.
    pub(crate) fn prove_import(
        &self,
        file: &str,
        name: &str,
        defined_in: impl Fn(&str, &str) -> bool,
    ) -> Option<(String, String)> {
        let (spec, original) = self.imports.get(file)?.get(name)?;
        module_candidates(file, spec)
            .into_iter()
            .find(|candidate| defined_in(candidate, original))
            .map(|candidate| (candidate, original.clone()))
    }
}

/// Candidate repo-relative files a relative specifier may resolve to
/// (NodeNext `.js` specifiers point at `.ts`/`.tsx` sources). Bare package
/// specifiers resolve to nothing — outside the repo, outside T0 proof.
pub(crate) fn module_candidates(from: &str, spec: &str) -> Vec<String> {
    if !spec.starts_with('.') {
        return Vec::new();
    }
    let dir = Path::new(from).parent().unwrap_or(Path::new(""));
    let mut normalized = PathBuf::new();
    for comp in dir.join(spec).components() {
        match comp {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other),
        }
    }
    let joined = normalized.to_string_lossy().replace('\\', "/");
    if joined.ends_with(".ts") || joined.ends_with(".tsx") {
        return vec![joined];
    }
    let base = joined
        .strip_suffix(".js")
        .or_else(|| joined.strip_suffix(".mjs"))
        .unwrap_or(&joined);
    vec![
        format!("{base}.ts"),
        format!("{base}.tsx"),
        format!("{base}/index.ts"),
    ]
}

/// True when `node` (a `variable_declarator` or `function_declaration`)
/// sits under an `export` statement.
pub(crate) fn is_exported(node: TsNode) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "export_statement" {
            return true;
        }
        if matches!(parent.kind(), "statement_block" | "program") {
            return false;
        }
        current = parent;
    }
    false
}

/// Collect one file's import bindings (named + aliased + default +
/// namespace) into the index. Also reports whether any binding names
/// `chrome` — a shadow of the platform global.
pub(crate) fn collect_imports(
    cx: &FileCx,
    root: TsNode,
    language: &tree_sitter::Language,
    index: &mut ConstIndex,
) -> bool {
    let query = Query::new(
        language,
        r#"
        (import_statement
            (import_clause
                [
                    (identifier) @default
                    (named_imports (import_specifier
                        name: (identifier) @name
                        alias: (identifier)? @alias))
                    (namespace_import (identifier) @namespace)
                ])
            source: (string) @source)
        "#,
    )
    .expect("static query");
    let mut bindings = ImportBindings::new();
    let mut shadows_chrome = false;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, cx.source);
    while let Some(m) = matches.next() {
        let (mut default, mut name, mut alias, mut namespace, mut source) =
            (None, None, None, None, None);
        for c in m.captures() {
            let text = cx.text(&c.node).to_string();
            match query.capture_names()[c.index as usize] {
                "default" => default = Some(text),
                "name" => name = Some(text),
                "alias" => alias = Some(text),
                "namespace" => namespace = Some(text),
                "source" => source = Some(text.trim_matches(['"', '\'']).to_string()),
                _ => {}
            }
        }
        let Some(source) = source else { continue };
        if let Some(original) = name {
            let local = alias.unwrap_or_else(|| original.clone());
            shadows_chrome |= local == "chrome";
            bindings.insert(local, (source.clone(), original));
        }
        for local in [default, namespace].into_iter().flatten() {
            shadows_chrome |= local == "chrome";
            bindings.insert(local.clone(), (source.clone(), local));
        }
    }
    index.record_imports(cx.path, bindings);
    shadows_chrome
}

/// Collect one file's const-object string members into the index.
pub(crate) fn collect_const_objects(
    cx: &FileCx,
    root: TsNode,
    language: &tree_sitter::Language,
    index: &mut ConstIndex,
) {
    let query = Query::new(
        language,
        r#"(variable_declarator name: (identifier) @name value: (_) @value) @decl"#,
    )
    .expect("static query");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, cx.source);
    while let Some(m) = matches.next() {
        let (mut name, mut value, mut decl) = (None, None, None);
        for c in m.captures() {
            match query.capture_names()[c.index as usize] {
                "name" => name = Some(c.node),
                "value" => value = Some(c.node),
                "decl" => decl = Some(c.node),
                _ => {}
            }
        }
        let (Some(name), Some(value), Some(decl)) = (name, value, decl) else {
            continue;
        };
        let value = unwrap_assertions(value);
        if value.kind() != "object" {
            continue;
        }
        let name_text = cx.text(&name).to_string();
        let exported = is_exported(decl);
        for (key, entry) in object_entries(cx, value) {
            if let Some(lit) = literal_string(cx, entry) {
                index.define(cx.path, format!("{name_text}.{key}"), lit, exported);
            }
        }
    }
}
