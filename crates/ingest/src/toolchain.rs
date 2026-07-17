//! Toolchain detection — config files as first-class evidence (#215,
//! AC-0096/AC-0097).
//!
//! One registry turns config files (`package.json`, `tsconfig.json`,
//! lockfiles, Dockerfiles, CI workflows, language manifests) into cited
//! graph facts: `Tool` nodes (`tool:{repo}@{name}`) with resolved settings
//! as props, `DEFINED_IN` edges to the config `File` node(s) that prove
//! them, and provenance citing the exact declaring span. [`detect_in_file`]
//! is the single detection function behind BOTH the graph extraction
//! ([`extract_dir`]) and Preflight's framework chips — the two surfaces can
//! never disagree because they quote the same detection.
//!
//! Redaction is fail-closed (AC-0097, same standard as the T1 state-JSON
//! redaction in AC-0009): `.env*` files contribute **key names only**;
//! for every other config only allowlisted setting keys are stored, and any
//! stored string that looks secret-shaped is dropped. JS-authored configs
//! (`vite.config.ts`, `webpack.config.js`, …) are detected by presence
//! only — settings that live behind code surface as an explicit
//! `config-behind-code` Unsupported finding, never as evaluated facts.

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::BTreeMap;
use std::path::Path;

/// Extractor id recorded in provenance for every toolchain fact.
pub const TOOLCHAIN_EXTRACTOR_ID: &str = "t0.toolchain";

/// Source identity for one extraction run (same convention as the
/// language adapters' `SourceId`).
#[derive(Debug, Clone, Copy)]
pub struct SourceId<'a> {
    /// Repository identity (`owner/name` or `local/<name>`).
    pub repo: &'a str,
    /// Commit the tree was read at.
    pub commit: &'a str,
}

/// One tool proven by a config file, before graph-fact conversion.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectedTool {
    /// Stable tool name — the `{name}` in `tool:{repo}@{name}`. Package
    /// names for dependency-proven tools (`react`, `vite`), the
    /// repo-relative config path for per-file configs (`tsconfig.json`,
    /// `.github/workflows/ci.yml`).
    pub name: String,
    /// Human display name (`React`, `TypeScript config`).
    pub display: String,
    /// Category chip: `framework`, `bundler`, `test-runner`,
    /// `package-manager`, `ci`, `container`, `linter`, `formatter`,
    /// `compiler-config`, `build-manifest`, `package-manifest`,
    /// `environment`, `language`.
    pub category: &'static str,
    /// Repo-relative path of the config file proving this tool.
    pub path: String,
    /// Byte span of the declaring evidence (the dependency line, the
    /// section header — not just "the file").
    pub byte_start: u64,
    /// Exclusive end of the declaring span.
    pub byte_end: u64,
    /// Resolved settings, already redacted (allowlisted keys only).
    pub settings: BTreeMap<String, serde_json::Value>,
    /// True when the config is authored in code and the settings cannot be
    /// read without evaluating it — presence-only detection.
    pub settings_behind_code: bool,
}

/// Everything one file contributes: tools for the graph, framework labels
/// for Preflight's chips, both from the same parse.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Detection {
    /// Tools proven by this file.
    pub tools: Vec<DetectedTool>,
    /// Preflight framework chip labels (exact display strings).
    pub framework_labels: Vec<String>,
}

/// Toolchain graph facts for one tree.
#[derive(Debug, Clone, Default)]
pub struct ToolExtraction {
    /// `Tool` nodes plus the config `File` nodes that prove them.
    pub nodes: Vec<Node>,
    /// `DEFINED_IN` edges (tool → config file).
    pub edges: Vec<Edge>,
    /// Config files that contributed at least one detection.
    pub files: u64,
}

/// Dependency names (from `package.json`) that identify a known tool.
/// Order is irrelevant — lookups only. Extend freely; unknown deps are
/// simply not tools.
const KNOWN_JS_TOOLS: &[(&str, &str, &str)] = &[
    ("react", "React", "framework"),
    ("next", "Next.js", "framework"),
    ("express", "Express", "framework"),
    ("fastify", "Fastify", "framework"),
    ("vue", "Vue", "framework"),
    ("svelte", "Svelte", "framework"),
    ("@angular/core", "Angular", "framework"),
    ("vite", "Vite", "bundler"),
    ("webpack", "webpack", "bundler"),
    ("rollup", "Rollup", "bundler"),
    ("esbuild", "esbuild", "bundler"),
    ("jest", "Jest", "test-runner"),
    ("vitest", "Vitest", "test-runner"),
    ("mocha", "Mocha", "test-runner"),
    ("cypress", "Cypress", "test-runner"),
    ("@playwright/test", "Playwright", "test-runner"),
    ("eslint", "ESLint", "linter"),
    ("@biomejs/biome", "Biome", "linter"),
    ("prettier", "Prettier", "formatter"),
    ("typescript", "TypeScript", "language"),
];

