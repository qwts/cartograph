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

/// Directories that are never part of the recovered system.
const SKIP_DIRS: &[&str] = &["node_modules", ".git", "target", "dist", "build", ".venv"];

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
        language: "JavaScript",
        extensions: &["js", "jsx", "mjs", "cjs"],
    },
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

/// Run the preflight scan over `root`. Purely local; deterministic for a
/// given tree (files walked in sorted order).
pub fn preflight(root: &Path) -> std::io::Result<PreflightReport> {
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
                if !SKIP_DIRS.contains(&name.as_str()) {
                    stack.push(path);
                }
                continue;
            }
            detect_framework(&name, &path, &mut frameworks);
            let Some(extension) = path.extension().map(|e| e.to_string_lossy().into_owned()) else {
                continue;
            };
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            // Risky-construct scanning is per-syntax, not per-coverage: a
            // .js file still gets eval/WASM findings even though JavaScript
            // extraction is a planned adapter (#192 review).
            if matches!(
                extension.as_str(),
                "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs"
            ) {
                scan_source(&path, &rel, &mut unsupported, &mut potential_gaps)?;
            }
            if let Some((language, adapter)) = adapter_for(&extension) {
                let entry = languages
                    .entry(language.to_string())
                    .or_insert(LanguageDetection {
                        language: language.to_string(),
                        files: 0,
                        adapter: Some(adapter.to_string()),
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

/// Recognize frameworks from marker files (cheap, deterministic).
fn detect_framework(name: &str, path: &Path, frameworks: &mut Vec<String>) {
    match name {
        "manifest.json" => {
            if std::fs::read_to_string(path).is_ok_and(|text| text.contains("\"manifest_version\""))
            {
                frameworks.push("WebExtension (Manifest V2/V3)".into());
            }
        }
        "package.json" => {
            if let Ok(text) = std::fs::read_to_string(path) {
                for (marker, framework) in [
                    ("\"react\"", "React"),
                    ("\"express\"", "Express"),
                    ("\"next\"", "Next.js"),
                ] {
                    if text.contains(marker) {
                        frameworks.push(framework.into());
                    }
                }
            }
        }
        "go.mod" => frameworks.push("Go module".into()),
        "pyproject.toml" | "requirements.txt" => frameworks.push("Python project".into()),
        "pom.xml" | "build.gradle" | "build.gradle.kts" => frameworks.push("Java project".into()),
        _ => {
            if name.ends_with(".tf") {
                frameworks.push("Terraform".into());
            }
        }
    }
}

/// Line-level construct detectors for covered JS/TS sources. Each detector
/// states which lane it feeds: unsupported (tool limitation) or potential
/// gap (statically unresolvable evidence).
fn scan_source(
    path: &Path,
    rel: &str,
    unsupported: &mut Vec<PatternFinding>,
    potential_gaps: &mut Vec<PatternFinding>,
) -> std::io::Result<()> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(()); // non-UTF-8 source: nothing to scan
    };
    for (index, line) in text.lines().enumerate() {
        let line_no = (index + 1) as u64;
        if line.contains("eval(") {
            unsupported.push(finding(
                "inline-eval",
                rel,
                line_no,
                "inline eval() — no adapter can extract facts from \
                 dynamically evaluated code",
            ));
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
}
