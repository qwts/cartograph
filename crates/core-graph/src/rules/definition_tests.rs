use super::*;

const CODE: &str =
    "function run(input:any){ const ready = input.enabled === false; if (ready) return false; }";

fn source(start: usize, end: usize) -> EvidenceRef {
    EvidenceRef {
        repo: "acme/example".into(),
        path: "logic.ts".into(),
        byte_start: start as u64,
        byte_end: end as u64,
        commit_sha: "original".into(),
    }
}

fn location(text: &str) -> EvidenceRef {
    let start = CODE.find(text).unwrap();
    source(start, start + text.len())
}

fn expression(source: EvidenceRef, kind: &str, display: &str) -> SourceExpression {
    SourceExpression {
        source,
        syntax_kind: kind.into(),
        display: SanitizedDisplay::from_sanitized(display.into()),
        capture: ExpressionCapture::CompleteSyntax,
        literal: None,
    }
}

fn fixture() -> GuardedExitEvidence {
    let declaration = location("ready = input.enabled === false");
    let binding_id = "binding:acme/example@logic.ts#declaration@30".to_string();
    let start = CODE.rfind("ready").unwrap();
    let usage = source(start, start + 5);
    let initializer = expression(
        location("input.enabled === false"),
        "binary_expression",
        "input.enabled === false",
    );
    let member = expression(
        location("input.enabled"),
        "member_expression",
        "input.enabled",
    );
    let object = expression(
        source(
            member.source.byte_start as usize,
            member.source.byte_start as usize + 5,
        ),
        "identifier",
        "input",
    );
    let property = expression(location("enabled"), "property_identifier", "enabled");
    let mut literal = expression(location("false"), "false", "false");
    literal.literal = Some(LiteralEvidence::Known {
        value: KnownLiteral::Boolean(false),
    });
    GuardedExitEvidence {
        schema_version: 2,
        kind: RuleKind::GuardedExit,
        owner_id: "sym:acme/example@logic.ts#run".into(),
        exit_source: location("return false;"),
        source_order: 0,
        conditions: vec![BranchCondition {
            branch_source: location("if (ready) return false;"),
            expression: expression(usage.clone(), "identifier", "ready"),
            polarity: BranchPolarity::TruthyBranch,
        }],
        effect: LocalExit::Return { value: None },
        dependencies: vec![RuleDependency {
            role: DependencyRole::Condition,
            source: usage.clone(),
            resolution: DependencyResolution::Binding {
                binding_id: binding_id.clone(),
                declaration: declaration.clone(),
            },
        }],
        interpretation: Interpretation {
            execution_predicate: InterpretationStatus::NotEstablished,
            consumer_effect: InterpretationStatus::NotEstablished,
            gap_ids: vec!["gap:runtime".into()],
        },
        redactions: vec![],
        local_definitions: Some(vec![LocalDefinition {
            binding_id,
            declaration,
            uses: vec![usage],
            initializer: initializer.clone(),
            expression: DefinitionExpression {
                root: 0,
                nodes: vec![
                    DefinitionExpressionNode {
                        expression: initializer,
                        kind: DefinitionExpressionKind::Binary {
                            operator: BinaryOperator::StrictEqual,
                            left: 1,
                            right: 4,
                        },
                    },
                    DefinitionExpressionNode {
                        expression: member.clone(),
                        kind: DefinitionExpressionKind::Member {
                            object: 2,
                            property: 3,
                            computed: false,
                            optional: false,
                            dependency: 0,
                        },
                    },
                    DefinitionExpressionNode {
                        expression: object.clone(),
                        kind: DefinitionExpressionKind::Identifier { dependency: 1 },
                    },
                    DefinitionExpressionNode {
                        expression: property,
                        kind: DefinitionExpressionKind::PropertyName,
                    },
                    DefinitionExpressionNode {
                        expression: literal,
                        kind: DefinitionExpressionKind::Literal,
                    },
                ],
            },
            dependencies: vec![
                DefinitionDependency {
                    source: member.source,
                    resolution: DependencyResolution::Unresolved {
                        gap_id: "gap:runtime".into(),
                    },
                },
                DefinitionDependency {
                    source: object.source,
                    resolution: DependencyResolution::Binding {
                        binding_id: "binding:input".into(),
                        declaration: location("input:any"),
                    },
                },
            ],
        }]),
    }
}