/// Dependencies whose presence is also a Preflight framework chip. The
/// labels are the exact strings the chips have always shown.
const FRAMEWORK_LABELS: &[(&str, &str)] = &[
    ("react", "React"),
    ("express", "Express"),
    ("next", "Next.js"),
];

/// JS/TS-authored config file stems, with the tool they configure.
/// Matched against the file name minus its final extension, so
/// `vite.config.ts`, `vite.config.mjs`, … all hit `vite.config`.
const CODE_CONFIG_STEMS: &[(&str, &str, &str)] = &[
    ("vite.config", "Vite config", "bundler"),
    ("webpack.config", "webpack config", "bundler"),
    ("rollup.config", "Rollup config", "bundler"),
    ("next.config", "Next.js config", "framework"),
    ("babel.config", "Babel config", "language"),
    ("postcss.config", "PostCSS config", "language"),
    ("tailwind.config", "Tailwind config", "framework"),
    ("eslint.config", "ESLint config", "linter"),
    ("prettier.config", "Prettier config", "formatter"),
    ("jest.config", "Jest config", "test-runner"),
    ("vitest.config", "Vitest config", "test-runner"),
    ("svelte.config", "Svelte config", "framework"),
    ("astro.config", "Astro config", "framework"),
    ("nuxt.config", "Nuxt config", "framework"),
];

/// Lockfile names and the package manager each proves.
const LOCKFILES: &[(&str, &str, &str)] = &[
    ("package-lock.json", "npm", "npm"),
    ("yarn.lock", "yarn", "Yarn"),
    ("pnpm-lock.yaml", "pnpm", "pnpm"),
    ("bun.lock", "bun", "Bun"),
    ("bun.lockb", "bun", "Bun"),
    ("Cargo.lock", "cargo", "Cargo"),
    ("poetry.lock", "poetry", "Poetry"),
    ("uv.lock", "uv", "uv"),
];

/// A stored string is dropped when it looks secret-shaped: one long
/// unbroken token-like run. Fail closed — a dropped setting is recoverable
/// by reading the cited file; a leaked secret is not.
fn secret_shaped(value: &str) -> bool {
    value.len() >= 40
        && !value.contains(char::is_whitespace)
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '_' | '-' | '=' | '.'))
}

/// Redact one settings value in place of storing it verbatim: strings are
/// dropped when secret-shaped; arrays/objects are filtered recursively.
fn redacted(value: &serde_json::Value) -> Option<serde_json::Value> {
    match value {
        serde_json::Value::String(s) => {
            if secret_shaped(s) {
                None
            } else {
                Some(value.clone())
            }
        }
        serde_json::Value::Array(items) => Some(serde_json::Value::Array(
            items.iter().filter_map(redacted).collect(),
        )),
        serde_json::Value::Object(map) => Some(serde_json::Value::Object(
            map.iter()
                .filter_map(|(k, v)| redacted(v).map(|v| (k.clone(), v)))
                .collect(),
        )),
        _ => Some(value.clone()),
    }
}

/// Insert an allowlisted setting, applying the secret-shape guard.
fn setting(
    settings: &mut BTreeMap<String, serde_json::Value>,
    key: &str,
    value: &serde_json::Value,
) {
    if value.is_null() {
        return;
    }
    if let Some(safe) = redacted(value) {
        settings.insert(key.to_string(), safe);
    }
}

/// First occurrence of `needle` in `text` as a byte span; whole-file span
/// when absent (the file as a whole is still the proof).
fn span_of(text: &str, needle: &str) -> (u64, u64) {
    match text.find(needle) {
        Some(start) => (start as u64, (start + needle.len()) as u64),
        None => (0, text.len() as u64),
    }
}

fn tool(
    name: impl Into<String>,
    display: impl Into<String>,
    category: &'static str,
    rel: &str,
    span: (u64, u64),
    settings: BTreeMap<String, serde_json::Value>,
) -> DetectedTool {
    DetectedTool {
        name: name.into(),
        display: display.into(),
        category,
        path: rel.to_string(),
        byte_start: span.0,
        byte_end: span.1,
        settings,
        settings_behind_code: false,
    }
}

