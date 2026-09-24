//! Real Node/ESM bare-specifier resolution (#213, AC-0100): tsconfig
//! `paths`/`baseUrl` aliases and workspace-package names resolve to real
//! files in the ingested tree, with the config file that decided the
//! resolution cited in the edge's provenance evidence.
//!
//! Runs directory-wide *after* per-file extraction, like the
//! extensionless-import reconciliation it extends: per-file parses stay
//! blind (and therefore content-addressed-cacheable — a tsconfig edit must
//! not invalidate unchanged source parses, so resolution re-reads configs
//! on every walk), and every rewrite is proven against the set of files
//! that actually exist. Anything unproven keeps its opaque `mod:` node —
//! fail closed, never a guess. External packages (`node_modules`) are
//! explicitly out of scope and always stay `mod:` nodes.
//!
//! The tsconfig parse is [`adapters_fw::tsconfig`] — the same parser the
//! toolchain detector (#215) stores facts from, so the resolution behavior
//! and the graph's `Tool` facts can never disagree.

use crate::{Extraction, SOURCE_EXTENSIONS, SourceId};
use core_graph::placeholder::Boundary;
use core_prov::{EvidenceRef, Provenance};
use std::collections::HashSet;
use std::path::{Component, Path};

/// One tsconfig/jsconfig scope: its directory governs files beneath it.
#[derive(Debug, Clone)]
struct TsconfigScope {
    /// Repo-relative directory of the config (`""` at the root).
    dir: String,
    /// Repo-relative path of the config file (the cited evidence).
    config_path: String,
    /// `compilerOptions.baseUrl`, config-directory-relative.
    base_url: Option<String>,
    /// `paths` patterns in declaration order.
    paths: Vec<(String, Vec<String>)>,
    /// Declaring span of `"paths"` in the config text.
    paths_span: (u64, u64),
    /// Declaring span of `"compilerOptions"` (baseUrl-only resolutions).
    span: (u64, u64),
    /// Whether the config `extends` another: inherited `paths`/`baseUrl`
    /// are not loaded, so no bare specifier it governs can be proven
    /// external by the alias checks alone (#237 review, fail closed).
    extends: bool,
}

/// One workspace package: a `package.json` with a `name`, resolvable
/// through its `exports`/`module`/`main` entry candidates.
#[derive(Debug, Clone)]
struct WorkspacePackage {
    /// The published name a bare specifier matches (`@acme/shared`).
    name: String,
    /// Repo-relative package directory.
    dir: String,
    /// Entry candidates in preference order, package-relative.
    entries: Vec<String>,
    /// Repo-relative path of the `package.json` (the cited evidence).
    config_path: String,
    /// Declaring span (the `"exports"`/`"main"`/`"name"` key).
    span: (u64, u64),
}

/// Everything config-driven resolution knows about one tree.
#[derive(Debug, Clone, Default)]
pub(crate) struct ResolutionIndex {
    tsconfigs: Vec<TsconfigScope>,
    packages: Vec<WorkspacePackage>,
    /// Every `package.json` `name` in the tree, entry map or not, with the
    /// manifest path and the span of its `"name"` key.
    workspace_names: std::collections::BTreeMap<String, (String, (u64, u64))>,
    /// Every `package.json`'s declared dependencies (#237 corroboration).
    manifests: Vec<Manifest>,
}

/// One `package.json`'s declared dependency names, for citing the
/// declaration behind an external import.
#[derive(Debug, Clone)]
struct Manifest {
    /// Repo-relative directory of the manifest (`""` at the root).
    dir: String,
    /// Repo-relative path of the `package.json` (the cited evidence).
    config_path: String,
    /// Dependency name → declaring span of its key.
    dependencies: std::collections::BTreeMap<String, (u64, u64)>,
}

/// First occurrence of `needle` as a byte span; `(0, 0)` when absent.
fn span_of(text: &str, needle: &str) -> (u64, u64) {
    match text.find(needle) {
        Some(start) => (start as u64, (start + needle.len()) as u64),
        None => (0, 0),
    }
}

/// Normalize a joined repo-relative path: resolve `.`/`..`, forward
/// slashes, no leading `./`.
fn normalize(path: &str) -> String {
    let mut out = std::path::PathBuf::new();
    for comp in Path::new(path).components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out.to_string_lossy().replace('\\', "/")
}