#[test]
fn local_definition_versions_preserve_v1_and_reject_null_or_unknown_contracts() {
    // AC-0164: v1 reads do not silently acquire or normalize new evidence.
    let mut old = fixture();
    old.schema_version = 1;
    old.local_definitions = None;
    let old_json = serde_json::to_string(&old).unwrap();
    assert!(!old_json.contains("local_definitions"));
    let decoded =
        GuardedExitEvidence::from_value(serde_json::from_str(&old_json).unwrap()).unwrap();
    assert_eq!(serde_json::to_string(&decoded).unwrap(), old_json);
    let current = fixture();
    assert_eq!(
        GuardedExitEvidence::from_value(serde_json::to_value(&current).unwrap()).unwrap(),
        current
    );
    for version in [1, 2] {
        let mut value = serde_json::to_value(&current).unwrap();
        value["schema_version"] = version.into();
        value["local_definitions"] = serde_json::Value::Null;
        assert!(GuardedExitEvidence::from_value(value.clone()).is_err());
        assert!(serde_json::from_value::<GuardedExitEvidence>(value).is_err());
    }
    for version in [
        serde_json::Value::Null,
        "private-invalid-version".into(),
        0.into(),
        3.into(),
    ] {
        let mut value = serde_json::to_value(&current).unwrap();
        value["schema_version"] = version;
        let error = GuardedExitEvidence::from_value(value).unwrap_err();
        assert!(!error.to_string().contains("private-invalid-version"));
    }
    let mut value = serde_json::to_value(&current).unwrap();
    value.as_object_mut().unwrap().remove("schema_version");
    assert!(GuardedExitEvidence::from_value(value).is_err());
    let mut value = serde_json::to_value(&current).unwrap();
    value.as_object_mut().unwrap().remove("local_definitions");
    assert!(GuardedExitEvidence::from_value(value).is_err());
}

#[test]
fn local_definition_arenas_reject_incoherent_structure_and_links() {
    // AC-0166 / AC-0167: typed syntax cannot hide malformed graph references.
    type Change = fn(&mut GuardedExitEvidence);
    let changes: &[Change] = &[
        |rule| rule.local_definitions.as_mut().unwrap()[0].expression.root = 100,
        |rule| {
            let d = &mut rule.local_definitions.as_mut().unwrap()[0];
            d.expression.nodes.push(d.expression.nodes[4].clone());
        },
        |rule| {
            rule.local_definitions.as_mut().unwrap()[0].expression.nodes[0].kind =
                DefinitionExpressionKind::Binary {
                    operator: BinaryOperator::StrictEqual,
                    left: 0,
                    right: 4,
                }
        },
        |rule| {
            rule.local_definitions.as_mut().unwrap()[0].expression.nodes[0].kind =
                DefinitionExpressionKind::Binary {
                    operator: BinaryOperator::StrictEqual,
                    left: 4,
                    right: 1,
                }
        },
        |rule| {
            rule.local_definitions.as_mut().unwrap()[0].expression.nodes[2].kind =
                DefinitionExpressionKind::Identifier { dependency: 99 }
        },
        |rule| {
            let d = &mut rule.local_definitions.as_mut().unwrap()[0];
            d.dependencies.push(d.dependencies[1].clone());
        },
        |rule| {
            rule.local_definitions.as_mut().unwrap()[0].dependencies[0].resolution =
                DependencyResolution::Target {
                    node_id: "invented-value".into(),
                }
        },
        |rule| {
            rule.local_definitions.as_mut().unwrap()[0].dependencies[0]
                .source
                .byte_start += 1
        },
        |rule| {
            rule.local_definitions.as_mut().unwrap()[0].expression.nodes[4]
                .expression
                .source
                .path = "other.ts".into()
        },
        |rule| {
            rule.local_definitions.as_mut().unwrap()[0].expression.nodes[4]
                .expression
                .source
                .byte_end += 100
        },
        |rule| {
            rule.local_definitions.as_mut().unwrap()[0]
                .initializer
                .display = SanitizedDisplay::from_sanitized("changed root".into())
        },
        |rule| rule.local_definitions.as_mut().unwrap()[0].uses[0].byte_start += 1,
        |rule| {
            let d = &mut rule.local_definitions.as_mut().unwrap()[0];
            d.uses.push(d.uses[0].clone());
        },
        |rule| {
            let d = rule.local_definitions.as_ref().unwrap()[0].clone();
            rule.local_definitions.as_mut().unwrap().push(d);
        },
        |rule| rule.interpretation.gap_ids.clear(),
        |rule| {
            rule.local_definitions.as_mut().unwrap()[0].expression.nodes[4]
                .expression
                .syntax_kind = "call_expression".into()
        },
    ];
    fixture().validate().unwrap();
    for (index, change) in changes.iter().enumerate() {
        let mut invalid = fixture();
        change(&mut invalid);
        assert!(invalid.validate().is_err(), "case {index}");
        assert!(
            GuardedExitEvidence::from_value(serde_json::to_value(invalid).unwrap()).is_err(),
            "wire case {index}"
        );
    }
    let mut value = serde_json::to_value(fixture()).unwrap();
    value["local_definitions"][0]["expression"]["nodes"][0]["kind"]["operator"] =
        "unknown-private-operator".into();
    let error = GuardedExitEvidence::from_value(value).unwrap_err();
    assert!(!error.to_string().contains("unknown-private-operator"));
}

