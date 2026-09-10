use super::*;
use crate::{Extraction, IncrementalCache, SourceId, extract_dir_incremental, extract_source};

fn extract(code: &str) -> (Extraction, Vec<GuardedExitEvidence>) {
    let out = extract_source(
        code.as_bytes(),
        "rules.ts",
        &SourceId {
            repo: "fixture",
            commit: "a",
        },
    )
    .unwrap();
    let rules = out
        .nodes
        .iter()
        .filter(|node| node.label == "BusinessRule")
        .map(|node| GuardedExitEvidence::from_value(node.props["rule"].clone()).unwrap())
        .collect();
    (out, rules)
}

fn definitions(rule: &GuardedExitEvidence) -> &[LocalDefinition] {
    assert_eq!(rule.schema_version, 2);
    rule.local_definitions.as_deref().unwrap()
}

fn original<'a>(code: &'a str, source: &core_prov::EvidenceRef) -> &'a str {
    &code[source.byte_start as usize..source.byte_end as usize]
}

#[test]
fn local_const_definitions_recover_original_predicate_structure_and_reads() {
    // AC-0165/AC-0166: retain the condition, recover direct and transitive
    // initializers at their original sites, and keep runtime dependencies open.
    let code = r#"function decide(variant: any, trackInventory: boolean) {
        const inherited = variant.trackInventory === GlobalFlag.INHERIT && trackInventory === false;
        const inventoryNotTracked = variant.trackInventory === GlobalFlag.FALSE || inherited;
        if (inventoryNotTracked) return Number.MAX_SAFE_INTEGER;
    }"#;
    let (out, rules) = extract(code);
    assert_eq!(rules.len(), 1);
    let rule = &rules[0];
    assert_eq!(
        rule.conditions[0].expression.display.as_str(),
        "(inventoryNotTracked)"
    );
    let defs = definitions(rule);
    assert_eq!(defs.len(), 2);
    assert!(original(code, &defs[0].declaration).starts_with("inventoryNotTracked ="));
    assert!(original(code, &defs[1].declaration).starts_with("inherited ="));
    assert_eq!(original(code, &defs[0].uses[0]), "inventoryNotTracked");
    assert_eq!(original(code, &defs[1].uses[0]), "inherited");
    for definition in defs {
        assert_eq!(
            definition.initializer,
            definition.expression.nodes[definition.expression.root as usize].expression
        );
        assert_eq!(
            original(code, &definition.initializer.source),
            definition.initializer.display.as_str()
        );
        for node in &definition.expression.nodes {
            assert_eq!(
                original(code, &node.expression.source),
                node.expression.display.as_str()
            );
            if let DefinitionExpressionKind::Member { dependency, .. } = node.kind {
                assert!(matches!(
                    definition.dependencies[dependency as usize].resolution,
                    DependencyResolution::Unresolved { .. }
                ));
            }
        }
    }
    assert!(
        defs[0]
            .dependencies
            .iter()
            .any(|dependency| matches!(&dependency.resolution,
        DependencyResolution::Binding { binding_id, .. } if binding_id == &defs[1].binding_id))
    );
    assert!(
        defs[1]
            .expression
            .nodes
            .iter()
            .any(|node| node.expression.literal
                == Some(LiteralEvidence::Known {
                    value: KnownLiteral::Boolean(false)
                }))
    );
    assert!(
        out.nodes
            .iter()
            .any(|node| node.props["reason_code"] == "runtime_value_unknown")
    );
    assert_eq!(
        rule.interpretation.execution_predicate,
        InterpretationStatus::NotEstablished
    );
    assert_eq!(
        rule.interpretation.consumer_effect,
        InterpretationStatus::NotEstablished
    );
    let (_, repeat) = extract(code);
    assert_eq!(rules, repeat);
}

