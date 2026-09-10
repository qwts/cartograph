//! Stored, typed rule observations → source-evidence inventory (SPEC-02).
//!
//! No source loader is accepted here: all displays came through the producer's
//! sanitizer before persistence. Parsing checks the typed payload, not secrecy.

use crate::{SpecAssertion, provenance};
use core_graph::rules::{
    BranchPolarity, DependencyResolution, GuardedExitEvidence, KnownLiteral, LiteralEvidence,
    LocalExit, SourceExpression,
};
use core_graph::{Edge, Node};
use core_prov::EvidenceRef;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

// Stored source is data, including Markdown/HTML-looking syntax. Escape it
// instead of allowing an expression to create headings, links or table rows.
fn text(value: &str) -> String {
    let mut safe = String::new();
    for character in value.chars() {
        match character {
            '&' => safe.push_str("&amp;"),
            '<' => safe.push_str("&lt;"),
            '>' => safe.push_str("&gt;"),
            '\r' => {}
            '\n' => safe.push_str(" ⏎ "),
            '\\' | '`' | '*' | '_' | '[' | ']' | '|' | '#' | '!' => {
                safe.push('\\');
                safe.push(character);
            }
            other => safe.push(other),
        }
    }
    safe
}

fn source(reference: &EvidenceRef) -> String {
    text(&format!(
        "{}:{} bytes {}..{} @ {}",
        reference.repo,
        reference.path,
        reference.byte_start,
        reference.byte_end,
        reference.commit_sha
    ))
}

fn literal(value: &Option<LiteralEvidence>) -> String {
    match value {
        None => "Not a captured scalar literal".into(),
        Some(LiteralEvidence::Known { value }) => match value {
            KnownLiteral::Boolean(value) => format!("boolean {value}"),
            KnownLiteral::String(value) => format!(
                "string {}",
                text(&serde_json::to_string(value).expect("string serializes"))
            ),
            KnownLiteral::Number(value) => format!("number {value}"),
            KnownLiteral::Null => "null".into(),
        },
        Some(LiteralEvidence::Withheld { kind, reasons }) => {
            format!("Withheld {kind:?}: {reasons:?}")
        }
    }
}

fn expression(output: &mut String, value: &SourceExpression) {
    writeln!(
        output,
        "- Stored expression: {}\n- Capture: {:?} ({})\n- Literal: {}\n- Expression source: {}\n",
        text(value.display.as_str()),
        value.capture,
        text(&value.syntax_kind),
        literal(&value.literal),
        source(&value.source)
    )
    .expect("write to string");
}

fn reference(id: &str, visible: &BTreeMap<&str, &Node>) -> String {
    if let Some(node) = visible.get(id) {
        if node.props["computed_name"] == true {
            format!(
                "{} (computed source name omitted; runtime key unresolved)",
                text(id)
            )
        } else if node.props["name_capture"] == "nonidentifier_key_omitted" {
            format!("{} (source name omitted)", text(id))
        } else {
            text(id)
        }
    } else {
        "Unavailable in this export".into()
    }
}