#[test]
fn local_definition_wire_bounds_precede_nested_deserialization() {
    // AC-0164 / AC-0167: oversize data is rejected before typed collection copies.
    let mut value = serde_json::to_value(fixture()).unwrap();
    let definition = value["local_definitions"][0].clone();
    value["local_definitions"] = vec![definition; MAX_LOCAL_DEFINITIONS + 1].into();
    assert!(matches!(
        GuardedExitEvidence::from_value(value),
        Err(RuleValidationError::Limit(_))
    ));
    let mut value = serde_json::to_value(fixture()).unwrap();
    let node = value["local_definitions"][0]["expression"]["nodes"][0].clone();
    value["local_definitions"][0]["expression"]["nodes"] =
        vec![node; MAX_DEFINITION_NODES + 1].into();
    assert!(matches!(
        GuardedExitEvidence::from_value(value),
        Err(RuleValidationError::Limit(_))
    ));
    let mut value = serde_json::to_value(fixture()).unwrap();
    value["local_definitions"][0]["initializer"]["display"] = "x".repeat(8193).into();
    assert!(matches!(
        GuardedExitEvidence::from_value(value),
        Err(RuleValidationError::Limit(_))
    ));
    let mut value = serde_json::to_value(fixture()).unwrap();
    let mut deep = serde_json::Value::Null;
    for _ in 0..40 {
        deep = serde_json::json!([deep]);
    }
    value["untrusted"] = deep;
    assert!(matches!(
        GuardedExitEvidence::from_value(value),
        Err(RuleValidationError::Limit(_))
    ));
    let mut value = serde_json::to_value(fixture()).unwrap();
    value["local_definitions"][0]["declaration"]["raw"] = "not-an-evidence-field".into();
    assert!(GuardedExitEvidence::from_value(value).is_err());
}

#[test]
fn local_definition_source_visitor_keeps_every_original_occurrence() {
    // AC-0168: duplicate initializer/root citations remain distinct occurrences.
    let mut rule = fixture();
    let mut expected = vec![
        rule.exit_source.clone(),
        rule.conditions[0].branch_source.clone(),
        rule.conditions[0].expression.source.clone(),
        rule.dependencies[0].source.clone(),
    ];
    if let DependencyResolution::Binding { declaration, .. } = &rule.dependencies[0].resolution {
        expected.push(declaration.clone());
    }
    let definition = &rule.local_definitions.as_ref().unwrap()[0];
    expected.push(definition.declaration.clone());
    expected.extend(definition.uses.iter().cloned());
    expected.push(definition.initializer.source.clone());
    expected.extend(
        definition
            .expression
            .nodes
            .iter()
            .map(|node| node.expression.source.clone()),
    );
    for dependency in &definition.dependencies {
        expected.push(dependency.source.clone());
        if let DependencyResolution::Binding { declaration, .. } = &dependency.resolution {
            expected.push(declaration.clone());
        }
    }
    let initializer = definition.initializer.source.clone();
    assert_eq!(
        expected
            .iter()
            .filter(|reference| **reference == initializer)
            .count(),
        2
    );
    let mut actual = Vec::new();
    rule.visit_sources_mut(|reference| {
        actual.push(reference.clone());
        reference.commit_sha = "retargeted".into();
    });
    assert_eq!(actual, expected);
    rule.validate().unwrap();
    let mut checked = 0;
    rule.visit_sources_mut(|reference| {
        assert_eq!(reference.commit_sha, "retargeted");
        checked += 1;
    });
    assert_eq!(checked, expected.len());
}

#[test]
fn local_definition_operator_enums_preserve_distinct_source_forms() {
    // AC-0166: ordinary JS operators stay closed, distinct and unevaluated.
    let operators = [
        BinaryOperator::LooseEqual,
        BinaryOperator::LooseNotEqual,
        BinaryOperator::StrictEqual,
        BinaryOperator::StrictNotEqual,
        BinaryOperator::LessThan,
        BinaryOperator::LessThanOrEqual,
        BinaryOperator::GreaterThan,
        BinaryOperator::GreaterThanOrEqual,
        BinaryOperator::Add,
        BinaryOperator::Subtract,
        BinaryOperator::Multiply,
        BinaryOperator::Divide,
        BinaryOperator::Remainder,
        BinaryOperator::Exponent,
        BinaryOperator::LeftShift,
        BinaryOperator::RightShift,
        BinaryOperator::UnsignedRightShift,
        BinaryOperator::BitwiseAnd,
        BinaryOperator::BitwiseOr,
        BinaryOperator::BitwiseXor,
        BinaryOperator::In,
        BinaryOperator::Instanceof,
    ];
    let mut seen = std::collections::BTreeSet::new();
    for operator in operators {
        let wire = serde_json::to_string(&operator).unwrap();
        assert!(seen.insert(wire.clone()));
        assert_eq!(
            serde_json::from_str::<BinaryOperator>(&wire).unwrap(),
            operator
        );
    }
    for operator in [
        UnaryOperator::Not,
        UnaryOperator::Plus,
        UnaryOperator::Minus,
        UnaryOperator::BitwiseNot,
        UnaryOperator::Typeof,
        UnaryOperator::Void,
    ] {
        assert_eq!(
            serde_json::from_value::<UnaryOperator>(serde_json::to_value(operator).unwrap())
                .unwrap(),
            operator
        );
    }
    for operator in [
        LogicalOperator::And,
        LogicalOperator::Or,
        LogicalOperator::Nullish,
    ] {
        assert_eq!(
            serde_json::from_value::<LogicalOperator>(serde_json::to_value(operator).unwrap())
                .unwrap(),
            operator
        );
    }
}

