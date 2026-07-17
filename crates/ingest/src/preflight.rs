//! Local-only preflight: detect languages, frameworks, and adapter coverage,
//! and classify risky constructs **before** recovery (#116, AC-0055/AC-0059).
//!
//! T0 discipline: a pure filesystem scan — zero egress, never an LLM. The
//! product's honesty contract starts here with the three-way classification:
//!
//! - **Potential system gaps** — evidence exists but is not statically
//!   resolvable (dynamic injection, runtime-computed identity). These become
//!   explicit Gaps with resolution strategies after recovery, never a guess.
//! - **Unsupported patterns** — no adapter covers the construct. A tool
//!   limitation, **not** a System Gap; the two are never conflated.
//!
//! Detectors are a versioned registry ([`DETECTOR_ID`]): provenance on every
//! finding records which detector produced it, so re-runs are attributable.

use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Versioned identity of this detector registry, recorded on every finding.
pub const DETECTOR_ID: &str = "preflight@1";

/// Directories that are never part of the recovered system. `.cartograph`
/// holds Cartograph's own plugin artifacts/corpora — never project source
/// (a dropped-in `.wasm` plugin must not surface as an uncovered language).
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    ".venv",
    ".cartograph",
];

/// Shared skip predicate — the toolchain walk (#215) must never disagree
/// with Preflight about what is part of the system.
pub(crate) fn skip_dir(name: &str) -> bool {
    SKIP_DIRS.contains(&name)
}

/// One detected language with its adapter coverage.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LanguageDetection {
    /// Language name, e.g. `TypeScript`.
    pub language: String,
    /// Source files found.
    pub files: u64,
    /// Covering adapter id, or `None` when no adapter exists — in which case
    /// the whole language surfaces as an unsupported pattern.
    pub adapter: Option<String>,
}

/// One classified construct: where it is and why it was flagged.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PatternFinding {
    /// Stable finding kind, e.g. `inline-eval`, `dynamic-injection`.
    pub kind: String,
    /// Repo-relative path.
    pub path: String,
    /// 1-based line of the first occurrence.
    pub line: u64,
    /// Human explanation shown in the Preflight card.
    pub message: String,
    /// Detector registry version that produced this finding.
    pub detector: String,
    /// For uncovered-language findings: the language to request an adapter
    /// for — the UI's "request adapter" lane (#201). `None` otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_adapter: Option<String>,
}

/// The full local detection report for one tree.
#[derive(Debug, Clone, Serialize, Default)]
pub struct PreflightReport {
    /// Languages by source-file extension, with adapter coverage.
    pub languages: Vec<LanguageDetection>,
    /// Frameworks/manifests recognized from marker files.
    pub frameworks: Vec<String>,
    /// Constructs no adapter covers — tool limitations, never Gaps.
    pub unsupported: Vec<PatternFinding>,
    /// Constructs that will become explicit Gaps if recovery cannot resolve
    /// them deterministically.
    pub potential_gaps: Vec<PatternFinding>,
    /// Detector registry version used for this report.
    pub detector: String,
}

/// One installed deterministic adapter — the per-language/per-format
/// extractor that turns source into Confirmed facts. This single registry
/// drives Preflight coverage AND the Settings inventory (#163), so the two
/// can never disagree.
#[derive(Debug, Clone, Serialize)]
pub struct AdapterInfo {
    /// Extractor id as recorded in provenance (`extractor_id`).
    pub id: &'static str,
    /// Language or format the adapter covers.
    pub language: &'static str,
    /// Source-file extensions the adapter claims.
    pub extensions: &'static [&'static str],
    /// What the adapter actually extracts, in plain language.
    pub covers: &'static str,
}

