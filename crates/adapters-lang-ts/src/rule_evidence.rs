//! Guarded local exits as cited source observations, never business verdicts.

use super::{Extraction, FileCx, TsNode, callable, source_expression::sanitize_expression};
use core_graph::rules::*;
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef};
use std::collections::{BTreeMap, BTreeSet};

const MAX_DEPENDENCIES: usize = 128;
const MAX_CONDITIONS: usize = 32;
const MAX_ANCESTOR_STEPS: usize = 256;
const MAX_EXPRESSION_BYTES: usize = 8 * 1024;
const MAX_RULE_BYTES: usize = 64 * 1024;
const MAX_FILE_RULE_BYTES: usize = 1024 * 1024;
const MAX_RULES_PER_FILE: usize = 256;

fn source(cx: &FileCx<'_>, node: TsNode<'_>) -> EvidenceRef {
    EvidenceRef {
        repo: cx.id.repo.into(),
        path: cx.path.into(),
        byte_start: node.start_byte() as u64,
        byte_end: node.end_byte() as u64,
        commit_sha: cx.id.commit.into(),
    }
}

fn reason_key(reason: RuleGapReason) -> String {
    serde_json::to_value(reason)
        .expect("fixed reason serializes")
        .as_str()
        .expect("reason is a string")
        .into()
}

fn reason_description(reason: &str) -> &'static str {
    match reason {
        "execution_predicate_unknown" => {
            "The complete execution predicate has not been established."
        }
        "consumer_semantics_unknown" => {
            "The caller's interpretation of this local return or throw is unknown."
        }
        "unresolved_call" => {
            "A call prerequisite lacks a proven target or behavioral interpretation."
        }
        "unresolved_binding" => "A value prerequisite lacks a stable scope-proven declaration.",
        "preceding_exit" => {
            "An earlier exit is present; its effect on reachability has not been analyzed."
        }
        "mutation" => {
            "Mutation is present in the callable; value changes have not been interpreted."
        }
        "loop_dependency" => {
            "Loop or accumulator behavior in the callable has not been interpreted."
        }
        "switch_control" => "Switch control flow in the callable has not been interpreted.",
        "exception_control" => {
            "Exception or finally behavior in the callable has not been interpreted."
        }
        "redacted_expression" => "An expression was withheld or incompletely captured.",
        "dependency_limit" => {
            "The dependency observation limit was reached; additional prerequisites remain unanalyzed."
        }
        "analysis_limit" => {
            "A source-rule capture limit was reached; additional analysis remains incomplete."
        }
        "unsupported_rule_syntax" => {
            "Incomplete source syntax prevented supported rule analysis; remaining file analysis was omitted."
        }
        _ => "Source-rule interpretation remains incomplete.",
    }
}

struct RuleBuilder<'cx, 'tree> {
    cx: &'cx FileCx<'cx>,
    rule_id: String,
    gaps: BTreeMap<String, (String, TsNode<'tree>)>,
    dependencies: Vec<RuleDependency>,
}

impl<'cx, 'tree> RuleBuilder<'cx, 'tree> {
    fn gap(&mut self, reason: RuleGapReason, at: TsNode<'tree>) -> String {
        let key = reason_key(reason);
        self.gaps
            .entry(key.clone())
            .or_insert_with(|| (format!("gap:{}:{key}", self.rule_id), at))
            .0
            .clone()
    }

    fn control_gap(&mut self, reason: RuleGapReason, at: TsNode<'tree>) {
        let gap_id = self.gap(reason, at);
        if !self.dependencies.iter().any(|dependency| {
            dependency.resolution
                == DependencyResolution::Unresolved {
                    gap_id: gap_id.clone(),
                }
        }) {
            self.dependencies.push(RuleDependency {
                role: DependencyRole::ControlFlow,
                source: source(self.cx, at),
                resolution: DependencyResolution::Unresolved { gap_id },
            });
        }
    }