#[test]
fn local_definition_operators_preserve_closed_source_forms_without_evaluation() {
    // AC-0166: ordinary arithmetic and comparisons remain observed operators;
    // do not fold operands or mistake optional/computed members for values.
    for (operator, expected) in [
        ("==", BinaryOperator::LooseEqual),
        ("!=", BinaryOperator::LooseNotEqual),
        ("===", BinaryOperator::StrictEqual),
        ("!==", BinaryOperator::StrictNotEqual),
        ("<", BinaryOperator::LessThan),
        ("<=", BinaryOperator::LessThanOrEqual),
        (">", BinaryOperator::GreaterThan),
        (">=", BinaryOperator::GreaterThanOrEqual),
        ("+", BinaryOperator::Add),
        ("-", BinaryOperator::Subtract),
        ("*", BinaryOperator::Multiply),
        ("/", BinaryOperator::Divide),
        ("%", BinaryOperator::Remainder),
        ("**", BinaryOperator::Exponent),
        ("<<", BinaryOperator::LeftShift),
        (">>", BinaryOperator::RightShift),
        (">>>", BinaryOperator::UnsignedRightShift),
        ("&", BinaryOperator::BitwiseAnd),
        ("|", BinaryOperator::BitwiseOr),
        ("^", BinaryOperator::BitwiseXor),
        ("in", BinaryOperator::In),
        ("instanceof", BinaryOperator::Instanceof),
    ] {
        let (_, rules) = extract(&format!(
            "function run(a,b) {{ const observed = a {operator} b; if(observed) return false; }}"
        ));
        let definition = &definitions(&rules[0])[0];
        assert!(matches!(definition.expression.nodes[0].kind,
            DefinitionExpressionKind::Binary { operator, left: 1, right: 2 } if operator == expected));
        assert!(definition.initializer.literal.is_none());
    }
    for (operator, expected) in [
        ("!", UnaryOperator::Not),
        ("+", UnaryOperator::Plus),
        ("-", UnaryOperator::Minus),
        ("~", UnaryOperator::BitwiseNot),
        ("typeof", UnaryOperator::Typeof),
        ("void", UnaryOperator::Void),
    ] {
        let (_, rules) = extract(&format!(
            "function run(a) {{ const observed = {operator} a; if(observed) return false; }}"
        ));
        assert!(matches!(definitions(&rules[0])[0].expression.nodes[0].kind,
            DefinitionExpressionKind::Unary { operator, operand: 1 } if operator == expected));
    }
    for (operator, expected) in [
        ("&&", LogicalOperator::And),
        ("||", LogicalOperator::Or),
        ("??", LogicalOperator::Nullish),
    ] {
        let (_, rules) = extract(&format!(
            "function run(a,b) {{ const observed = a {operator} b; if(observed) return false; }}"
        ));
        assert!(matches!(definitions(&rules[0])[0].expression.nodes[0].kind,
            DefinitionExpressionKind::Logical { operator, left: 1, right: 2 } if operator == expected));
    }
    let (_, rules) = extract(
        "function run(value,key){const observed = (value?.[key] ?? value.flag); if(observed) return false;}",
    );
    let arena = &definitions(&rules[0])[0].expression.nodes;
    assert!(matches!(
        arena[0].kind,
        DefinitionExpressionKind::Parenthesized { .. }
    ));
    assert!(arena.iter().any(|node| matches!(
        node.kind,
        DefinitionExpressionKind::Member {
            computed: true,
            optional: true,
            ..
        }
    )));
    assert!(arena.iter().any(|node| matches!(
        node.kind,
        DefinitionExpressionKind::Member {
            computed: false,
            optional: false,
            ..
        }
    )));
}

#[test]
fn local_definitions_reject_tdz_control_captures_and_non_const_bindings() {
    // AC-0165: lexical lookup alone is not proof of initialization.
    for (code, reason, expected_initializers) in [
        (
            "function run(){ if(value) return false; const value = true; }",
            "definition_order_unproven",
            vec![],
        ),
        (
            "function run(){ const first = true, value = first; if(value) return false; }",
            "definition_order_unproven",
            vec!["first"],
        ),
        (
            "function run(){ const value = value; if(value) return false; }",
            "definition_order_unproven",
            vec!["value"],
        ),
        (
            "function run(){ const value = later; const later = true; if(value) return false; }",
            "definition_order_unproven",
            vec!["later"],
        ),
        (
            "function run(){ let value = true; if(value) return false; }",
            "definition_binding_unsupported",
            vec![],
        ),
        (
            "function run(){ var value = true; if(value) return false; }",
            "definition_binding_unsupported",
            vec![],
        ),
        (
            "function run(){ const {value} = input; if(value) return false; }",
            "definition_binding_unsupported",
            vec![],
        ),
        (
            "function run(){ const value = true; value = false; if(value) return false; }",
            "definition_binding_unsupported",
            vec![],
        ),
        (
            "function outer(){ const value = true; function inner(){if(value)return false;} }",
            "definition_scope_unsupported",
            vec![],
        ),
        (
            "function run(){ while(enabled){ const value = true; if(value) return false; } }",
            "definition_scope_unsupported",
            vec![],
        ),
        (
            "function run(){ try { const value = true; if(value) return false; } finally {} }",
            "definition_scope_unsupported",
            vec![],
        ),
        (
            "function run(){ switch(input){case 1: const value = true; if(value) return false;} }",
            "definition_scope_unsupported",
            vec![],
        ),
    ] {
        let (out, rules) = extract(code);
        assert_eq!(rules.len(), 1, "{code}");
        assert!(
            out.nodes
                .iter()
                .any(|node| node.props["reason_code"] == reason),
            "missing {reason}: {code}"
        );
        let defs = definitions(&rules[0]);
        assert_eq!(
            defs.iter()
                .map(|definition| definition.initializer.display.as_str())
                .collect::<Vec<_>>(),
            expected_initializers,
            "{code}"
        );
        // Nested TDZ/self reads retain only the written outer `value`
        // initializer; no definition for the unresolved dependency is invented.
        assert!(
            defs.iter()
                .all(|definition| original(code, &definition.declaration).starts_with("value ="))
        );
    }
    let code = "function run(){ const value = false; { const value = true; if(value && value) return false; } }";
    let (_, rules) = extract(code);
    let defs = definitions(&rules[0]);
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].initializer.display.as_str(), "true");
    assert_eq!(defs[0].uses.len(), 2);
    let (_, rules) =
        extract("function run(){ const value = false; if(enabled) { if(value) return false; } }");
    assert_eq!(
        definitions(&rules[0]).len(),
        1,
        "earlier ancestor-block declarations remain useful"
    );
}