/// Every adapter shipped in this build.
pub const INSTALLED_ADAPTERS: &[AdapterInfo] = &[
    AdapterInfo {
        id: "t0.adapter-ts",
        language: "TypeScript",
        extensions: &["ts", "tsx"],
        covers: "imports, call graph, endpoints (Express/Next/React Router), \
                 chrome messaging channels, IndexedDB",
    },
    AdapterInfo {
        id: "t0.adapter-ts",
        language: "JavaScript",
        extensions: &["js", "jsx", "mjs", "cjs"],
        covers: "imports, call graph, endpoints (Express/Next/React Router), \
                 chrome messaging channels, IndexedDB — the TypeScript \
                 crate's own grammar parses plain JS/JSX directly",
    },
    AdapterInfo {
        id: "t0.webextension",
        language: "WebExtension",
        extensions: &[],
        covers: "manifest.json execution contexts (background, content scripts, \
                 popups), keyboard commands, permissions as grants, entry binding",
    },
    AdapterInfo {
        id: "t0.adapter-python",
        language: "Python",
        extensions: &["py"],
        covers: "imports, call graph, FastAPI/Flask endpoint registrations",
    },
    AdapterInfo {
        id: "t0.adapter-go",
        language: "Go",
        extensions: &["go"],
        covers: "imports, call graph, net/http, chi and gin endpoint registrations",
    },
    AdapterInfo {
        id: "t0.adapter-java",
        language: "Java",
        extensions: &["java"],
        covers: "types, methods, call graph, Spring Web endpoint annotations",
    },
    AdapterInfo {
        id: "t0.iac-terraform",
        language: "Terraform",
        extensions: &["tf"],
        covers: "resource DAG, AWS capability edges (TRIGGERS/ROUTES/GRANTS)",
    },
];

/// A named adapter that does not exist yet. Adapters are per *language*,
/// never per toolchain version (a JDK bump is not a new adapter): the
/// grammar covers syntax across versions and version-specific constructs
/// degrade to explicit Unsupported findings.
#[derive(Debug, Clone, Serialize)]
pub struct PlannedAdapter {
    /// Language the future adapter would cover.
    pub language: &'static str,
    /// Extensions Preflight uses to detect (and name) the language today.
    pub extensions: &'static [&'static str],
}

/// The recommendation catalog (#163): languages Preflight can detect and
/// name as installable-later adapter types.
pub const PLANNED_ADAPTERS: &[PlannedAdapter] = &[
    PlannedAdapter {
        language: "C",
        extensions: &["c", "h"],
    },
    PlannedAdapter {
        language: "C++",
        extensions: &["cc", "cpp", "cxx", "hpp", "hh"],
    },
    PlannedAdapter {
        language: "Kotlin",
        extensions: &["kt", "kts"],
    },
    PlannedAdapter {
        language: "Swift",
        extensions: &["swift"],
    },
    PlannedAdapter {
        language: "Objective-C",
        extensions: &["m", "mm"],
    },
];

/// Languages the deterministic tier covers today, keyed by extension.
fn adapter_for(extension: &str) -> Option<(&'static str, &'static str)> {
    INSTALLED_ADAPTERS
        .iter()
        .find(|adapter| adapter.extensions.contains(&extension))
        .map(|adapter| (adapter.language, adapter.id))
}

/// Source languages we can *name* but not extract — surfaced honestly as
/// uncovered rather than silently ignored. Planned adapter types come from
/// the recommendation catalog; the rest are named-only.
fn uncovered_language(extension: &str) -> Option<&'static str> {
    if let Some(planned) = PLANNED_ADAPTERS
        .iter()
        .find(|planned| planned.extensions.contains(&extension))
    {
        return Some(planned.language);
    }
    match extension {
        "rb" => Some("Ruby"),
        "cs" => Some("C#"),
        "php" => Some("PHP"),
        "rs" => Some("Rust"),
        "wasm" => Some("WebAssembly"),
        _ => None,
    }
}

/// True when a `language` has a planned adapter in the catalog.
pub fn planned_adapter_for(language: &str) -> bool {
    PLANNED_ADAPTERS
        .iter()
        .any(|planned| planned.language == language)
}