/// Detect every tool and framework label one file proves. `name` is the
/// bare file name, `rel` the repo-relative path. Reads the file only when
/// the name matches a detector — cheap on non-config files. This is the
/// single source of truth for Preflight AND the graph (#215).
pub fn detect_in_file(name: &str, rel: &str, path: &Path) -> Detection {
    let mut detection = Detection::default();
    match name {
        "package.json" => detect_package_json(rel, path, &mut detection),
        "go.mod" => {
            detection.framework_labels.push("Go module".into());
            detect_go_mod(rel, path, &mut detection);
        }
        "pyproject.toml" => {
            detection.framework_labels.push("Python project".into());
            detect_pyproject(rel, path, &mut detection);
        }
        "requirements.txt" => {
            detection.framework_labels.push("Python project".into());
            detection.tools.push(tool(
                rel,
                "pip requirements",
                "package-manifest",
                rel,
                (0, 0),
                BTreeMap::new(),
            ));
        }
        "pom.xml" => {
            detection.framework_labels.push("Java project".into());
            detection.tools.push(tool(
                "maven",
                "Maven",
                "build-manifest",
                rel,
                (0, 0),
                BTreeMap::new(),
            ));
        }
        "build.gradle" | "build.gradle.kts" => {
            detection.framework_labels.push("Java project".into());
            detection.tools.push(tool(
                "gradle",
                "Gradle",
                "build-manifest",
                rel,
                (0, 0),
                BTreeMap::new(),
            ));
        }
        "Cargo.toml" => detect_cargo_toml(rel, path, &mut detection),
        "manifest.json"
            if std::fs::read_to_string(path)
                .is_ok_and(|text| text.contains("\"manifest_version\"")) =>
        {
            detection
                .framework_labels
                .push("WebExtension (Manifest V2/V3)".into());
        }
        _ => {}
    }
    if name == "tsconfig.json"
        || name == "jsconfig.json"
        || (name.starts_with("tsconfig.") && name.ends_with(".json"))
    {
        detect_tsconfig(rel, path, &mut detection);
    }
    if let Some((_, tool_name, display)) = LOCKFILES.iter().find(|(file, _, _)| file == &name) {
        let mut settings = BTreeMap::new();
        settings.insert("lockfile".into(), serde_json::Value::String(rel.into()));
        detection.tools.push(tool(
            *tool_name,
            *display,
            "package-manager",
            rel,
            (0, 0),
            settings,
        ));
    }
    if name == "Dockerfile" || name.starts_with("Dockerfile.") {
        detect_dockerfile(rel, path, &mut detection);
    }
    if matches!(
        name,
        "docker-compose.yml" | "docker-compose.yaml" | "compose.yml" | "compose.yaml"
    ) {
        detect_compose(rel, path, &mut detection);
    }
    if (rel.starts_with(".github/workflows/") || rel.starts_with(".github\\workflows\\"))
        && (name.ends_with(".yml") || name.ends_with(".yaml"))
    {
        detect_workflow(rel, path, &mut detection);
    }
    if name.ends_with(".tf") {
        detection.framework_labels.push("Terraform".into());
    }
    if name == ".env" || name.starts_with(".env.") {
        detect_env(rel, path, &mut detection);
    }
    detect_code_config(name, rel, &mut detection);
    detection
}

/// `package.json`: the manifest itself (scripts/engines/type as settings)
/// plus one tool per known dependency, each cited at its declaring line.
fn detect_package_json(rel: &str, path: &Path, detection: &mut Detection) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return; // unparseable manifest: fail closed, no facts
    };
    let mut settings = BTreeMap::new();
    setting(&mut settings, "name", &json["name"]);
    setting(&mut settings, "type", &json["type"]);
    setting(&mut settings, "engines", &json["engines"]);
    setting(&mut settings, "workspaces", &json["workspaces"]);
    setting(&mut settings, "packageManager", &json["packageManager"]);
    // Script *names* only — script bodies are shell commands and can embed
    // anything (fail-closed redaction, AC-0097).
    if let Some(scripts) = json["scripts"].as_object() {
        settings.insert(
            "scripts".into(),
            serde_json::Value::Array(
                scripts
                    .keys()
                    .map(|k| serde_json::Value::String(k.clone()))
                    .collect(),
            ),
        );
    }
    detection.tools.push(tool(
        rel,
        "npm package manifest",
        "package-manifest",
        rel,
        span_of(&text, "\"name\""),
        settings,
    ));
    // `packageManager: "pnpm@9.1.0"` proves the package manager directly.
    if let Some(pm) = json["packageManager"].as_str()
        && let Some((pm_name, version)) = pm.split_once('@')
    {
        let mut settings = BTreeMap::new();
        setting(
            &mut settings,
            "version",
            &serde_json::Value::String(version.into()),
        );
        detection.tools.push(tool(
            pm_name,
            pm_name,
            "package-manager",
            rel,
            span_of(&text, "\"packageManager\""),
            settings,
        ));
    }
    for deps_key in ["dependencies", "devDependencies"] {
        let Some(deps) = json[deps_key].as_object() else {
            continue;
        };
        for (dep, requirement) in deps {
            let Some((_, display, category)) =
                KNOWN_JS_TOOLS.iter().find(|(known, _, _)| known == dep)
            else {
                continue;
            };
            let mut settings = BTreeMap::new();
            setting(&mut settings, "requirement", requirement);
            detection.tools.push(tool(
                dep,
                *display,
                category,
                rel,
                span_of(&text, &format!("\"{dep}\"")),
                settings,
            ));
            if let Some((_, label)) = FRAMEWORK_LABELS.iter().find(|(known, _)| known == dep) {
                detection.framework_labels.push((*label).into());
            }
        }
    }
}

