//! Conformance gate (#200, AC-0068): a plugin may only join extraction
//! after proving, against its own generator-supplied golden corpus, that
//! it (1) honors the SPI contract under the standard bounds, (2) emits
//! exactly the expected facts with exact evidence spans, and (3) is
//! deterministic across a double run. Anything less and the adapter stays
//! in the proposed state — discovery and enablement never bypass this.

use crate::{HostError, PluginExtraction, PluginHost, PluginLimits, SourceId};
use serde::{Deserialize, Serialize};

/// One golden case: source text in, the exact expected facts out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenCase {
    /// Repo-relative path handed to the plugin.
    pub path: String,
    /// UTF-8 source content.
    pub source: String,
    /// Expected nodes, exact (id, label, props) — evidence spans included.
    #[serde(default)]
    pub nodes: Vec<ExpectedNode>,
    /// Expected edges, exact (src, dst, label, props).
    #[serde(default)]
    pub edges: Vec<ExpectedEdge>,
}

/// Expected node shape; `props` compares exactly (JSON equality).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedNode {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub props: serde_json::Value,
}

/// Expected edge shape; `props` compares exactly (JSON equality).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedEdge {
    pub src: String,
    pub dst: String,
    pub label: String,
    #[serde(default)]
    pub props: serde_json::Value,
}

/// The generator-supplied corpus shipped next to the artifact as
/// `{plugin-id}.golden.json`. `extensions` declares which source files the
/// adapter claims once gated (#201 routing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoldenCorpus {
    #[serde(default)]
    pub extensions: Vec<String>,
    pub cases: Vec<GoldenCase>,
}

/// One named gate check with its verdict and a human-readable detail.
#[derive(Debug, Clone, Serialize)]
pub struct GateCheck {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

/// The whole gate outcome. `passed` is the conjunction of every check —
/// one failure keeps the adapter proposed.
#[derive(Debug, Clone, Serialize)]
pub struct GateReport {
    pub passed: bool,
    pub checks: Vec<GateCheck>,
}

fn canonical(extraction: &PluginExtraction) -> Vec<String> {
    let mut facts: Vec<String> = extraction
        .nodes
        .iter()
        .map(|node| format!("node:{}:{}:{}", node.id, node.label, node.props))
        .chain(extraction.edges.iter().map(|edge| {
            format!(
                "edge:{}:{}:{}:{}",
                edge.src, edge.dst, edge.label, edge.props
            )
        }))
        .collect();
    facts.sort();
    facts
}

/// Expected facts, expressed exactly as the host will return them: built
/// into a `PluginExtraction` and pinned with the same identity, so golden
/// authors never hand-write `plugin_artifact_hash` or the pinned
/// extractor id.
fn expected_canonical(case: &GoldenCase, plugin_id: &str, artifact_hash: &str) -> Vec<String> {
    let mut expected = PluginExtraction {
        nodes: case
            .nodes
            .iter()
            .map(|node| core_graph::Node {
                id: node.id.clone(),
                label: node.label.clone(),
                props: node.props.clone(),
            })
            .collect(),
        edges: case
            .edges
            .iter()
            .map(|edge| core_graph::Edge {
                src: edge.src.clone(),
                dst: edge.dst.clone(),
                label: edge.label.clone(),
                props: edge.props.clone(),
            })
            .collect(),
    };
    crate::pin_extraction(&mut expected, plugin_id, artifact_hash);
    canonical(&expected)
}

/// Run the full conformance gate for one artifact against its corpus.
/// Never partial: any bound violation, contract error, golden mismatch, or
/// nondeterminism fails the gate closed with the reason named.
pub fn run_gate(
    host: &PluginHost,
    plugin_id: &str,
    wasm_bytes: &[u8],
    corpus: &GoldenCorpus,
    limits: PluginLimits,
    source_id: &SourceId,
) -> GateReport {
    let mut checks: Vec<GateCheck> = Vec::new();
    let mut check = |name: &str, passed: bool, detail: String| {
        checks.push(GateCheck {
            name: name.to_string(),
            passed,
            detail,
        });
        passed
    };

    let plugin = match host.load(wasm_bytes, plugin_id) {
        Ok(plugin) => {
            check("spi-compiles", true, "component compiles".into());
            plugin
        }
        Err(error) => {
            check("spi-compiles", false, error.to_string());
            return GateReport {
                passed: false,
                checks,
            };
        }
    };

    if !check(
        "corpus-nonempty",
        !corpus.cases.is_empty(),
        format!("{} golden case(s)", corpus.cases.len()),
    ) {
        return GateReport {
            passed: false,
            checks,
        };
    }

    let mut first_runs: Vec<Option<PluginExtraction>> = Vec::new();
    for case in &corpus.cases {
        let run: Result<PluginExtraction, HostError> = host.call_extract(
            &plugin,
            case.source.as_bytes(),
            &case.path,
            source_id,
            limits,
        );
        match run {
            Ok(extraction) => {
                check(
                    &format!("contract:{}", case.path),
                    true,
                    "extract-source honored the SPI within bounds".into(),
                );
                let got = canonical(&extraction);
                let want = expected_canonical(case, plugin_id, plugin.artifact_hash());
                let matched = got == want;
                check(
                    &format!("golden:{}", case.path),
                    matched,
                    if matched {
                        "facts match the golden expectation exactly".into()
                    } else {
                        format!("expected {} fact(s), got {}", want.len(), got.len())
                    },
                );
                first_runs.push(matched.then_some(extraction));
            }
            Err(error) => {
                check(&format!("contract:{}", case.path), false, error.to_string());
                first_runs.push(None);
            }
        }
    }

    // Double-run determinism over the whole corpus: identical canonical
    // facts, case by case.
    let mut deterministic = true;
    let mut detail = "second run identical".to_string();
    for (case, first) in corpus.cases.iter().zip(&first_runs) {
        let Some(first) = first else {
            deterministic = false;
            detail = "skipped: a first-run case already failed".into();
            break;
        };
        match host.call_extract(
            &plugin,
            case.source.as_bytes(),
            &case.path,
            source_id,
            limits,
        ) {
            Ok(second) if canonical(&second) == canonical(first) => {}
            Ok(_) => {
                deterministic = false;
                detail = format!("case {} differed across runs", case.path);
                break;
            }
            Err(error) => {
                deterministic = false;
                detail = format!("case {} failed on rerun: {error}", case.path);
                break;
            }
        }
    }
    check("determinism-double-run", deterministic, detail);

    GateReport {
        passed: checks.iter().all(|check| check.passed),
        checks,
    }
}