/// A gated, enabled plugin adapter's coverage claim (#201): the extensions
/// its golden corpus declares. Only plugins that passed the conformance
/// gate for their exact bytes may claim coverage — the caller enforces
/// that; this type just carries the claim.
#[derive(Debug, Clone)]
pub struct PluginCoverage {
    /// The plugin id, reported as the covering adapter.
    pub plugin_id: String,
    /// Extensions the corpus declares, without the leading dot.
    pub extensions: Vec<String>,
}

/// The TS adapter's AST-level classification of one `eval(` site (#214).
/// Preflight's textual scan cannot itself prove an argument compile-time
/// known; the adapter that owns extraction supplies these claims (host-
/// filled, mirroring [`PluginCoverage`]), so the scan and the AST proof can
/// never disagree about which sites are covered.
#[derive(Debug, Clone)]
pub struct EvalSiteCoverage {
    /// Repo-relative path, matching this scan's finding paths.
    pub path: String,
    /// 1-based line of the call, matching this scan's finding lines.
    pub line: u64,
    /// What the adapter's argument proof established.
    pub proof: EvalProof,
}

/// Outcome of the adapter's eval-argument proof (#214).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalProof {
    /// Compile-time-proven literal — the adapter extracted its facts as
    /// Confirmed T0, so the `inline-eval` finding is covered and closes
    /// (the #201 finding-closure semantics).
    Covered,
    /// Const-shaped but unproven — downgraded to an explicit potential Gap.
    ConstUnproven,
    /// Runtime-computed — stays an Unsupported finding.
    Dynamic,
}

/// Run the preflight scan over `root`. Purely local; deterministic for a
/// given tree (files walked in sorted order).
pub fn preflight(root: &Path) -> std::io::Result<PreflightReport> {
    preflight_with_plugins(root, &[])
}

/// Preflight with gated plugin adapters in the coverage set (#201): an
/// extension a plugin claims counts as covered (the plugin id is the
/// adapter), so the uncovered-language finding for it closes on the next
/// scan instead of dead-ending. Compiled-in adapters win over plugins.
pub fn preflight_with_plugins(
    root: &Path,
    plugins: &[PluginCoverage],
) -> std::io::Result<PreflightReport> {
    preflight_with_coverage(root, plugins, &[])
}