    fn dependencies(
        &mut self,
        expression: TsNode<'tree>,
        role: DependencyRole,
        bindings: &callable::Bindings<'tree>,
        targets: &BTreeSet<String>,
    ) {
        let Some(owner) = callable::enclosing(self.cx, expression) else {
            self.control_gap(RuleGapReason::ExecutionPredicateUnknown, expression);
            return;
        };
        let mut stack = vec![expression];
        while let Some(node) = stack.pop() {
            if self.dependencies.len() >= MAX_DEPENDENCIES {
                self.control_gap(RuleGapReason::DependencyLimit, expression);
                return;
            }
            // Static initialization can execute during class creation, but its
            // separate execution scope is not modeled. Preserve the actual
            // omitted location even when the generic interpretation Gap exists.
            let static_initializer = node.parent().is_some_and(|parent| {
                if parent.kind() != "public_field_definition"
                    || parent.child_by_field_name("value") != Some(node)
                {
                    return false;
                }
                let mut cursor = parent.walk();
                parent
                    .children(&mut cursor)
                    .any(|child| child.kind() == "static")
            });
            if node.kind() == "class_static_block" || static_initializer {
                let gap_id = self.gap(RuleGapReason::ExecutionPredicateUnknown, node);
                self.dependencies.push(RuleDependency {
                    role,
                    source: source(self.cx, node),
                    resolution: DependencyResolution::Unresolved { gap_id },
                });
                continue;
            }
            if callable::enclosing(self.cx, node).as_deref() != Some(owner.as_str()) {
                // In particular, instance field initializers are deferred
                // until construction, not evaluation of the class expression.
                self.control_gap(RuleGapReason::ExecutionPredicateUnknown, node);
                continue;
            }
            // Callable bodies and parameter defaults are deferred execution.
            // A method's computed name and decorators instead execute while
            // the object/class is created and remain expression prerequisites.
            if callable::is_callable(node) {
                self.control_gap(RuleGapReason::ExecutionPredicateUnknown, node);
                if node.kind() == "method_definition" {
                    let mut creation = Vec::new();
                    if let Some(key) = node
                        .child_by_field_name("name")
                        .filter(|key| key.kind() == "computed_property_name")
                    {
                        creation.push(key);
                    }
                    let mut cursor = node.walk();
                    creation.extend(
                        node.named_children(&mut cursor)
                            .filter(|child| child.kind() == "decorator"),
                    );
                    if let Some(parameters) = node.child_by_field_name("parameters") {
                        let mut cursor = parameters.walk();
                        for parameter in parameters.named_children(&mut cursor) {
                            let mut cursor = parameter.walk();
                            creation.extend(
                                parameter
                                    .named_children(&mut cursor)
                                    .filter(|child| child.kind() == "decorator"),
                            );
                        }
                    }
                    creation.sort_by_key(|node| node.start_byte());
                    stack.extend(creation.into_iter().rev());
                }
                continue;
            }
            let resolution = if matches!(node.kind(), "call_expression" | "new_expression") {
                node.child_by_field_name("function")
                    .filter(|callee| callee.kind() == "identifier")
                    .and_then(|callee| bindings.local_target(callee, self.cx.text(&callee)))
                    .filter(|target| targets.contains(target))
                    .map(|node_id| DependencyResolution::Target { node_id })
                    .or_else(|| {
                        Some(DependencyResolution::Unresolved {
                            gap_id: self.gap(RuleGapReason::UnresolvedCall, node),
                        })
                    })
            } else if is_value_identifier(node) {
                Some(
                    bindings
                        .stable_declaration(node, self.cx.text(&node))
                        .map(|declaration| DependencyResolution::Binding {
                            binding_id: format!(
                                "binding:{}@{}#declaration@{}",
                                self.cx.id.repo,
                                self.cx.path,
                                declaration.start_byte()
                            ),
                            declaration: source(self.cx, declaration),
                        })
                        .unwrap_or_else(|| DependencyResolution::Unresolved {
                            gap_id: self.gap(RuleGapReason::UnresolvedBinding, node),
                        }),
                )
            } else {
                None
            };
            if let Some(resolution) = resolution {
                self.dependencies.push(RuleDependency {
                    role,
                    source: source(self.cx, node),
                    resolution,
                });
            }
            let mut cursor = node.walk();
            stack.extend(
                node.named_children(&mut cursor)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev(),
            );
        }
    }
}

fn is_value_identifier(node: TsNode<'_>) -> bool {
    if !matches!(node.kind(), "identifier" | "shorthand_property_identifier") {
        return false;
    }
    !node.parent().is_some_and(|parent| {
        (parent.kind() == "pair" && parent.child_by_field_name("key") == Some(node))
            || (parent.kind() == "member_expression"
                && parent.child_by_field_name("property") == Some(node))
    })
}

fn branches(mut exit: TsNode<'_>) -> (Vec<(TsNode<'_>, TsNode<'_>, BranchPolarity)>, bool, bool) {
    let mut conditions = Vec::new();
    let mut steps = 0;
    let mut limited = false;
    let mut recovered = exit.has_error() || exit.is_missing() || exit.is_error();
    while let Some(parent) = exit.parent() {
        if callable::is_callable(parent) {
            break;
        }
        steps += 1;
        if steps > MAX_ANCESTOR_STEPS || conditions.len() == MAX_CONDITIONS {
            limited = true;
            break;
        }
        // A valid-looking child cannot establish an exit through a recovered
        // wrapper or controlling branch. Unsupported sanitized expressions
        // remain useful only when the parser established the original syntax.
        recovered |= parent.is_error()
            || parent.is_missing()
            || (parent.kind() == "if_statement" && parent.has_error());
        if parent.kind() == "if_statement"
            && let Some(condition) = parent.child_by_field_name("condition")
        {
            let polarity = if parent.child_by_field_name("consequence") == Some(exit) {
                Some(BranchPolarity::TruthyBranch)
            } else if parent.child_by_field_name("alternative") == Some(exit) {
                Some(BranchPolarity::FalsyBranch)
            } else {
                None
            };
            if let Some(polarity) = polarity {
                conditions.push((parent, condition, polarity));
            }
        }
        exit = parent;
    }
    conditions.reverse();
    (conditions, limited, recovered)
}

fn exit_value(exit: TsNode<'_>) -> Option<TsNode<'_>> {
    let mut cursor = exit.walk();
    exit.named_children(&mut cursor)
        .find(|node| node.kind() != "comment")
}

fn omission_source(mut node: TsNode<'_>) -> TsNode<'_> {
    // Missing parser nodes can be zero-width. Cite the enclosing source that
    // failed to parse instead of manufacturing an expression or empty span.
    for _ in 0..MAX_ANCESTOR_STEPS {
        if !node.byte_range().is_empty() {
            break;
        }
        let Some(parent) = node.parent() else {
            break;
        };
        node = parent;
    }
    node
}

fn omission_gap(
    cx: &FileCx<'_>,
    at: TsNode<'_>,
    owner: &str,
    reason: RuleGapReason,
    out: &mut Extraction,
) {
    let id = format!(
        "gap:{}@{}#rule-analysis@{}",
        cx.id.repo,
        cx.path,
        at.start_byte()
    );
    let reason = reason_key(reason);
    let props = serde_json::json!({
        "rule_evidence_gap": true,
        "reason_code": reason,
        "reason": reason_description(&reason),
        "scope": "rule_or_remaining_file_analysis",
        "prov": cx.prov_with_confidence(&at, ConfidenceTier::Gap, &format!("Gap {id}")),
    });
    out.nodes.push(Node {
        id: id.clone(),
        label: "Gap".into(),
        props: props.clone(),
    });
    out.edges.push(Edge {
        src: owner.into(),
        dst: id,
        label: "DEPENDS_ON".into(),
        props,
    });
}

fn capture(cx: &FileCx<'_>, node: TsNode<'_>) -> (SourceExpression, Vec<SourceRedaction>) {
    if node.byte_range().len() <= MAX_EXPRESSION_BYTES {
        return sanitize_expression(cx, node);
    }
    let source = source(cx, node);
    (
        SourceExpression {
            source: source.clone(),
            syntax_kind: node.kind().into(),
            display: SanitizedDisplay::from_sanitized(
                "[expression omitted: analysis limit]".into(),
            ),
            capture: ExpressionCapture::Unsupported,
            literal: None,
        },
        vec![SourceRedaction {
            source,
            reason: RedactionReason::UnsupportedSyntax,
        }],
    )
}

fn control_reason(kind: &str) -> Option<RuleGapReason> {
    match kind {
        "for_statement" | "for_in_statement" | "while_statement" | "do_statement" => {
            Some(RuleGapReason::LoopDependency)
        }
        "switch_statement" => Some(RuleGapReason::SwitchControl),
        "try_statement" | "catch_clause" | "finally_clause" => {
            Some(RuleGapReason::ExceptionControl)
        }
        "assignment_expression" | "augmented_assignment_expression" | "update_expression" => {
            Some(RuleGapReason::Mutation)
        }
        _ => None,
    }
}

pub(super) fn extract<'tree>(
    cx: &FileCx<'_>,
    nodes: &[TsNode<'tree>],
    bindings: &callable::Bindings<'tree>,
    out: &mut Extraction,
) {
    let targets: BTreeSet<_> = out.nodes.iter().map(|node| node.id.clone()).collect();
    // Collect once by callable, rather than repeatedly scanning the whole file.
    // Recovery may erase an entire exit or callable. Keep that incomplete
    // syntax as an omission candidate, anchored to the existing File when no
    // real callable can be established; do not infer an exit from raw text.
    let mut exits: BTreeMap<String, Vec<TsNode<'tree>>> = BTreeMap::new();
    let mut controls: BTreeMap<String, BTreeMap<String, (RuleGapReason, TsNode<'tree>)>> =
        BTreeMap::new();
    for node in nodes.iter().copied() {
        if node.is_error()
            || node.is_missing()
            || (node.kind() == "if_statement" && node.has_error())
        {
            let owner = callable::enclosing(cx, node)
                .filter(|owner| targets.contains(owner))
                .unwrap_or_else(|| super::file_id(cx.id.repo, cx.path));
            exits.entry(owner).or_default().push(node);
        } else if matches!(node.kind(), "return_statement" | "throw_statement") {
            if let Some(owner) = callable::enclosing(cx, node) {
                exits.entry(owner).or_default().push(node);
            }
        } else if let Some(reason) = control_reason(node.kind())
            && let Some(owner) = callable::enclosing(cx, node)
        {
            controls
                .entry(owner)
                .or_default()
                .entry(reason_key(reason))
                .or_insert((reason, node));
        }
    }
    let mut rule_count = 0;
    let mut file_bytes = 0;
    for (owner_id, owner_exits) in exits {
        if !targets.contains(&owner_id) {
            continue;
        }
        for (order, exit) in owner_exits.iter().copied().enumerate() {
            let (ancestors, limited, recovered) = branches(exit);
            if recovered || (exit.kind() == "throw_statement" && exit_value(exit).is_none()) {
                omission_gap(
                    cx,
                    omission_source(exit),
                    &owner_id,
                    RuleGapReason::UnsupportedRuleSyntax,
                    out,
                );
                return;
            }
            if ancestors.is_empty() && !limited {
                continue;
            }
            if rule_count == MAX_RULES_PER_FILE || file_bytes >= MAX_FILE_RULE_BYTES {
                omission_gap(cx, exit, &owner_id, RuleGapReason::AnalysisLimit, out);
                return;
            }
            if limited {
                omission_gap(cx, exit, &owner_id, RuleGapReason::AnalysisLimit, out);
                return;
            }
            let rule_id = format!("rule:{}@{}#exit@{}", cx.id.repo, cx.path, exit.start_byte());
            let mut builder = RuleBuilder {
                cx,
                rule_id: rule_id.clone(),
                gaps: BTreeMap::new(),
                dependencies: Vec::new(),
            };
            builder.control_gap(RuleGapReason::ExecutionPredicateUnknown, exit);
            builder.control_gap(RuleGapReason::ConsumerSemanticsUnknown, exit);
            if let Some(prior) = order.checked_sub(1).map(|index| owner_exits[index]) {
                builder.control_gap(RuleGapReason::PrecedingExit, prior);
            }
            if let Some(controls) = controls.get(&owner_id) {
                for (reason, at) in controls.values() {
                    builder.control_gap(*reason, *at);
                }
            }
            let mut redactions = Vec::new();
            let mut conditions = Vec::new();
            for (branch, condition, polarity) in ancestors {
                let (expression, mut withheld) = capture(cx, condition);
                if expression.capture != ExpressionCapture::CompleteSyntax {
                    builder.control_gap(RuleGapReason::RedactedExpression, condition);
                }
                redactions.append(&mut withheld);
                if condition.byte_range().len() <= MAX_EXPRESSION_BYTES {
                    builder.dependencies(condition, DependencyRole::Condition, bindings, &targets);
                } else {
                    builder.control_gap(RuleGapReason::AnalysisLimit, condition);
                }
                conditions.push(BranchCondition {
                    branch_source: source(cx, branch),
                    expression,
                    polarity,
                });
            }
            let value = exit_value(exit).map(|node| {
                let (expression, mut withheld) = capture(cx, node);
                if expression.capture != ExpressionCapture::CompleteSyntax {
                    builder.control_gap(RuleGapReason::RedactedExpression, node);
                }
                redactions.append(&mut withheld);
                if node.byte_range().len() <= MAX_EXPRESSION_BYTES {
                    builder.dependencies(node, DependencyRole::ExitValue, bindings, &targets);
                } else {
                    builder.control_gap(RuleGapReason::AnalysisLimit, node);
                }
                expression
            });
            let effect = if exit.kind() == "throw_statement" {
                let Some(value) = value else {
                    omission_gap(
                        cx,
                        exit,
                        &owner_id,
                        RuleGapReason::UnsupportedRuleSyntax,
                        out,
                    );
                    return;
                };
                LocalExit::Throw { value }
            } else {
                LocalExit::Return { value }
            };
            let evidence = GuardedExitEvidence {
                schema_version: RULE_EVIDENCE_SCHEMA_VERSION,
                kind: RuleKind::GuardedExit,
                owner_id: owner_id.clone(),
                exit_source: source(cx, exit),
                source_order: order as u32,
                conditions,
                effect,
                dependencies: builder.dependencies,
                interpretation: Interpretation {
                    execution_predicate: InterpretationStatus::NotEstablished,
                    consumer_effect: InterpretationStatus::NotEstablished,
                    gap_ids: builder.gaps.values().map(|(id, _)| id.clone()).collect(),
                },
                redactions,
            };
            if evidence.validate().is_err() {
                omission_gap(
                    cx,
                    exit,
                    &owner_id,
                    RuleGapReason::UnsupportedRuleSyntax,
                    out,
                );
                return;
            }
            let payload = serde_json::to_value(evidence).expect("rule serializes");
            let canonical = serde_json::to_string(&payload).expect("rule serializes");
            if canonical.len() > MAX_RULE_BYTES
                || file_bytes + canonical.len() > MAX_FILE_RULE_BYTES
            {
                omission_gap(cx, exit, &owner_id, RuleGapReason::AnalysisLimit, out);
                return;
            }
            file_bytes += canonical.len();
            rule_count += 1;
            out.nodes.push(Node {
                id: rule_id.clone(),
                label: "BusinessRule".into(),
                props: serde_json::json!({
                    "name": format!("Guarded local exit at {}:{}", cx.path, exit.start_position().row + 1),
                    "observation": "guarded_exit",
                    "rule": payload,
                    "prov": cx.prov(&exit, &canonical),
                }),
            });
            out.edges.push(Edge {
                src: rule_id.clone(),
                dst: owner_id.clone(),
                label: "GOVERNS".into(),
                props: serde_json::json!({"observation": "lexical_owner", "prov": cx.prov(&exit, &format!("GOVERNS {rule_id} -> {owner_id}"))}),
            });
            for (reason, (gap_id, at)) in builder.gaps {
                let props = serde_json::json!({
                    "rule_evidence_gap": true,
                    "reason_code": reason,
                    "reason": reason_description(&reason),
                    "prov": cx.prov_with_confidence(&at, ConfidenceTier::Gap, &format!("Gap {gap_id}")),
                });
                out.nodes.push(Node {
                    id: gap_id.clone(),
                    label: "Gap".into(),
                    props: props.clone(),
                });
                out.edges.push(Edge {
                    src: rule_id.clone(),
                    dst: gap_id,
                    label: "DEPENDS_ON".into(),
                    props,
                });
            }
        }
    }
}

/// Cached unchanged source still belongs to the revision being ingested now.
pub(super) fn retarget_commit(props: &mut serde_json::Value, commit: &str) {
    let Some(payload) = props.get("rule").cloned() else {
        return;
    };
    let Ok(mut rule) = GuardedExitEvidence::from_value(payload) else {
        return;
    };
    rule.visit_sources_mut(|source| source.commit_sha = commit.into());
    props["rule"] = serde_json::to_value(rule).expect("rule serializes");
    props["prov"]["content_hash"] = serde_json::json!(core_prov::content_hash(
        &serde_json::to_vec(&props["rule"]).expect("rule serializes")
    ));
}

#[cfg(test)]
mod tests;