/// Entry candidates from a parsed `package.json`, package-relative, in
/// Node's preference order: `exports` (`.` subpath: `import` > `default` >
/// `require`), then `module`, then `main`.
fn entry_candidates(json: &serde_json::Value) -> Vec<String> {
    let mut entries = Vec::new();
    let mut push = |value: &serde_json::Value| {
        if let Some(entry) = value.as_str() {
            entries.push(entry.trim_start_matches("./").to_string());
        }
    };
    let exports = &json["exports"];
    match exports {
        serde_json::Value::String(_) => push(exports),
        serde_json::Value::Object(map) => {
            let dot = map.get(".").unwrap_or(&serde_json::Value::Null);
            match dot {
                serde_json::Value::String(_) => push(dot),
                serde_json::Value::Object(conditions) => {
                    for condition in ["import", "default", "require"] {
                        if let Some(value) = conditions.get(condition) {
                            push(value);
                        }
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
    push(&json["module"]);
    push(&json["main"]);
    entries
}

impl ResolutionIndex {
    /// Walk `root` (the same `.gitignore`-aware walk as source collection)
    /// and load every tsconfig/jsconfig scope and named workspace package.
    /// Deterministic: entries sorted, config order stable.
    pub(crate) fn load(root: &Path) -> std::io::Result<Self> {
        let mut index = Self::default();
        let walked = source_walk::files(
            root,
            &crate::skipped_directory,
            source_walk::Gitignores::Honor,
        )?;
        for source_walk::WalkedFile { rel, path, name } in walked {
            let dir_rel = Path::new(&rel)
                .parent()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            let is_tsconfig = name == "tsconfig.json"
                || name == "jsconfig.json"
                || (name.starts_with("tsconfig.") && name.ends_with(".json"));
            if is_tsconfig {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let facts = adapters_fw::tsconfig::parse(&text);
                // A config with no baseUrl/paths still SHADOWS its
                // parents (#220 review): the nearest tsconfig governs
                // its files, so a root alias must not reach into a
                // nested package that didn't declare it.
                let mut paths = facts.paths;
                // TypeScript matches the pattern with the longest
                // prefix before `*`; exact patterns are most specific
                // of all (#220 review).
                paths.sort_by(|(a, _), (b, _)| {
                    let specificity = |pattern: &str| match pattern.split_once('*') {
                        None => usize::MAX,
                        Some((prefix, _)) => prefix.len(),
                    };
                    specificity(b).cmp(&specificity(a)).then(a.cmp(b))
                });
                index.tsconfigs.push(TsconfigScope {
                    dir: dir_rel,
                    config_path: rel,
                    base_url: facts.base_url,
                    paths,
                    paths_span: facts.paths_span,
                    span: facts.span,
                    extends: facts.settings.contains_key("extends"),
                });
            } else if name == "package.json" {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
                    continue;
                };
                index.manifests.push(Manifest {
                    dir: dir_rel.clone(),
                    config_path: rel.clone(),
                    dependencies: declared_dependencies(&json, &text),
                });
                let Some(pkg_name) = json["name"].as_str() else {
                    continue;
                };
                index
                    .workspace_names
                    .entry(pkg_name.to_string())
                    .or_insert_with(|| (rel.clone(), span_of(&text, "\"name\"")));
                let entries = entry_candidates(&json);
                if entries.is_empty() {
                    continue;
                }
                let span_key = if json.get("exports").is_some() {
                    "\"exports\""
                } else if json.get("module").is_some() {
                    "\"module\""
                } else {
                    "\"main\""
                };
                index.packages.push(WorkspacePackage {
                    name: pkg_name.to_string(),
                    dir: dir_rel,
                    entries,
                    config_path: rel,
                    span: span_of(&text, span_key),
                });
            }
        }
        // Longest (most specific) tsconfig scope first per importer lookup;
        // within one directory the canonical `tsconfig.json`/`jsconfig.json`
        // outranks `tsconfig.*.json` variants.
        index.tsconfigs.sort_by(|a, b| {
            let canonical = |scope: &TsconfigScope| {
                let name = scope.config_path.rsplit('/').next().unwrap_or("");
                !(name == "tsconfig.json" || name == "jsconfig.json")
            };
            b.dir
                .len()
                .cmp(&a.dir.len())
                .then(a.dir.cmp(&b.dir))
                .then(canonical(a).cmp(&canonical(b)))
                .then(a.config_path.cmp(&b.config_path))
        });
        index.packages.sort_by(|a, b| a.name.cmp(&b.name));
        // Nearest manifest first, like Node's upward `node_modules` walk.
        index.manifests.sort_by(|a, b| {
            b.dir
                .len()
                .cmp(&a.dir.len())
                .then(a.config_path.cmp(&b.config_path))
        });
        Ok(index)
    }

    fn is_empty(&self) -> bool {
        self.tsconfigs.is_empty() && self.packages.is_empty()
    }

    /// The tsconfig scope governing `importer` (nearest wins outright).
    fn nearest_scope(&self, importer: &str) -> Option<&TsconfigScope> {
        self.tsconfigs
            .iter()
            .find(|scope| scope.dir.is_empty() || importer.starts_with(&format!("{}/", scope.dir)))
    }
}

/// Dependency names declared by a parsed `package.json`, with the span of
/// each name's key inside its declaring section.
fn declared_dependencies(
    json: &serde_json::Value,
    text: &str,
) -> std::collections::BTreeMap<String, (u64, u64)> {
    let mut out = std::collections::BTreeMap::new();
    for section in [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ] {
        let Some(names) = json[section].as_object() else {
            continue;
        };
        let offset = text.find(&format!("\"{section}\"")).unwrap_or(0);
        for name in names.keys() {
            let (start, end) = span_of(&text[offset..], &format!("\"{name}\""));
            let span = if end == 0 {
                (0, 0)
            } else {
                (start + offset as u64, end + offset as u64)
            };
            out.entry(name.clone()).or_insert(span);
        }
    }
    out
}

/// Match `spec` against one `paths` pattern (`@/*` or an exact key) and
/// substitute the wildcard into `target`.
fn apply_pattern(pattern: &str, target: &str, spec: &str) -> Option<String> {
    match pattern.split_once('*') {
        None => (pattern == spec).then(|| target.to_string()),
        Some((prefix, suffix)) => {
            let matched = spec.strip_prefix(prefix)?.strip_suffix(suffix)?;
            Some(target.replacen('*', matched, 1))
        }
    }
}

/// A candidate repo-relative path, proven against the real file set: an
/// explicit source extension must match a known file; anything else tries
/// every source extension, then `index.<ext>` under the path as a
/// directory. Returns the real rel path, or `None` — never a guess.
fn prove_candidate(candidate: &str, repo: &str, known_files: &HashSet<String>) -> Option<String> {
    let file_id = |rel: &str| format!("file:{repo}@{rel}");
    if SOURCE_EXTENSIONS.iter().any(|ext| candidate.ends_with(ext)) {
        return known_files
            .contains(&file_id(candidate))
            .then(|| candidate.to_string());
    }
    for ext in SOURCE_EXTENSIONS {
        let with_ext = format!("{candidate}{ext}");
        if known_files.contains(&file_id(&with_ext)) {
            return Some(with_ext);
        }
    }
    for ext in SOURCE_EXTENSIONS {
        let index_file = format!("{candidate}/index{ext}");
        if known_files.contains(&file_id(&index_file)) {
            return Some(index_file);
        }
    }
    None
}

/// One successful config-driven resolution: the real file plus the config
/// evidence that decided it.
struct Resolved {
    rel_path: String,
    config_path: String,
    span: (u64, u64),
    via: &'static str,
}

fn resolve_spec(
    spec: &str,
    importer: &str,
    index: &ResolutionIndex,
    repo: &str,
    known_files: &HashSet<String>,
) -> Option<Resolved> {
    // The nearest governing config wins outright (#220 review): a child
    // tsconfig shadows its parents even when it declares no aliases of its
    // own, so a root alias can never reach into a nested package that
    // didn't declare it — no fall-through to less specific scopes.
    if let Some(scope) = index.nearest_scope(importer) {
        let base = match &scope.base_url {
            Some(base_url) if scope.dir.is_empty() => normalize(base_url),
            Some(base_url) => normalize(&format!("{}/{}", scope.dir, base_url)),
            None => scope.dir.clone(),
        };
        let join = |target: &str| {
            if base.is_empty() {
                normalize(target)
            } else {
                normalize(&format!("{base}/{target}"))
            }
        };
        for (pattern, targets) in &scope.paths {
            for target in targets {
                let Some(mapped) = apply_pattern(pattern, target, spec) else {
                    continue;
                };
                if let Some(real) = prove_candidate(&join(&mapped), repo, known_files) {
                    return Some(Resolved {
                        rel_path: real,
                        config_path: scope.config_path.clone(),
                        span: scope.paths_span,
                        via: "tsconfig-paths",
                    });
                }
            }
        }
        // Plain-baseUrl resolution: only when baseUrl is explicit.
        if scope.base_url.is_some()
            && let Some(real) = prove_candidate(&join(spec), repo, known_files)
        {
            return Some(Resolved {
                rel_path: real,
                config_path: scope.config_path.clone(),
                span: scope.span,
                via: "tsconfig-baseurl",
            });
        }
    }
    // Workspace packages: exact name match resolves through the entry map.
    for package in &index.packages {
        if package.name != spec {
            continue;
        }
        for entry in &package.entries {
            let candidate = if package.dir.is_empty() {
                normalize(entry)
            } else {
                normalize(&format!("{}/{}", package.dir, entry))
            };
            if let Some(real) = prove_candidate(&candidate, repo, known_files) {
                return Some(Resolved {
                    rel_path: real,
                    config_path: package.config_path.clone(),
                    span: package.span,
                    via: "workspace-package",
                });
            }
        }
    }
    None
}

/// Rewrite every `IMPORTS` edge whose target stayed an opaque `mod:` node
/// when the specifier resolves — through tsconfig `paths`/`baseUrl` or a
/// workspace package's entry map — to a file that really exists. The
/// deciding config file joins the edge's provenance evidence; unmatched
/// bare specifiers keep their `mod:` node exactly as before.
pub(crate) fn resolve_bare_imports(
    extraction: &mut Extraction,
    index: &ResolutionIndex,
    id: &SourceId,
    known_files: &HashSet<String>,
) {
    if index.is_empty() {
        return;
    }
    let file_prefix = format!("file:{}@", id.repo);
    for edge in &mut extraction.edges {
        if edge.label != "IMPORTS" || !edge.dst.starts_with("mod:") {
            continue;
        }
        let spec = edge.dst["mod:".len()..].to_string();
        let Some(importer) = edge.src.strip_prefix(&file_prefix) else {
            continue;
        };
        let Some(resolved) = resolve_spec(&spec, importer, index, id.repo, known_files) else {
            continue;
        };
        edge.dst = format!("{file_prefix}{}", resolved.rel_path);
        edge.props["resolved_via"] = serde_json::Value::String(resolved.via.into());
        // The config that decided the resolution becomes citable evidence
        // beside the import statement's own span.
        if let Ok(mut provenance) = serde_json::from_value::<Provenance>(edge.props["prov"].clone())
        {
            provenance.evidence.push(EvidenceRef {
                repo: id.repo.to_string(),
                path: resolved.config_path,
                byte_start: resolved.span.0,
                byte_end: resolved.span.1,
                commit_sha: id.commit.to_string(),
            });
            edge.props["prov"] = serde_json::to_value(provenance).expect("provenance serializes");
        }
    }
}

/// Node.js core modules (`node:`-less spellings); a subpath such as
/// `fs/promises` is matched by its first segment.
const NODE_BUILTINS: &[&str] = &[
    "assert",
    "async_hooks",
    "buffer",
    "child_process",
    "cluster",
    "console",
    "constants",
    "crypto",
    "dgram",
    "diagnostics_channel",
    "dns",
    "domain",
    "events",
    "fs",
    "http",
    "http2",
    "https",
    "inspector",
    "module",
    "net",
    "os",
    "path",
    "perf_hooks",
    "process",
    "punycode",
    "querystring",
    "readline",
    "repl",
    "stream",
    "string_decoder",
    "sys",
    "timers",
    "tls",
    "trace_events",
    "tty",
    "url",
    "util",
    "v8",
    "vm",
    "wasi",
    "worker_threads",
    "zlib",
];

/// The npm package name a bare specifier addresses (`@scope/name` or the
/// first segment), or `None` when the specifier cannot name a package at
/// all — `@/x`, `~/x`, and other bundler-alias spellings.
fn package_name(spec: &str) -> Option<&str> {
    let valid = |part: &str| {
        !part.is_empty()
            && !part.starts_with(['.', '_'])
            && part
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.' | '_'))
    };
    if let Some(scoped) = spec.strip_prefix('@') {
        let mut parts = scoped.splitn(3, '/');
        let (scope, name) = (parts.next()?, parts.next()?);
        return (valid(scope) && valid(name)).then(|| &spec[..scope.len() + name.len() + 2]);
    }
    let name = spec.split('/').next()?;
    valid(name).then_some(name)
}

/// Classify a bare specifier config-driven resolution left as a `mod:`
/// node (#237, ADR-0031). Anything the repository could still answer —
/// a `#` subpath import, a tsconfig alias or `baseUrl` path, a workspace
/// package, a repository source directory, or a spelling that cannot name
/// an npm package — stays an explicit Gap. What remains resolves only from
/// `node_modules` (or Node core) under Node's rules: an external boundary,
/// citing the nearest `package.json` that declares it when one does.
pub(crate) fn classify_bare_import(
    spec: &str,
    importer: &str,
    index: &ResolutionIndex,
    id: &SourceId,
    known_files: &HashSet<String>,
) -> Boundary {
    let unresolved = |reason: &str| Boundary::Unresolved {
        reason: reason.into(),
    };
    if spec.starts_with('.') || spec.starts_with('/') {
        return unresolved("unresolved import target");
    }
    if spec.starts_with('#') {
        return unresolved("package subpath import with no proven file");
    }
    let scope = index.nearest_scope(importer);
    if scope.is_some_and(|scope| {
        scope
            .paths
            .iter()
            .any(|(pattern, _)| apply_pattern(pattern, "", spec).is_some())
    }) {
        return unresolved("tsconfig paths alias with no proven file");
    }
    if let Some(builtin) = spec.strip_prefix("node:") {
        return Boundary::External {
            reason: format!("Node.js built-in module {builtin}"),
            evidence: vec![],
        };
    }
    let Some(name) = package_name(spec) else {
        return unresolved("bare specifier that names no package (unconfigured alias?)");
    };
    if let Some((manifest, span)) = index.workspace_names.get(name) {
        // The package itself is proven the repository's own by its manifest;
        // a module path inside it that no file answers is an explicit Gap.
        if spec != name {
            return unresolved("workspace package subpath with no proven file");
        }
        return Boundary::Internal {
            reason: format!("workspace package {name} declared in {manifest}"),
            evidence: vec![EvidenceRef {
                repo: id.repo.to_string(),
                path: manifest.clone(),
                byte_start: span.0,
                byte_end: span.1,
                commit_sha: id.commit.to_string(),
            }],
        };
    }
    let file_prefix = format!("file:{}@", id.repo);
    let first = spec.split('/').next().unwrap_or(spec);
    let under = |dir: &str| {
        let prefix = if dir.is_empty() {
            format!("{file_prefix}{first}")
        } else {
            format!("{file_prefix}{dir}/{first}")
        };
        known_files.iter().any(|file| {
            file.strip_prefix(&prefix)
                .is_some_and(|rest| rest.starts_with(['/', '.']))
        })
    };
    let base_dir = scope.and_then(|scope| {
        let base_url = scope.base_url.as_ref()?;
        Some(if scope.dir.is_empty() {
            normalize(base_url)
        } else {
            normalize(&format!("{}/{}", scope.dir, base_url))
        })
    });
    if base_dir.as_deref().is_some_and(under) {
        return unresolved("tsconfig baseUrl path with no proven file");
    }
    if ["", "src"].into_iter().any(under) {
        return unresolved("bare specifier matching a repository source directory");
    }
    // An inherited alias could name this specifier, and inherited configs
    // are not loaded: only a manifest-declared dependency stays provable.
    let inherits_aliases = scope.is_some_and(|scope| scope.extends);
    if NODE_BUILTINS.contains(&first) && !inherits_aliases {
        return Boundary::External {
            reason: format!("Node.js built-in module {first}"),
            evidence: vec![],
        };
    }
    let declared = index.manifests.iter().find(|manifest| {
        (manifest.dir.is_empty() || importer.starts_with(&format!("{}/", manifest.dir)))
            && manifest.dependencies.contains_key(name)
    });
    match declared {
        Some(manifest) => {
            let span = manifest.dependencies[name];
            Boundary::External {
                reason: format!("package {name} declared in {}", manifest.config_path),
                evidence: vec![EvidenceRef {
                    repo: id.repo.to_string(),
                    path: manifest.config_path.clone(),
                    byte_start: span.0,
                    byte_end: span.1,
                    commit_sha: id.commit.to_string(),
                }],
            }
        }
        None if inherits_aliases => unresolved(
            "bare specifier under a tsconfig that extends another (inherited aliases not loaded)",
        ),
        None => Boundary::External {
            reason: format!("package {name} not provided by this repository"),
            evidence: vec![],
        },
    }
}