/// [`preflight_with_plugins`] plus the adapter's eval-site claims (#214):
/// an `eval(` or `new Function(` line whose every AST site is a proven
/// literal is covered — no finding, it closes on this scan like plugin
/// coverage closes uncovered-language findings — while const-shaped-but-
/// unproven sites downgrade to explicit potential Gaps and everything else
/// (including lines the adapter never classified) stays Unsupported.
/// Never a guess.
pub fn preflight_with_coverage(
    root: &Path,
    plugins: &[PluginCoverage],
    eval_sites: &[EvalSiteCoverage],
) -> std::io::Result<PreflightReport> {
    let mut languages: BTreeMap<String, LanguageDetection> = BTreeMap::new();
    let mut frameworks: Vec<String> = Vec::new();
    let mut unsupported = Vec::new();
    let mut potential_gaps = Vec::new();

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
                if !skip_dir(&name) {
                    stack.push(path);
                }
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            // Framework chips and config-behind-code findings quote the
            // toolchain registry (#215) — the same detection that produces
            // the graph's Tool facts, so the two surfaces cannot disagree.
            let detection = crate::toolchain::detect_in_file(&name, &rel, &path);
            frameworks.extend(detection.framework_labels);
            for tool in &detection.tools {
                if tool.settings_behind_code {
                    unsupported.push(PatternFinding {
                        kind: "config-behind-code".into(),
                        path: rel.clone(),
                        line: 1,
                        message: format!(
                            "{} is authored in code — detected by presence; its \
                             settings are not evaluated and stay uncited",
                            tool.display
                        ),
                        detector: DETECTOR_ID.into(),
                        request_adapter: None,
                    });
                }
            }
            let Some(extension) = path.extension().map(|e| e.to_string_lossy().into_owned()) else {
                continue;
            };
            // Risky-construct scanning is per-syntax, not per-coverage: a
            // .ts file gets the same eval/WASM findings as .js (#192
            // review) — both extensions are covered by the same adapter.
            if matches!(
                extension.as_str(),
                "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs"
            ) {
                scan_source(
                    &path,
                    &rel,
                    eval_sites,
                    &mut unsupported,
                    &mut potential_gaps,
                )?;
            }
            let plugin_claim = plugins
                .iter()
                .find(|plugin| plugin.extensions.contains(&extension));
            if let Some((language, adapter)) = adapter_for(&extension) {
                let entry = languages
                    .entry(language.to_string())
                    .or_insert(LanguageDetection {
                        language: language.to_string(),
                        files: 0,
                        adapter: Some(adapter.to_string()),
                    });
                entry.files += 1;
            } else if let Some(plugin) = plugin_claim {
                // A gated plugin covers this extension (#201): report it as
                // covered under the plugin id — no unsupported finding.
                let language = uncovered_language(&extension)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!(".{extension}"));
                let entry = languages
                    .entry(language.clone())
                    .or_insert(LanguageDetection {
                        language,
                        files: 0,
                        adapter: Some(plugin.plugin_id.clone()),
                    });
                entry.files += 1;
            } else if let Some(language) = uncovered_language(&extension) {
                let entry = languages
                    .entry(language.to_string())
                    .or_insert(LanguageDetection {
                        language: language.to_string(),
                        files: 0,
                        adapter: None,
                    });
                entry.files += 1;
                if entry.files == 1 {
                    // A planned adapter type is a recommendation, not a dead
                    // end (#163): name the missing adapter and where to ask.
                    let message = if planned_adapter_for(language) {
                        format!(
                            "{language} sources present but no adapter covers them — \
                             a tool limitation, not a System Gap. A {language} adapter \
                             is a known adapter type: request it from Settings → Adapters"
                        )
                    } else {
                        format!(
                            "{language} sources present but no adapter covers them — \
                             a tool limitation, not a System Gap"
                        )
                    };
                    unsupported.push(PatternFinding {
                        kind: "uncovered-language".into(),
                        path: rel,
                        line: 1,
                        message,
                        detector: DETECTOR_ID.into(),
                        request_adapter: Some(language.to_string()),
                    });
                }
            }
        }
    }

    frameworks.sort();
    frameworks.dedup();
    Ok(PreflightReport {
        languages: languages.into_values().collect(),
        frameworks,
        unsupported,
        potential_gaps,
        detector: DETECTOR_ID.into(),
    })
}