#[test]
fn local_definition_dependencies_resolve_at_original_read_before_later_shadow() {
    // AC-0164: the guard's nearer flag binding must not replace the parameter
    // which the earlier initializer actually read.
    let code = "function run(flag){ const observed = flag === false; { const flag = true; if(observed) return false; } }";
    let (_, rules) = extract(code);
    let defs = definitions(&rules[0]);
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].initializer.display.as_str(), "flag === false");
    let dependency = defs[0]
        .dependencies
        .iter()
        .find(|dependency| original(code, &dependency.source) == "flag")
        .unwrap();
    let DependencyResolution::Binding {
        declaration,
        binding_id,
    } = &dependency.resolution
    else {
        panic!("original parameter remains a cited binding");
    };
    assert_eq!(original(code, declaration), "flag");
    assert_eq!(declaration.byte_start as usize, code.find("flag").unwrap());
    assert_eq!(
        binding_id,
        &format!(
            "binding:fixture@rules.ts#declaration@{}",
            declaration.byte_start
        )
    );
    assert!(
        defs.iter()
            .all(|definition| definition.initializer.display.as_str() != "true")
    );
}

#[test]
fn local_definition_unsupported_and_mutated_collection_values_remain_unknown() {
    // AC-0166: preserve the written array initializer and mutable member read,
    // without replacing the condition or interpreting a post-mutation length.
    let code = "function run(){ const values = []; values.push(1); const empty = values.length === 0; if(empty) return false; }";
    let (out, rules) = extract(code);
    let defs = definitions(&rules[0]);
    assert_eq!(defs.len(), 2);
    assert_eq!(
        rules[0].conditions[0].expression.display.as_str(),
        "(empty)"
    );
    assert_eq!(defs[1].initializer.display.as_str(), "[]");
    assert!(matches!(
        defs[1].expression.nodes[0].kind,
        DefinitionExpressionKind::Unsupported { .. }
    ));
    assert!(
        out.nodes
            .iter()
            .any(|node| node.props["reason_code"] == "runtime_value_unknown")
    );
    for initializer in [
        "read()",
        "new Thing()",
        "{}",
        "await read()",
        "a++",
        "a = 1",
        "delete a.x",
        "`value`",
        "/pattern/",
        "(() => hidden())",
        "(a as boolean)",
    ] {
        let code = format!(
            "async function run(a){{const value = {initializer}; if(value) return false;}}"
        );
        let (out, rules) = extract(&code);
        assert_eq!(rules.len(), 1, "{initializer}");
        let definition = &definitions(&rules[0])[0];
        assert!(
            definition
                .expression
                .nodes
                .iter()
                .any(|node| matches!(node.kind, DefinitionExpressionKind::Unsupported { .. })),
            "{initializer}"
        );
        assert!(
            out.nodes
                .iter()
                .any(|node| node.props["reason_code"] == "local_definition_unsupported")
        );
    }
}

#[test]
fn local_definition_citations_sanitize_secrets_comments_and_preserve_false() {
    // AC-0167: every new source display is sanitized on the original AST,
    // including credential-context literals and escaped provider credentials.
    let code = r#"function run(){
        const password = false;
        const token = "\x67hp_localdefinition1234";
        const enabled = password === /* initializer-private-comment */ false;
        if(enabled && token) return false;
    }"#;
    let (out, rules) = extract(code);
    let serialized = serde_json::to_string(&(&out.nodes, &out.edges)).unwrap();
    for canary in ["localdefinition1234", "initializer-private-comment"] {
        assert!(!serialized.contains(canary));
    }
    let defs = definitions(&rules[0]);
    assert_eq!(defs.len(), 3);
    assert!(defs.iter().any(|definition| definition.initializer.literal
        == Some(LiteralEvidence::Known {
            value: KnownLiteral::Boolean(false)
        })));
    assert!(defs.iter().any(|definition| matches!(
        definition.initializer.literal,
        Some(LiteralEvidence::Withheld { .. })
    )));
    assert!(
        rules[0]
            .redactions
            .iter()
            .any(|redaction| redaction.reason == RedactionReason::RemovedComment)
    );
    for redaction in &rules[0].redactions {
        assert!(redaction.source.byte_start < redaction.source.byte_end);
        assert!(redaction.source.byte_end <= code.len() as u64);
    }
}