#[test]
fn local_definition_depth_payload_and_admitted_use_boundaries_hold() {
    // AC-0164 / AC-0167: the exact expression-depth boundary is accepted, while
    // the next level and the entire oversized envelope fail explicitly.
    fn nested(depth: usize) -> GuardedExitEvidence {
        let mut rule = fixture();
        let definition = &mut rule.local_definitions.as_mut().unwrap()[0];
        let base = definition.initializer.source.byte_start;
        let width = (depth * 2 + 5) as u64;
        definition.declaration = source(0, (base + width + 1) as usize);
        let usage = source((base + width + 5) as usize, (base + width + 10) as usize);
        definition.uses = vec![usage.clone()];
        definition.dependencies.clear();
        definition.expression.nodes.clear();
        for level in 0..depth {
            let start = base + level as u64;
            let end = base + width - level as u64;
            let is_literal = level + 1 == depth;
            let mut expression = expression(
                source(start as usize, end as usize),
                if is_literal {
                    "false"
                } else {
                    "parenthesized_expression"
                },
                if is_literal { "false" } else { "(false)" },
            );
            if is_literal {
                expression.literal = Some(LiteralEvidence::Known {
                    value: KnownLiteral::Boolean(false),
                });
            }
            definition.expression.nodes.push(DefinitionExpressionNode {
                expression,
                kind: if is_literal {
                    DefinitionExpressionKind::Literal
                } else {
                    DefinitionExpressionKind::Parenthesized {
                        value: level as u32 + 1,
                    }
                },
            });
        }
        definition.initializer = definition.expression.nodes[0].expression.clone();
        rule.dependencies[0].source = usage.clone();
        rule.dependencies[0].resolution = DependencyResolution::Binding {
            binding_id: definition.binding_id.clone(),
            declaration: definition.declaration.clone(),
        };
        rule.conditions[0].expression.source = usage.clone();
        rule.conditions[0].branch_source =
            source(usage.byte_start as usize - 1, usage.byte_end as usize + 1);
        rule
    }
    nested(MAX_DEFINITION_EXPRESSION_DEPTH).validate().unwrap();
    assert!(matches!(
        nested(MAX_DEFINITION_EXPRESSION_DEPTH + 1).validate(),
        Err(RuleValidationError::Limit("expression_depth"))
    ));
    let mut large = fixture();
    large.redactions = vec![
        SourceRedaction {
            source: large.exit_source.clone(),
            reason: RedactionReason::RemovedComment
        };
        600
    ];
    assert!(serde_json::to_vec(&large).unwrap().len() > MAX_RULE_PAYLOAD_BYTES);
    assert!(matches!(
        large.validate(),
        Err(RuleValidationError::Limit("payload_bytes"))
    ));
    assert!(matches!(
        GuardedExitEvidence::from_value(serde_json::to_value(large).unwrap()),
        Err(RuleValidationError::Limit(_))
    ));

    // A raw self binding is preserved as an observation, but does not form a
    // recovered definition cycle when that initializer use was not admitted.
    let mut self_reference = fixture();
    let definition = &mut self_reference.local_definitions.as_mut().unwrap()[0];
    let expression = expression(definition.initializer.source.clone(), "identifier", "ready");
    definition.initializer = expression.clone();
    definition.expression.nodes = vec![DefinitionExpressionNode {
        expression: expression.clone(),
        kind: DefinitionExpressionKind::Identifier { dependency: 0 },
    }];
    definition.dependencies = vec![DefinitionDependency {
        source: expression.source,
        resolution: DependencyResolution::Binding {
            binding_id: definition.binding_id.clone(),
            declaration: definition.declaration.clone(),
        },
    }];
    self_reference
        .interpretation
        .gap_ids
        .push("gap:definition-order".into());
    self_reference.validate().unwrap();
}