/// Line-level construct detectors for covered JS/TS sources. Each detector
/// states which lane it feeds: unsupported (tool limitation) or potential
/// gap (statically unresolvable evidence).
fn scan_source(
    path: &Path,
    rel: &str,
    eval_sites: &[EvalSiteCoverage],
    unsupported: &mut Vec<PatternFinding>,
    potential_gaps: &mut Vec<PatternFinding>,
) -> std::io::Result<()> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(()); // non-UTF-8 source: nothing to scan
    };
    for (index, line) in text.lines().enumerate() {
        let line_no = (index + 1) as u64;
        let has_eval = line.contains("eval(");
        let has_new_function = line.contains("new Function(");
        if has_eval || has_new_function {
            // The adapter's AST claims refine the textual hit (#214); both
            // dynamic-code constructs share one claim pool per line,
            // worst-wins: one unproven site keeps the line flagged even
            // next to a proven one — never guess. A line with no claim at
            // all (nothing the adapter recognized as the global
            // eval/Function) keeps its Unsupported finding.
            let claims: Vec<EvalProof> = eval_sites
                .iter()
                .filter(|claim| claim.path == rel && claim.line == line_no)
                .map(|claim| claim.proof)
                .collect();
            if claims.is_empty() || claims.contains(&EvalProof::Dynamic) {
                let message = if has_eval {
                    "inline eval() — no adapter can extract facts from \
                     dynamically evaluated code"
                } else {
                    "new Function() — no adapter can extract facts from \
                     dynamically constructed code"
                };
                unsupported.push(finding("inline-eval", rel, line_no, message));
            } else if claims.contains(&EvalProof::ConstUnproven) {
                potential_gaps.push(finding(
                    "inline-eval",
                    rel,
                    line_no,
                    "a const-shaped eval()/new Function() argument the \
                     adapter could not prove to a literal — becomes an \
                     explicit Gap if recovery cannot resolve it \
                     deterministically",
                ));
            }
            // Every site on the line proved literal: the adapter extracted
            // its facts as Confirmed T0 — covered, the finding closes.
        }
        if line.contains("WebAssembly.instantiate") {
            unsupported.push(finding(
                "wasm-module",
                rel,
                line_no,
                "WebAssembly module instantiation — no adapter covers WASM",
            ));
        }
        if line.contains("executeScript") {
            potential_gaps.push(finding(
                "dynamic-injection",
                rel,
                line_no,
                "dynamically injected script body — becomes an explicit Gap \
                 if recovery cannot resolve the injected function statically",
            ));
        }
        // `import(` followed by anything but a string literal is a
        // runtime-computed module identity.
        if let Some(offset) = line.find("import(") {
            let after = line[offset + "import(".len()..].trim_start();
            if !after.starts_with('\'') && !after.starts_with('"') && !after.starts_with('`') {
                potential_gaps.push(finding(
                    "computed-import",
                    rel,
                    line_no,
                    "dynamic import with a runtime-computed specifier — \
                     becomes an explicit Gap if unresolvable",
                ));
            }
        }
    }
    Ok(())
}