/// `tsconfig.json`/`jsconfig.json`: the compiler options #213's module
/// resolution consumes — parsed once here, cited from this fact.
fn detect_tsconfig(rel: &str, path: &Path, detection: &mut Detection) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let mut settings = BTreeMap::new();
    // tsconfig allows comments; serde_json does not. An unparseable config
    // degrades to presence-only — never guessed settings.
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
        for key in [
            "target",
            "module",
            "moduleResolution",
            "strict",
            "jsx",
            "baseUrl",
            "paths",
        ] {
            setting(&mut settings, key, &json["compilerOptions"][key]);
        }
        setting(&mut settings, "extends", &json["extends"]);
    }
    detection.tools.push(tool(
        rel,
        "TypeScript config",
        "compiler-config",
        rel,
        span_of(&text, "\"compilerOptions\""),
        settings,
    ));
}

/// `Cargo.toml`: package name/edition (or workspace members).
fn detect_cargo_toml(rel: &str, path: &Path, detection: &mut Detection) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(value) = text.parse::<toml::Value>() else {
        return;
    };
    let mut settings = BTreeMap::new();
    if let Some(package) = value.get("package") {
        if let Some(name) = package.get("name").and_then(|v| v.as_str()) {
            settings.insert("package".into(), serde_json::Value::String(name.into()));
        }
        if let Some(edition) = package.get("edition").and_then(|v| v.as_str()) {
            settings.insert("edition".into(), serde_json::Value::String(edition.into()));
        }
    }
    if let Some(members) = value
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
    {
        settings.insert(
            "workspace_members".into(),
            serde_json::Value::Array(
                members
                    .iter()
                    .filter_map(|m| m.as_str())
                    .map(|m| serde_json::Value::String(m.into()))
                    .collect(),
            ),
        );
    }
    detection.tools.push(tool(
        "cargo",
        "Cargo",
        "build-manifest",
        rel,
        span_of(&text, "[package]"),
        settings,
    ));
}

/// `pyproject.toml`: project name, Python requirement, build backend.
fn detect_pyproject(rel: &str, path: &Path, detection: &mut Detection) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let mut settings = BTreeMap::new();
    if let Ok(value) = text.parse::<toml::Value>() {
        if let Some(project) = value.get("project") {
            if let Some(name) = project.get("name").and_then(|v| v.as_str()) {
                settings.insert("project".into(), serde_json::Value::String(name.into()));
            }
            if let Some(requires) = project.get("requires-python").and_then(|v| v.as_str()) {
                settings.insert(
                    "requires-python".into(),
                    serde_json::Value::String(requires.into()),
                );
            }
        }
        if let Some(backend) = value
            .get("build-system")
            .and_then(|b| b.get("build-backend"))
            .and_then(|v| v.as_str())
        {
            settings.insert(
                "build-backend".into(),
                serde_json::Value::String(backend.into()),
            );
        }
    }
    detection.tools.push(tool(
        rel,
        "Python project manifest",
        "build-manifest",
        rel,
        span_of(&text, "[project]"),
        settings,
    ));
}

/// `go.mod`: module path and Go version from the two directive lines.
fn detect_go_mod(rel: &str, path: &Path, detection: &mut Detection) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let mut settings = BTreeMap::new();
    for line in text.lines() {
        if let Some(module) = line.trim().strip_prefix("module ") {
            settings.insert(
                "module".into(),
                serde_json::Value::String(module.trim().into()),
            );
        } else if let Some(version) = line.trim().strip_prefix("go ") {
            settings.insert(
                "go".into(),
                serde_json::Value::String(version.trim().into()),
            );
        }
    }
    detection.tools.push(tool(
        "go",
        "Go module",
        "build-manifest",
        rel,
        span_of(&text, "module "),
        settings,
    ));
}

/// `Dockerfile`: base images (`FROM`) and exposed ports (`EXPOSE`) by
/// deterministic line scan.
fn detect_dockerfile(rel: &str, path: &Path, detection: &mut Detection) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let mut base_images = Vec::new();
    let mut ports = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(from) = trimmed
            .strip_prefix("FROM ")
            .or_else(|| trimmed.strip_prefix("from "))
        {
            // `FROM node:22 AS build` — the image is the first word.
            if let Some(image) = from.split_whitespace().next() {
                base_images.push(serde_json::Value::String(image.into()));
            }
        }
        if let Some(exposed) = trimmed.strip_prefix("EXPOSE ") {
            for port in exposed.split_whitespace() {
                ports.push(serde_json::Value::String(port.into()));
            }
        }
    }
    let mut settings = BTreeMap::new();
    if !base_images.is_empty() {
        settings.insert("base_images".into(), serde_json::Value::Array(base_images));
    }
    if !ports.is_empty() {
        settings.insert("exposed_ports".into(), serde_json::Value::Array(ports));
    }
    detection.tools.push(tool(
        rel,
        "Docker",
        "container",
        rel,
        span_of(&text, "FROM "),
        settings,
    ));
}