#[test]
fn local_definition_capture_limits_are_explicit_and_never_publish_partial_arenas() {
    // AC-0167: bounds cover emitted definitions, arenas and traversal chains;
    // exhausted expression capture keeps one unsupported root, never a prefix.
    let declarations = (0..24)
        .map(|i| format!("const value{i}=true;"))
        .collect::<String>();
    let condition = (0..24)
        .map(|i| format!("value{i}"))
        .collect::<Vec<_>>()
        .join(" || ");
    let (out, rules) = extract(&format!(
        "function run(){{{declarations}if({condition})return false;}}"
    ));
    assert_eq!(definitions(&rules[0]).len(), MAX_LOCAL_DEFINITIONS);
    assert!(
        out.nodes
            .iter()
            .any(|node| node.props["reason_code"] == "definition_limit")
    );
    let code = "function run(){ const a=true; const b=a; const c=b; const d=c; const e=d; const f=e; if(f)return false; }";
    let (out, rules) = extract(code);
    assert_eq!(definitions(&rules[0]).len(), MAX_DEFINITION_CHAIN_DEPTH);
    assert!(
        out.nodes
            .iter()
            .any(|node| node.props["reason_code"] == "definition_limit")
    );
    let (out, rules) = extract(&format!(
        "function run(a){{const value={}a; if(value)return false;}}",
        "!".repeat(40)
    ));
    let definition = &definitions(&rules[0])[0];
    assert_eq!(definition.expression.nodes.len(), 1);
    assert!(matches!(
        definition.expression.nodes[0].kind,
        DefinitionExpressionKind::Unsupported { .. }
    ));
    assert!(definition.dependencies.is_empty());
    assert!(
        out.nodes
            .iter()
            .any(|node| node.props["reason_code"] == "definition_limit")
    );
    let declarations = (0..16)
        .map(|i| format!("const value{i}=a===0 || b===1;"))
        .collect::<String>();
    let condition = (0..16)
        .map(|i| format!("value{i}"))
        .collect::<Vec<_>>()
        .join(" || ");
    let (out, rules) = extract(&format!(
        "function run(a,b){{{declarations}if({condition})return false;}}"
    ));
    let defs = definitions(&rules[0]);
    assert_eq!(
        defs.iter()
            .map(|definition| definition.expression.nodes.len())
            .sum::<usize>(),
        MAX_DEFINITION_NODES
    );
    let last = defs.last().unwrap();
    assert_eq!(last.expression.nodes.len(), 1);
    assert!(matches!(
        last.expression.nodes[0].kind,
        DefinitionExpressionKind::Unsupported { .. }
    ));
    assert!(last.dependencies.is_empty());
    assert!(
        out.nodes
            .iter()
            .any(|node| node.props["reason_code"] == "definition_limit")
    );
}

#[test]
fn local_definition_cache_retargets_every_new_source_and_complete_hash() {
    // AC-0168: cached v2 definitions cannot leave stale initializer/use/node
    // references or complete-fact hashes when the revision changes.
    let dir = tempfile::tempdir().unwrap();
    let code = "function run(input){const value=input.flag === false; if(value)return false;}";
    std::fs::write(dir.path().join("rules.ts"), code).unwrap();
    let mut cache = IncrementalCache::default();
    extract_dir_incremental(
        dir.path(),
        &SourceId {
            repo: "fixture",
            commit: "old-revision",
        },
        &mut cache,
    )
    .unwrap();
    let id = SourceId {
        repo: "fixture",
        commit: "new-revision",
    };
    let (cached, stats) = extract_dir_incremental(dir.path(), &id, &mut cache).unwrap();
    assert_eq!(stats.reused_files, 1);
    let (fresh, _) =
        extract_dir_incremental(dir.path(), &id, &mut IncrementalCache::default()).unwrap();
    assert_eq!(
        serde_json::to_value((&cached.nodes, &cached.edges)).unwrap(),
        serde_json::to_value((&fresh.nodes, &fresh.edges)).unwrap()
    );
    assert!(
        !serde_json::to_string(&cached.nodes)
            .unwrap()
            .contains("old-revision")
    );
    let rule = cached
        .nodes
        .iter()
        .find(|node| node.label == "BusinessRule")
        .unwrap();
    assert_eq!(
        rule.props["rule"]["local_definitions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