fn finding(kind: &str, path: &str, line: u64, message: &str) -> PatternFinding {
    PatternFinding {
        kind: kind.into(),
        path: path.into(),
        line,
        message: message.into(),
        detector: DETECTOR_ID.into(),
        request_adapter: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn classifies_unsupported_and_potential_gaps_distinctly() {
        // AC-0059 groundwork: unsupported ≠ gap, from first contact.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/app.ts",
            "const r = eval(input);\nchrome.scripting.executeScript({ func });\n",
        );
        write(
            root,
            "src/load.ts",
            "const mod = await import(pluginPath);\n",
        );
        write(
            root,
            "src/ok.ts",
            "import x from './x';\nconst m = import('./static');\n",
        );

        let report = preflight(root).unwrap();
        let unsupported: Vec<_> = report.unsupported.iter().map(|f| f.kind.as_str()).collect();
        let gaps: Vec<_> = report
            .potential_gaps
            .iter()
            .map(|f| f.kind.as_str())
            .collect();
        assert_eq!(unsupported, vec!["inline-eval"]);
        assert_eq!(gaps, vec!["dynamic-injection", "computed-import"]);
        // Static import specifiers never flag.
        assert!(
            !report
                .potential_gaps
                .iter()
                .any(|f| f.path.ends_with("ok.ts"))
        );
        // Every finding carries the versioned detector.
        assert!(report.unsupported.iter().all(|f| f.detector == DETECTOR_ID));
    }

    #[test]
    fn adapter_eval_claims_reclassify_inline_eval_findings() {
        // AC-0099 (#214): mirrors gated_plugin_coverage — the TS adapter's
        // AST proof is the single authority on eval sites; preflight only
        // consumes host-filled claims, so scan and extraction never disagree.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/app.ts",
            concat!(
                "eval('function f() {}');\n",  // proven literal
                "eval(CODE);\n",               // const-shaped, unproven
                "eval(input + '()');\n",       // runtime-computed
                "eval('x()'); eval(dyn());\n", // proven + dynamic on one line
                "new Function(body);\n",       // const-shaped, unproven
                "new Function('return 1');\n", // proven literal body
            ),
        );

        // Without claims every textual hit — eval() and new Function()
        // alike (#217 review) — stays Unsupported.
        let before = preflight(root).unwrap();
        assert_eq!(
            before
                .unsupported
                .iter()
                .filter(|f| f.kind == "inline-eval")
                .count(),
            6
        );
        assert!(before.potential_gaps.is_empty());

        let claim = |line: u64, proof: EvalProof| EvalSiteCoverage {
            path: "src/app.ts".into(),
            line,
            proof,
        };
        let claims = [
            claim(1, EvalProof::Covered),
            claim(2, EvalProof::ConstUnproven),
            claim(3, EvalProof::Dynamic),
            claim(4, EvalProof::Covered),
            claim(4, EvalProof::Dynamic),
            claim(5, EvalProof::ConstUnproven),
            claim(6, EvalProof::Covered),
        ];
        let after = preflight_with_coverage(root, &[], &claims).unwrap();
        let unsupported: Vec<u64> = after
            .unsupported
            .iter()
            .filter(|f| f.kind == "inline-eval")
            .map(|f| f.line)
            .collect();
        // Lines 1 and 6 are covered and close; line 3 stays; line 4 keeps
        // its finding because one site on it is dynamic (worst-wins, no
        // guess).
        assert_eq!(unsupported, vec![3, 4]);
        let gaps: Vec<u64> = after
            .potential_gaps
            .iter()
            .filter(|f| f.kind == "inline-eval")
            .map(|f| f.line)
            .collect();
        // The eval const line and the new Function const line both
        // downgrade — new Function claims reconcile through the same pool.
        assert_eq!(gaps, vec![2, 5]);
        // The downgraded finding still carries the versioned detector.
        assert!(
            after
                .potential_gaps
                .iter()
                .all(|f| f.detector == DETECTOR_ID)
        );
    }

    #[test]
    fn detects_languages_frameworks_and_adapter_coverage() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/app.ts", "export const a = 1;\n");
        write(root, "main.go", "package main\n");
        write(
            root,
            "infra/net.tf",
            "resource \"aws_sqs_queue\" \"q\" {}\n",
        );
        write(root, "legacy/report.rb", "puts 'hi'\n");
        write(
            root,
            "manifest.json",
            "{\"manifest_version\": 3, \"name\": \"x\"}\n",
        );
        write(
            root,
            "package.json",
            "{\"dependencies\": {\"react\": \"^19\"}}\n",
        );
        // Ignored tree must not contribute detections.
        write(root, "node_modules/dep/index.js", "eval('x')\n");

        let report = preflight(root).unwrap();
        let by_language: BTreeMap<_, _> = report
            .languages
            .iter()
            .map(|l| (l.language.as_str(), l))
            .collect();
        assert_eq!(
            by_language["TypeScript"].adapter.as_deref(),
            Some("t0.adapter-ts")
        );
        assert_eq!(by_language["Go"].adapter.as_deref(), Some("t0.adapter-go"));
        assert_eq!(
            by_language["Terraform"].adapter.as_deref(),
            Some("t0.iac-terraform")
        );
        // Ruby is named but uncovered — and surfaces as an unsupported finding.
        assert_eq!(by_language["Ruby"].adapter, None);
        assert!(
            report
                .unsupported
                .iter()
                .any(|f| f.kind == "uncovered-language" && f.message.contains("Ruby"))
        );
        assert!(report.frameworks.iter().any(|f| f.contains("WebExtension")));
        assert!(report.frameworks.contains(&"React".to_string()));
        // node_modules is skipped: no eval finding from the dependency.
        assert!(
            !report
                .unsupported
                .iter()
                .any(|f| f.path.contains("node_modules"))
        );
    }

    #[test]
    fn javascript_is_covered_not_unsupported() {
        // AC-0095: JavaScript used to be a PLANNED_ADAPTERS recommendation
        // ("request it from Settings → Adapters"); the TypeScript crate's
        // grammar now parses it directly, so a JS-only tree is covered like
        // any other installed adapter — no uncovered-language finding, no
        // adapter-request message.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/index.js", "export function run() {}\n");
        write(root, "src/Widget.jsx", "export function Widget() {}\n");

        let report = preflight(root).unwrap();
        let by_language: BTreeMap<_, _> = report
            .languages
            .iter()
            .map(|l| (l.language.as_str(), l))
            .collect();
        assert_eq!(
            by_language["JavaScript"].adapter.as_deref(),
            Some("t0.adapter-ts")
        );
        assert_eq!(by_language["JavaScript"].files, 2);
        assert!(
            !report
                .unsupported
                .iter()
                .any(|f| f.message.contains("JavaScript"))
        );
        assert!(!planned_adapter_for("JavaScript"));
    }

    #[test]
    fn adapter_registry_drives_coverage_and_recommendations() {
        // AC-0087 (#163): one registry serves Preflight and Settings — every
        // installed adapter's extensions resolve back to exactly that
        // adapter, so the inventory can never disagree with coverage.
        for adapter in INSTALLED_ADAPTERS {
            for extension in adapter.extensions {
                assert_eq!(adapter_for(extension), Some((adapter.language, adapter.id)));
            }
        }
        // Planned adapter types are detectable and marked requestable.
        for planned in PLANNED_ADAPTERS {
            assert!(planned_adapter_for(planned.language));
            for extension in planned.extensions {
                assert_eq!(uncovered_language(extension), Some(planned.language));
                assert_eq!(adapter_for(extension), None);
            }
        }
        // Named-only languages are honest but not in the request catalog.
        assert_eq!(uncovered_language("rb"), Some("Ruby"));
        assert!(!planned_adapter_for("Ruby"));

        // A detected planned language recommends the adapter in place.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "App.kt", "fun main() {}\n");
        let report = preflight(dir.path()).unwrap();
        let finding = report
            .unsupported
            .iter()
            .find(|f| f.kind == "uncovered-language" && f.message.contains("Kotlin"))
            .expect("Kotlin surfaces as uncovered");
        assert!(
            finding
                .message
                .contains("request it from Settings → Adapters")
        );
    }

    #[test]
    fn gated_plugin_coverage_closes_the_uncovered_finding() {
        // AC-0093 (#201): the same tree, rescanned with a gated plugin
        // claiming the extension, reports the language covered under the
        // plugin id — the uncovered-language finding closes — and the
        // finding it replaces carried the request-adapter action.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app.rb", "puts 'hi'\n");

        let before = preflight(dir.path()).unwrap();
        let finding = before
            .unsupported
            .iter()
            .find(|f| f.kind == "uncovered-language" && f.message.contains("Ruby"))
            .expect("Ruby surfaces as uncovered");
        assert_eq!(finding.request_adapter.as_deref(), Some("Ruby"));

        let plugins = [PluginCoverage {
            plugin_id: "t0.plugin-ruby".into(),
            extensions: vec!["rb".into()],
        }];
        let after = preflight_with_plugins(dir.path(), &plugins).unwrap();
        let ruby = after
            .languages
            .iter()
            .find(|l| l.language == "Ruby")
            .expect("Ruby still detected");
        assert_eq!(ruby.adapter.as_deref(), Some("t0.plugin-ruby"));
        assert!(
            !after
                .unsupported
                .iter()
                .any(|f| f.kind == "uncovered-language" && f.message.contains("Ruby"))
        );

        // Compiled-in adapters always win over a plugin claiming the same
        // extension — first-class languages stay compiled in.
        write(dir.path(), "main.ts", "export const x = 1;\n");
        let contested = [PluginCoverage {
            plugin_id: "t0.plugin-rogue".into(),
            extensions: vec!["ts".into()],
        }];
        let report = preflight_with_plugins(dir.path(), &contested).unwrap();
        let ts = report
            .languages
            .iter()
            .find(|l| l.language == "TypeScript")
            .expect("TypeScript detected");
        assert_eq!(ts.adapter.as_deref(), Some("t0.adapter-ts"));
    }
}