/// `docker-compose.yml`: service names and images by indentation scan —
/// enough for evidence without a YAML dependency.
fn detect_compose(rel: &str, path: &Path, detection: &mut Detection) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let mut services = Vec::new();
    let mut images = Vec::new();
    let mut in_services = false;
    for line in text.lines() {
        if line.starts_with("services:") {
            in_services = true;
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if in_services && !line.trim().is_empty() && indent == 0 {
            in_services = false; // next top-level section
        }
        if in_services
            && indent == 2
            && let Some(service) = line.trim().strip_suffix(':')
            && !service.is_empty()
        {
            services.push(serde_json::Value::String(service.into()));
        }
        if let Some(image) = line.trim().strip_prefix("image:") {
            images.push(serde_json::Value::String(image.trim().into()));
        }
    }
    let mut settings = BTreeMap::new();
    if !services.is_empty() {
        settings.insert("services".into(), serde_json::Value::Array(services));
    }
    if !images.is_empty() {
        settings.insert("images".into(), serde_json::Value::Array(images));
    }
    detection.tools.push(tool(
        rel,
        "Docker Compose",
        "container",
        rel,
        span_of(&text, "services:"),
        settings,
    ));
}

/// `.github/workflows/*.yml`: workflow name, triggers, and job names by
/// indentation scan.
fn detect_workflow(rel: &str, path: &Path, detection: &mut Detection) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let mut settings = BTreeMap::new();
    let mut triggers = Vec::new();
    let mut jobs = Vec::new();
    let mut section = "";
    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim();
        if indent == 0 {
            if let Some(workflow_name) = trimmed.strip_prefix("name:") {
                settings.insert(
                    "workflow".into(),
                    serde_json::Value::String(workflow_name.trim().trim_matches('"').into()),
                );
            }
            if let Some(inline) = trimmed.strip_prefix("on:") {
                // `on: push` / `on: [push, pull_request]` inline forms.
                for trigger in inline.trim_matches(['[', ']', ' ']).split(',') {
                    if !trigger.trim().is_empty() {
                        triggers.push(serde_json::Value::String(trigger.trim().into()));
                    }
                }
            }
            section = if trimmed.starts_with("on:") {
                "on"
            } else if trimmed.starts_with("jobs:") {
                "jobs"
            } else {
                ""
            };
            continue;
        }
        if indent == 2 && !trimmed.is_empty() {
            match section {
                "on" => {
                    let key = trimmed.trim_start_matches('-').trim();
                    let key = key.split(':').next().unwrap_or(key).trim();
                    if !key.is_empty() {
                        triggers.push(serde_json::Value::String(key.into()));
                    }
                }
                "jobs" => {
                    if let Some(job) = trimmed.strip_suffix(':') {
                        jobs.push(serde_json::Value::String(job.into()));
                    }
                }
                _ => {}
            }
        }
    }
    if !triggers.is_empty() {
        settings.insert("triggers".into(), serde_json::Value::Array(triggers));
    }
    if !jobs.is_empty() {
        settings.insert("jobs".into(), serde_json::Value::Array(jobs));
    }
    detection.tools.push(tool(
        rel,
        "GitHub Actions workflow",
        "ci",
        rel,
        span_of(&text, "on:"),
        settings,
    ));
}

/// `.env*`: key names ONLY — values are never read into settings, never
/// stored, never hashed (AC-0097, fail closed).
fn detect_env(rel: &str, path: &Path, detection: &mut Detection) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let keys: Vec<serde_json::Value> = text
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let key = line.strip_prefix("export ").unwrap_or(line);
            let key = key.split('=').next()?.trim();
            if key.is_empty() {
                None
            } else {
                Some(serde_json::Value::String(key.into()))
            }
        })
        .collect();
    let mut settings = BTreeMap::new();
    settings.insert("keys".into(), serde_json::Value::Array(keys));
    detection.tools.push(tool(
        rel,
        "Environment file",
        "environment",
        rel,
        (0, 0),
        settings,
    ));
}

/// JS/TS-authored configs: presence only — settings live behind code and
/// are never evaluated (#215 non-goal). Preflight surfaces the limitation
/// as a `config-behind-code` finding from this same detection.
fn detect_code_config(name: &str, rel: &str, detection: &mut Detection) {
    let stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name);
    let Some((_, display, category)) = CODE_CONFIG_STEMS
        .iter()
        .find(|(config_stem, _, _)| config_stem == &stem)
    else {
        return;
    };
    let mut detected = tool(rel, *display, category, rel, (0, 0), BTreeMap::new());
    detected.settings_behind_code = true;
    detection.tools.push(detected);
}