pub(crate) fn inventory(nodes: &[&Node], edges: &[&Edge]) -> (String, Vec<SpecAssertion>) {
    let visible: BTreeMap<_, _> = nodes.iter().map(|node| (node.id.as_str(), *node)).collect();
    let mut rules: Vec<_> = nodes
        .iter()
        .copied()
        .filter(|node| node.label == "BusinessRule")
        .collect();
    rules.sort_by(|left, right| left.id.cmp(&right.id));
    let mut output = String::from(
        "# Source rule evidence\n\nThese are guarded local exit observations from stored source evidence. \
        A complete execution predicate and the consumer effect are not established. \
        Source order is lexical, not runtime order. Coverage is limited to the facts in this export.\n\n",
    );
    let mut assertions = Vec::new();
    let mut asserted_edges = BTreeSet::new();
    if rules.is_empty() {
        output.push_str("No source-rule observations are available in this export.\n");
    }
    for node in rules {
        let producing = provenance(&node.props, &node.id);
        writeln!(
            output,
            "## Observation {}\n\nProducing tier: {:?} · Confidence: {:?}\n",
            text(&node.id),
            producing.tier,
            producing.confidence_tier
        )
        .expect("write to string");
        let parsed = GuardedExitEvidence::from_value(node.props["rule"].clone());
        assertions.push(SpecAssertion {
            id: format!("node:{}", node.id),
            subject_id: node.id.clone(),
            subject_kind: "BusinessRule".into(),
            summary: if parsed.is_ok() {
                "Guarded local exit observation; behavioral interpretation not established"
            } else {
                "Source-rule payload unavailable: malformed or unsupported schema"
            }
            .into(),
            provenance: producing,
        });
        let Ok(rule) = parsed else {
            output.push_str("Source-rule payload unavailable: malformed or unsupported schema. No source expressions were rendered.\n\n");
            continue;
        };
        writeln!(
            output,
            "Owner: {}\n\nExit source: {}\n\nSource order within callable: {} (zero-based)\n\n\
            Complete execution predicate: not established.\n\nConsumer effect: not established.\n",
            reference(&rule.owner_id, &visible),
            source(&rule.exit_source),
            rule.source_order
        )
        .expect("write to string");
        output.push_str("### Same-callable branch conditions\n\n| Order | Polarity | Stored expression | Capture | Branch source | Expression source |\n|---|---|---|---|---|---|\n");
        if rule.conditions.is_empty() {
            output.push_str("| — | — | No branch conditions captured | — | — | — |\n");
        }
        for (index, condition) in rule.conditions.iter().enumerate() {
            let polarity = match condition.polarity {
                BranchPolarity::TruthyBranch => "truthy branch",
                BranchPolarity::FalsyBranch => "falsy branch",
            };
            writeln!(
                output,
                "| {} | {} | {} | {:?} | {} | {} |",
                index,
                polarity,
                text(condition.expression.display.as_str()),
                condition.expression.capture,
                source(&condition.branch_source),
                source(&condition.expression.source)
            )
            .expect("write to string");
        }
        output.push_str("\n### Local effect\n\n");
        match &rule.effect {
            LocalExit::Return { value } => {
                output.push_str("Return from this callable.\n\n");
                match value {
                    Some(value) => expression(&mut output, value),
                    None => output.push_str("Bare return: no value expression.\n\n"),
                }
            }
            LocalExit::Throw { value } => {
                output.push_str(
                    "Throw from this source location; exception handling is not established.\n\n",
                );
                expression(&mut output, value);
            }
        }
        output.push_str("### Dependencies\n\n| Role | Resolution | Source |\n|---|---|---|\n");
        for dependency in &rule.dependencies {
            let resolved = match &dependency.resolution {
                DependencyResolution::Binding {
                    binding_id,
                    declaration,
                } => format!(
                    "Binding {} declared at {}",
                    text(binding_id),
                    source(declaration)
                ),
                DependencyResolution::Target { node_id } => {
                    format!("Target {}", reference(node_id, &visible))
                }
                DependencyResolution::Unresolved { gap_id } => {
                    format!("Unresolved: {}", reference(gap_id, &visible))
                }
            };
            writeln!(
                output,
                "| {:?} | {} | {} |",
                dependency.role,
                resolved,
                source(&dependency.source)
            )
            .expect("write to string");
        }
        if rule.dependencies.is_empty() {
            output.push_str(
                "| — | No dependencies captured; completeness is not established | — |\n",
            );
        }
        output.push_str("\nInterpretation gaps:\n\n");
        if rule.interpretation.gap_ids.is_empty() {
            output.push_str("- No gap references supplied; behavioral interpretation remains not established.\n");
        }
        for gap in &rule.interpretation.gap_ids {
            writeln!(output, "- {}", reference(gap, &visible)).expect("write to string");
        }
        output.push_str("\n### Redactions\n\n");
        if rule.redactions.is_empty() {
            output.push_str("No redactions recorded in this payload.\n");
        }
        for redaction in &rule.redactions {
            writeln!(
                output,
                "- {:?}: {}",
                redaction.reason,
                source(&redaction.source)
            )
            .expect("write to string");
        }
        output.push_str("\n### Visible graph relationships\n\n| Relation | Target | Tier | Confidence |\n|---|---|---|---|\n");
        let mut relationships: Vec<_> = edges
            .iter()
            .copied()
            .filter(|edge| {
                edge.src == node.id
                    && matches!(edge.label.as_str(), "GOVERNS" | "DEPENDS_ON")
                    && visible.contains_key(edge.dst.as_str())
            })
            .collect();
        relationships
            .sort_by(|left, right| (&left.label, &left.dst).cmp(&(&right.label, &right.dst)));
        if relationships.is_empty() {
            output.push_str("| — | No relationships available in this export | — | — |\n");
        }
        for edge in relationships {
            let identity = format!("{} {} {}", edge.src, edge.label, edge.dst);
            let producing = provenance(&edge.props, &identity);
            writeln!(
                output,
                "| {} | {} | {:?} | {:?} |",
                edge.label,
                text(&edge.dst),
                producing.tier,
                producing.confidence_tier
            )
            .expect("write to string");
            if asserted_edges.insert(identity.clone()) {
                assertions.push(SpecAssertion {
                    id: format!("edge:{identity}"),
                    subject_id: identity,
                    subject_kind: edge.label.clone(),
                    summary: format!("Source-rule {} relationship", edge.label),
                    provenance: producing,
                });
            }
        }
        output.push('\n');
    }
    (output, assertions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExportMode, SpecBundle, compile_spec};
    use core_graph::rules::*;
    use core_prov::{ConfidenceTier, Provenance, Tier};
    use serde_json::json;

    fn evidence() -> EvidenceRef {
        EvidenceRef {
            repo: "example/shop".into(),
            path: "unavailable/source.ts".into(),
            byte_start: 10,
            byte_end: 25,
            commit_sha: "abc123".into(),
        }
    }

    fn payload() -> GuardedExitEvidence {
        let value = SourceExpression {
            source: evidence(),
            syntax_kind: "false".into(),
            display: SanitizedDisplay::from_sanitized("false".into()),
            capture: ExpressionCapture::CompleteSyntax,
            literal: Some(LiteralEvidence::Known {
                value: KnownLiteral::Boolean(false),
            }),
        };
        GuardedExitEvidence {
            schema_version: RULE_EVIDENCE_SCHEMA_VERSION,
            kind: RuleKind::GuardedExit,
            owner_id: "sym:owner".into(),
            exit_source: evidence(),
            source_order: 0,
            conditions: vec![BranchCondition {
                branch_source: evidence(),
                expression: SourceExpression {
                    source: evidence(),
                    syntax_kind: "binary_expression".into(),
                    display: SanitizedDisplay::from_sanitized("options.enabled !== false".into()),
                    capture: ExpressionCapture::CompleteSyntax,
                    literal: None,
                },
                polarity: BranchPolarity::TruthyBranch,
            }],
            effect: LocalExit::Return { value: Some(value) },
            dependencies: vec![
                RuleDependency {
                    role: DependencyRole::Condition,
                    source: evidence(),
                    resolution: DependencyResolution::Binding {
                        binding_id: "binding:options".into(),
                        declaration: evidence(),
                    },
                },
                RuleDependency {
                    role: DependencyRole::ExitValue,
                    source: evidence(),
                    resolution: DependencyResolution::Target {
                        node_id: "sym:owner".into(),
                    },
                },
                RuleDependency {
                    role: DependencyRole::ControlFlow,
                    source: evidence(),
                    resolution: DependencyResolution::Unresolved {
                        gap_id: "gap:consumer".into(),
                    },
                },
            ],
            interpretation: Interpretation {
                execution_predicate: InterpretationStatus::NotEstablished,
                consumer_effect: InterpretationStatus::NotEstablished,
                gap_ids: vec!["gap:consumer".into()],
            },
            redactions: vec![],
        }
    }

    fn node(id: &str, label: &str, tier: Tier, confidence: ConfidenceTier) -> Node {
        let provenance = Provenance::new(
            tier,
            confidence,
            vec![evidence()],
            "spec.rules.test",
            id.as_bytes(),
        )
        .unwrap();
        Node {
            id: id.into(),
            label: label.into(),
            props: json!({"name": id, "prov": provenance}),
        }
    }

    fn rule_node(id: &str, tier: Tier, confidence: ConfidenceTier) -> Node {
        let mut node = node(id, "BusinessRule", tier, confidence);
        node.props["rule"] = serde_json::to_value(payload()).unwrap();
        node
    }

    fn edge(src: &str, dst: &str, label: &str, tier: Tier, confidence: ConfidenceTier) -> Edge {
        Edge {
            src: src.into(),
            dst: dst.into(),
            label: label.into(),
            props: json!({"prov":
            Provenance::new(tier, confidence, vec![evidence()], "spec.rules.test", format!("{src} {label} {dst}").as_bytes()).unwrap()}),
        }
    }

    fn base() -> Vec<Node> {
        vec![
            node(
                "sym:owner",
                "Symbol",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            node(
                "gap:consumer",
                "Gap",
                Tier::Deterministic,
                ConfidenceTier::Gap,
            ),
        ]
    }

    fn artifact(bundle: &SpecBundle) -> &crate::SpecArtifact {
        bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "rule-evidence")
            .unwrap()
    }

    #[test]
    fn rule_inventory_renders_cited_local_observations_deterministically() {
        // AC-0125: inventory uses the shared stored facts, not a source re-read.
        let mut nodes = base();
        nodes.push(rule_node(
            "rule:guard",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        ));
        let edges = vec![
            edge(
                "rule:guard",
                "sym:owner",
                "GOVERNS",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            edge(
                "rule:guard",
                "gap:consumer",
                "DEPENDS_ON",
                Tier::Deterministic,
                ConfidenceTier::Gap,
            ),
        ];
        let bundle = compile_spec(
            &nodes,
            &edges,
            &[],
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
        );
        let output = artifact(&bundle);
        assert_eq!(output.file_name, "rule-evidence.md");
        for expected in [
            "guarded local exit observations",
            "Complete execution predicate: not established",
            "Consumer effect: not established",
            "sym:owner",
            "options.enabled",
            "false",
            "truthy branch",
            "boolean false",
            "Return from this callable",
            "Binding binding:options",
            "gap:consumer",
            "unavailable/source.ts bytes 10..25 @ abc123",
            "Deterministic",
            "Confirmed",
            "DEPENDS_ON",
            "GOVERNS",
        ] {
            assert!(
                output.content.contains(expected),
                "inventory omitted {expected}"
            );
        }
        assert_eq!(output.assertions.len(), 3);
        assert_eq!(
            output.assertions[0].provenance.confidence_tier,
            ConfidenceTier::Confirmed
        );
        assert!(
            output
                .assertions
                .iter()
                .all(|assertion| assertion.summary.starts_with("Guarded local")
                    || assertion.summary.starts_with("Source-rule"))
        );
        let mut reversed_edges = edges;
        reversed_edges.reverse();
        nodes.reverse();
        assert_eq!(
            bundle,
            compile_spec(
                &nodes,
                &reversed_edges,
                &[],
                ExportMode::VerifiedOnly,
                &BTreeSet::new()
            )
        );
    }

    #[test]
    fn rule_inventory_and_relationships_share_export_and_rejection_policy() {
        // AC-0125 / R-INT-5: a confirmed relation cannot leak an excluded rule.
        let mut nodes = base();
        for (id, tier, confidence) in [
            (
                "rule:confirmed",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            ),
            (
                "rule:strong",
                Tier::Semantic,
                ConfidenceTier::InferredStrong,
            ),
            ("rule:weak", Tier::Agentic, ConfidenceTier::InferredWeak),
        ] {
            nodes.push(rule_node(id, tier, confidence));
        }
        let edges: Vec<_> = ["rule:confirmed", "rule:strong", "rule:weak"]
            .into_iter()
            .flat_map(|id| {
                [
                    edge(
                        id,
                        "sym:owner",
                        "GOVERNS",
                        Tier::Deterministic,
                        ConfidenceTier::Confirmed,
                    ),
                    edge(
                        id,
                        "gap:consumer",
                        "DEPENDS_ON",
                        Tier::Deterministic,
                        ConfidenceTier::Gap,
                    ),
                ]
            })
            .collect();
        let verified = compile_spec(
            &nodes,
            &edges,
            &[],
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
        );
        let verified_json = serde_json::to_string(&verified).unwrap();
        assert!(verified_json.contains("rule:confirmed"));
        assert!(verified_json.contains("rule:strong"));
        assert!(!verified_json.contains("rule:weak"));
        let best = compile_spec(
            &nodes,
            &edges,
            &[],
            ExportMode::BestEffort,
            &BTreeSet::new(),
        );
        assert!(artifact(&best).content.contains("rule:weak"));
        assert!(artifact(&best).content.contains("InferredWeak"));
        let rejected: BTreeSet<_> = nodes
            .iter()
            .filter(|node| ["rule:weak", "rule:strong"].contains(&node.id.as_str()))
            .map(|node| {
                node.props["prov"]["content_hash"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        let curated = compile_spec(&nodes, &edges, &[], ExportMode::BestEffort, &rejected);
        let curated_json = serde_json::to_string(&curated).unwrap();
        assert!(curated_json.contains("rule:confirmed"));
        assert!(!curated_json.contains("rule:weak"));
        assert!(!curated_json.contains("rule:strong"));
        assert!(
            curated_json.contains("gap:consumer"),
            "independent explicit gaps remain visible"
        );
    }

    #[test]
    fn rule_inventory_hides_policy_excluded_dependency_targets_and_links() {
        // AC-0125: payload references do not bypass target visibility policy.
        let mut nodes = base();
        nodes.push(node(
            "sym:hidden",
            "Symbol",
            Tier::Agentic,
            ConfidenceTier::InferredWeak,
        ));
        let mut rule = rule_node("rule:guard", Tier::Deterministic, ConfidenceTier::Confirmed);
        let mut observed = payload();
        observed.owner_id = "sym:hidden".into();
        observed.dependencies[1].resolution = DependencyResolution::Target {
            node_id: "sym:hidden".into(),
        };
        rule.props["rule"] = serde_json::to_value(observed).unwrap();
        nodes.push(rule);
        let edges = vec![edge(
            "rule:guard",
            "sym:hidden",
            "GOVERNS",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        )];
        let bundle = compile_spec(
            &nodes,
            &edges,
            &[],
            ExportMode::VerifiedOnly,
            &BTreeSet::new(),
        );
        assert!(
            artifact(&bundle)
                .content
                .contains("Unavailable in this export")
        );
        assert!(
            !serde_json::to_string(&bundle)
                .unwrap()
                .contains("sym:hidden")
        );
    }

    #[test]
    fn malformed_rule_payload_uses_a_fixed_diagnostic_without_echoing_input() {
        // AC-0125: malformed/unknown payloads cannot become source prose.
        let mut invalid_version = serde_json::to_value(payload()).unwrap();
        invalid_version["schema_version"] = json!(999);
        let mut unknown_field = serde_json::to_value(payload()).unwrap();
        unknown_field["raw_source"] = json!("synthetic-private-payload");
        for invalid in [
            json!("synthetic-private-payload"),
            invalid_version,
            unknown_field,
        ] {
            let mut nodes = base();
            let mut rule = rule_node(
                "rule:invalid",
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
            );
            rule.props["rule"] = invalid;
            rule.props["name"] = json!("synthetic-private-payload");
            nodes.push(rule);
            let bundle = compile_spec(&nodes, &[], &[], ExportMode::VerifiedOnly, &BTreeSet::new());
            let output = artifact(&bundle);
            assert!(
                output
                    .content
                    .contains("Source-rule payload unavailable: malformed or unsupported schema")
            );
            assert!(
                !serde_json::to_string(&bundle)
                    .unwrap()
                    .contains("synthetic-private-payload")
            );
            assert!(!output.content.contains("options.enabled"));
            assert_eq!(output.assertions.len(), 1);
        }
    }

    #[test]
    fn rule_inventory_preserves_withholding_and_typed_false_without_raw_fallback() {
        // AC-0124 / AC-0125: render sanitized producer output only. The extra
        // raw field is deliberately hostile; this tests the renderer boundary,
        // not the separate source sanitizer's ability to detect secrets.
        let mut nodes = base();
        let mut rule = rule_node(
            "rule:redacted",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        let mut observed = payload();
        observed.conditions[0].expression.display = SanitizedDisplay::from_sanitized(
            "password === \"[withheld string]\" && enabled !== false".into(),
        );
        observed.conditions[0].expression.capture = ExpressionCapture::Redacted;
        observed.redactions.push(SourceRedaction {
            source: evidence(),
            reason: RedactionReason::CredentialValue,
        });
        rule.props["rule"] = serde_json::to_value(&observed).unwrap();
        rule.props["raw_source"] = json!("ghp_synthetic_fixture_secret");
        nodes.push(rule);
        let mut withheld = rule_node(
            "rule:withheld",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        observed.effect = LocalExit::Return {
            value: Some(SourceExpression {
                source: evidence(),
                syntax_kind: "string".into(),
                display: SanitizedDisplay::from_sanitized("[withheld string]".into()),
                capture: ExpressionCapture::Redacted,
                literal: Some(LiteralEvidence::Withheld {
                    kind: LiteralKind::String,
                    reasons: vec![RedactionReason::CredentialValue],
                }),
            }),
        };
        withheld.props["rule"] = serde_json::to_value(observed).unwrap();
        nodes.push(withheld);
        let bundle = compile_spec(&nodes, &[], &[], ExportMode::VerifiedOnly, &BTreeSet::new());
        let output = artifact(&bundle);
        assert!(output.content.contains("boolean false"));
        assert!(
            output
                .content
                .contains("Withheld String: [CredentialValue]")
        );
        assert!(output.content.contains("Redacted"));
        assert!(output.content.contains("CredentialValue"));
        assert!(
            !serde_json::to_string(&bundle)
                .unwrap()
                .contains("ghp_synthetic_fixture_secret")
        );
    }

    #[test]
    fn stored_rule_syntax_cannot_inject_inventory_markup() {
        // AC-0125: source fragments remain data in the portable Markdown view.
        let mut nodes = base();
        let mut rule = rule_node(
            "rule:markup",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        );
        let mut observed = payload();
        observed.conditions[0].expression.display = SanitizedDisplay::from_sanitized(
            "</table>\n# Forged | [claim](untrusted) `code`".into(),
        );
        rule.props["rule"] = serde_json::to_value(observed).unwrap();
        nodes.push(rule);
        let bundle = compile_spec(&nodes, &[], &[], ExportMode::VerifiedOnly, &BTreeSet::new());
        let output = &artifact(&bundle).content;
        assert!(!output.contains("</table>"));
        assert!(!output.contains("\n# Forged"));
        assert!(output.contains("&lt;/table&gt;"));
        assert!(output.contains("\\# Forged \\| \\[claim\\]"));
    }
}