/// Walk `root` (sorted, same skip set as Preflight) and convert every
/// detection into graph facts: `Tool` nodes, config `File` nodes, and
/// `DEFINED_IN` edges, all with T0/Confirmed provenance citing declaring
/// spans (AC-0096). Deterministic for a given tree.
pub fn extract_dir(
    root: &Path,
    id: &SourceId,
    on_file: &mut dyn FnMut(&str),
) -> std::io::Result<ToolExtraction> {
    // One declaring proof: (config path, span start, span end).
    type Proof = (String, u64, u64);
    // name → merged tool (a package manager proven by both `packageManager`
    // and a lockfile is one Tool with two proofs).
    let mut tools: BTreeMap<String, (DetectedTool, Vec<Proof>)> = BTreeMap::new();
    let mut config_files: BTreeMap<String, u64> = BTreeMap::new();

    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&dir)?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        for path in entries {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if path.is_dir() {
                if !crate::preflight::skip_dir(&name) {
                    stack.push(path);
                }
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let detection = detect_in_file(&name, &rel, &path);
            if detection.tools.is_empty() {
                continue;
            }
            on_file(&rel);
            let file_len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            config_files.entry(rel.clone()).or_insert(file_len);
            for detected in detection.tools {
                let proof = (
                    detected.path.clone(),
                    detected.byte_start,
                    detected.byte_end,
                );
                match tools.entry(detected.name.clone()) {
                    std::collections::btree_map::Entry::Vacant(slot) => {
                        slot.insert((detected, vec![proof]));
                    }
                    std::collections::btree_map::Entry::Occupied(mut slot) => {
                        let (merged, proofs) = slot.get_mut();
                        for (key, value) in detected.settings {
                            merged.settings.entry(key).or_insert(value);
                        }
                        merged.settings_behind_code =
                            merged.settings_behind_code || detected.settings_behind_code;
                        proofs.push(proof);
                    }
                }
            }
        }
    }

    let mut extraction = ToolExtraction {
        files: config_files.len() as u64,
        ..Default::default()
    };
    let evidence = |path: &str, start: u64, end: u64| EvidenceRef {
        repo: id.repo.to_string(),
        path: path.to_string(),
        byte_start: start,
        byte_end: end,
        commit_sha: id.commit.to_string(),
    };
    for (rel, len) in &config_files {
        let prov = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![evidence(rel, 0, *len)],
            TOOLCHAIN_EXTRACTOR_ID,
            format!("file:{}@{rel}", id.repo).as_bytes(),
        )
        .expect("within ceiling");
        extraction.nodes.push(Node {
            id: format!("file:{}@{rel}", id.repo),
            label: "File".into(),
            props: serde_json::json!({
                "path": rel,
                "config": true,
                "prov": serde_json::to_value(prov).expect("serializes"),
            }),
        });
    }
    for (name, (detected, proofs)) in &tools {
        let tool_id = format!("tool:{}@{name}", id.repo);
        let settings = serde_json::Value::Object(
            detected
                .settings
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        );
        let spans: Vec<EvidenceRef> = proofs
            .iter()
            .map(|(path, start, end)| evidence(path, *start, *end))
            .collect();
        let prov = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            spans.clone(),
            TOOLCHAIN_EXTRACTOR_ID,
            format!("{tool_id}:{settings}").as_bytes(),
        )
        .expect("within ceiling");
        let mut props = serde_json::json!({
            "name": name,
            "display": detected.display,
            "category": detected.category,
            "settings": settings,
            "prov": serde_json::to_value(&prov).expect("serializes"),
        });
        if detected.settings_behind_code {
            props["settings_behind_code"] = serde_json::Value::Bool(true);
        }
        extraction.nodes.push(Node {
            id: tool_id.clone(),
            label: "Tool".into(),
            props,
        });
        let mut proven_paths: Vec<&String> = proofs.iter().map(|(path, _, _)| path).collect();
        proven_paths.sort();
        proven_paths.dedup();
        for path in proven_paths {
            let span = proofs
                .iter()
                .find(|(proof_path, _, _)| proof_path == path)
                .map(|(_, start, end)| (*start, *end))
                .unwrap_or((0, 0));
            let edge_prov = Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
                vec![evidence(path, span.0, span.1)],
                TOOLCHAIN_EXTRACTOR_ID,
                format!("{tool_id} DEFINED_IN {path}").as_bytes(),
            )
            .expect("within ceiling");
            extraction.edges.push(Edge {
                src: tool_id.clone(),
                dst: format!("file:{}@{path}", id.repo),
                label: "DEFINED_IN".into(),
                props: serde_json::json!({
                    "prov": serde_json::to_value(edge_prov).expect("serializes"),
                }),
            });
        }
    }
    Ok(extraction)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn extract(root: &Path) -> ToolExtraction {
        extract_dir(
            root,
            &SourceId {
                repo: "local/tools",
                commit: "c0ffee",
            },
            &mut |_| {},
        )
        .unwrap()
    }

    fn tool_node<'a>(extraction: &'a ToolExtraction, id: &str) -> &'a Node {
        extraction
            .nodes
            .iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("no node {id}"))
    }

    #[test]
    fn package_json_yields_cited_tool_facts_with_settings() {
        // AC-0096: dependency-proven tools carry the declaring span and the
        // manifest's resolved settings as citable props.
        let dir = tempfile::tempdir().unwrap();
        let text = r#"{
  "name": "shop",
  "type": "module",
  "packageManager": "pnpm@9.1.0",
  "scripts": { "build": "vite build", "test": "vitest run" },
  "dependencies": { "react": "^19.0.0", "left-pad": "^1.0.0" },
  "devDependencies": { "vite": "^6.0.0" }
}"#;
        write(dir.path(), "package.json", text);
        let extraction = extract(dir.path());

        let react = tool_node(&extraction, "tool:local/tools@react");
        assert_eq!(react.label, "Tool");
        assert_eq!(react.props["category"], "framework");
        assert_eq!(react.props["settings"]["requirement"], "^19.0.0");
        // Evidence cites the `"react"` declaring span, not just the file.
        let prov: Provenance = serde_json::from_value(react.props["prov"].clone()).unwrap();
        let span = &prov.evidence[0];
        assert_eq!(
            &text[span.byte_start as usize..span.byte_end as usize],
            "\"react\""
        );
        assert_eq!(prov.extractor_id, TOOLCHAIN_EXTRACTOR_ID);

        // Unknown dependencies are not tools.
        assert!(!extraction.nodes.iter().any(|n| n.id.contains("left-pad")));

        // The manifest itself carries scripts (names only), type, engines.
        let manifest = tool_node(&extraction, "tool:local/tools@package.json");
        assert_eq!(manifest.props["settings"]["type"], "module");
        assert_eq!(
            manifest.props["settings"]["scripts"],
            serde_json::json!(["build", "test"])
        );
        // Script bodies (shell commands) are never stored.
        assert!(!manifest.props.to_string().contains("vite build"));

        // packageManager proves pnpm; the lockfile would merge into it.
        let pnpm = tool_node(&extraction, "tool:local/tools@pnpm");
        assert_eq!(pnpm.props["settings"]["version"], "9.1.0");

        // Every tool is DEFINED_IN the config file that proves it.
        assert!(
            extraction
                .edges
                .iter()
                .any(|e| e.src == "tool:local/tools@react"
                    && e.dst == "file:local/tools@package.json"
                    && e.label == "DEFINED_IN")
        );
        // And the config File node exists so the edge can never dangle.
        assert!(
            extraction
                .nodes
                .iter()
                .any(|n| n.id == "file:local/tools@package.json" && n.label == "File")
        );
    }

    #[test]
    fn tsconfig_settings_are_the_parse_once_source_for_resolution() {
        // #213 seam: paths/baseUrl parsed once here, cited from this fact.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "tsconfig.json",
            r#"{
  "compilerOptions": {
    "target": "ES2022",
    "moduleResolution": "bundler",
    "strict": true,
    "baseUrl": ".",
    "paths": { "@/*": ["src/*"] }
  }
}"#,
        );
        let extraction = extract(dir.path());
        let tsconfig = tool_node(&extraction, "tool:local/tools@tsconfig.json");
        assert_eq!(tsconfig.props["category"], "compiler-config");
        assert_eq!(tsconfig.props["settings"]["moduleResolution"], "bundler");
        assert_eq!(tsconfig.props["settings"]["strict"], true);
        assert_eq!(
            tsconfig.props["settings"]["paths"]["@/*"],
            serde_json::json!(["src/*"])
        );
    }

    #[test]
    fn env_files_contribute_key_names_and_never_values() {
        // AC-0097 fail-closed redaction: the secret value must not appear
        // anywhere in the serialized extraction — props, evidence, hashes.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            ".env.production",
            "# comment\nDATABASE_URL=postgres://user:hunter2@db/prod\nexport API_TOKEN=sk-live-abcdef123456\n",
        );
        let extraction = extract(dir.path());
        let env = tool_node(&extraction, "tool:local/tools@.env.production");
        assert_eq!(env.props["category"], "environment");
        assert_eq!(
            env.props["settings"]["keys"],
            serde_json::json!(["DATABASE_URL", "API_TOKEN"])
        );
        let serialized = serde_json::to_string(&extraction.nodes).unwrap();
        assert!(!serialized.contains("hunter2"));
        assert!(!serialized.contains("sk-live"));
    }

    #[test]
    fn secret_shaped_values_are_dropped_from_any_config() {
        // A token-like string in an allowlisted setting is dropped, not
        // stored (fail closed) — the cited file remains the way to read it.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "package.json",
            r#"{ "name": "x", "engines": { "node": "AKIAIOSFODNN7EXAMPLEAKIAIOSFODNN7EXAMPLE12" } }"#,
        );
        let extraction = extract(dir.path());
        let manifest = tool_node(&extraction, "tool:local/tools@package.json");
        assert!(manifest.props["settings"]["engines"]["node"].is_null());
        assert!(
            !serde_json::to_string(&extraction.nodes)
                .unwrap()
                .contains("AKIAIOSFODNN7EXAMPLE")
        );
    }

    #[test]
    fn code_authored_configs_are_presence_only() {
        // #215 non-goal: JS-authored configs are never evaluated — the tool
        // exists, flagged as settings-behind-code, with empty settings.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "vite.config.ts",
            "export default { base: '/' }\n",
        );
        let extraction = extract(dir.path());
        let vite = tool_node(&extraction, "tool:local/tools@vite.config.ts");
        assert_eq!(vite.props["settings_behind_code"], true);
        assert_eq!(vite.props["settings"], serde_json::json!({}));
    }

    #[test]
    fn docker_ci_lockfiles_and_manifests_are_detected() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "Dockerfile",
            "FROM node:22-alpine AS build\nEXPOSE 3000 9229\n",
        );
        write(
            dir.path(),
            "docker-compose.yml",
            "services:\n  web:\n    image: shop:latest\n  db:\n    image: postgres:16\n",
        );
        write(
            dir.path(),
            ".github/workflows/ci.yml",
            "name: CI\non:\n  push:\n  pull_request:\njobs:\n  build:\n    runs-on: ubuntu-latest\n  test:\n    runs-on: ubuntu-latest\n",
        );
        write(dir.path(), "pnpm-lock.yaml", "lockfileVersion: 9\n");
        write(
            dir.path(),
            "go.mod",
            "module github.com/acme/shop\n\ngo 1.23\n",
        );
        let extraction = extract(dir.path());

        let docker = tool_node(&extraction, "tool:local/tools@Dockerfile");
        assert_eq!(
            docker.props["settings"]["base_images"],
            serde_json::json!(["node:22-alpine"])
        );
        assert_eq!(
            docker.props["settings"]["exposed_ports"],
            serde_json::json!(["3000", "9229"])
        );

        let compose = tool_node(&extraction, "tool:local/tools@docker-compose.yml");
        assert_eq!(
            compose.props["settings"]["services"],
            serde_json::json!(["web", "db"])
        );

        let workflow = tool_node(&extraction, "tool:local/tools@.github/workflows/ci.yml");
        assert_eq!(workflow.props["settings"]["workflow"], "CI");
        assert_eq!(
            workflow.props["settings"]["triggers"],
            serde_json::json!(["push", "pull_request"])
        );
        assert_eq!(
            workflow.props["settings"]["jobs"],
            serde_json::json!(["build", "test"])
        );

        let pnpm = tool_node(&extraction, "tool:local/tools@pnpm");
        assert_eq!(pnpm.props["settings"]["lockfile"], "pnpm-lock.yaml");

        let go = tool_node(&extraction, "tool:local/tools@go");
        assert_eq!(go.props["settings"]["module"], "github.com/acme/shop");
        assert_eq!(go.props["settings"]["go"], "1.23");
    }

    #[test]
    fn extraction_is_deterministic_and_skips_vendored_trees() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "package.json", r#"{ "name": "x" }"#);
        write(
            dir.path(),
            "node_modules/dep/package.json",
            r#"{ "name": "dep" }"#,
        );
        let first = extract(dir.path());
        let second = extract(dir.path());
        assert_eq!(first.nodes, second.nodes);
        assert_eq!(first.edges, second.edges);
        assert!(!first.nodes.iter().any(|n| n.id.contains("node_modules")));
        assert_eq!(first.files, 1);
    }

    #[test]
    fn preflight_frameworks_quote_the_same_detection() {
        // #215 single-source invariant: the framework chip strings come out
        // of detect_in_file, so Preflight can never disagree with the graph.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "package.json",
            r#"{ "dependencies": { "react": "^19", "express": "^5" } }"#,
        );
        let detection = detect_in_file(
            "package.json",
            "package.json",
            &dir.path().join("package.json"),
        );
        assert_eq!(detection.framework_labels, vec!["Express", "React"]);
        // And the same call produced the graph-fact tools.
        assert!(detection.tools.iter().any(|t| t.name == "react"));
    }
}
