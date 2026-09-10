use super::*;
use core_graph::rules::GuardedExitEvidence;

const CODE: &[u8] = b"function run(input:any){ const base = input.enabled; const ready = base === false; if (ready) return false; }";

fn captured(
    code: &[u8],
) -> (
    tempfile::TempDir,
    source_capture::Capture,
    crate::Extraction,
    Vec<Receipt>,
) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("source.ts"), code).unwrap();
    let source = source_capture::SourceId::new("src_11111111111111111111111111111111").unwrap();
    let capture = source_capture::capture_working_tree(
        dir.path(),
        &source,
        &["source.ts".into()],
        source_capture::CaptureLimits::default(),
    )
    .unwrap();
    let (facts, receipts) = super::super::extract_file(
        capture.file("source.ts").unwrap(),
        &SourceId {
            repo: "acme/project",
            commit: "workdir",
        },
    )
    .unwrap();
    (dir, capture, facts, receipts)
}

fn rule_receipt<'a>(
    facts: &'a crate::Extraction,
    receipts: &'a [Receipt],
) -> (&'a Node, &'a Receipt) {
    let node = facts
        .nodes
        .iter()
        .find(|node| node.label == "BusinessRule")
        .unwrap();
    let receipt = receipts
        .iter()
        .find(|receipt| receipt.fact_key() == &FactKey::from_node(node))
        .unwrap();
    (node, receipt)
}

#[test]
fn receipt_v1_wire_identity_and_order_remain_unchanged() {
    // AC-0164 / AC-0169: freeze the old contract independently of new minting.
    let file = format!(
        "{{\"source_id\":\"src_{}\",\"capture_id\":\"capture-v1:{}\",\"path\":\"source.ts\",\"digest\":\"{}\",\"byte_len\":5}}",
        "1".repeat(32),
        "c".repeat(64),
        "a".repeat(64)
    );
    let content_json = format!(
        concat!(
            "{{\"schema_version\":1,\"source_id\":\"src_{}\",\"repo_key\":\"acme/project\",\"file\":{},",
            "\"producer_contract\":\"t0.adapter-ts/direct-lexical-v1\",\"grammar\":\"typescript\",",
            "\"grammar_package\":\"tree-sitter-typescript@0.23.2\",\"parser_package\":\"tree-sitter@0.26.12\",",
            "\"fact_key\":{{\"kind\":\"node\",\"id\":\"symbol:historical\"}},\"fact_digest\":\"node-v1:{}\",",
            "\"ranges\":[{{\"role\":\"provenance\",\"index\":0,\"evidence\":{{\"repo\":\"acme/project\",",
            "\"path\":\"source.ts\",\"byte_start\":0,\"byte_end\":5,\"commit_sha\":\"historical\"}},",
            "\"captured\":{{\"file\":{},\"byte_start\":0,\"byte_end\":5}}}}],",
            "\"source_scope\":\"primary_source_only\",\"input_closure\":\"input_closure_not_established\"}}"
        ),
        "1".repeat(32),
        file,
        "b".repeat(64),
        file
    );
    // serde_json's Value maps sort keys recursively; this is the original v1
    // canonical byte recipe and literal domain, not the version-dispatch helper.
    let content: serde_json::Value = serde_json::from_str(&content_json).unwrap();
    let mut bytes = b"cartograph:ts-primary-receipt:v1\0".to_vec();
    bytes.extend(serde_json::to_vec(&content).unwrap());
    let expected_id = format!("ts-primary-v1:{}", core_prov::content_hash(&bytes));
    let original = format!("{{\"receipt_id\":\"{expected_id}\",\"content\":{content_json}}}");
    let decoded = Receipt::from_json(&original).unwrap();
    assert_eq!(decoded.id(), expected_id);
    assert_eq!(decoded.to_json().unwrap(), original);
    assert_eq!(decoded.ranges()[0].role, RangeRole::Provenance);
    for version in [0, 3] {
        let mut invalid = decoded.clone();
        invalid.content.schema_version = version;
        assert!(invalid.validate().is_err());
        assert!(Receipt::from_json(&serde_json::to_string(&invalid).unwrap()).is_err());
    }
    let mut mismatched = decoded;
    mismatched.content.schema_version = 2;
    mismatched.receipt_id = content_id(&mismatched.content).unwrap();
    assert!(mismatched.validate().is_err());
}

#[test]
fn legacy_guarded_exit_receipt_keeps_all_v1_roles_and_rejects_v2_rules() {
    // AC-0164 / AC-0169: reconstruct a legacy guarded-exit envelope with the
    // original v1 inventory/domain recipe, independently of the new mint path.
    let code = b"function run(input:any){ const base = input.enabled; const ready = base === false; if (ready /* historical note */) return false; }";
    let (_dir, capture, facts, receipts) = captured(code);
    let (new_node, new_receipt) = rule_receipt(&facts, &receipts);
    let mut legacy_node = new_node.clone();
    let mut rule = GuardedExitEvidence::from_value(new_node.props["rule"].clone()).unwrap();
    assert!(!rule.redactions.is_empty());
    rule.schema_version = 1;
    rule.local_definitions = None;
    rule.validate().unwrap();
    legacy_node.props["rule"] = serde_json::to_value(&rule).unwrap();
    legacy_node.props["prov"]["content_hash"] =
        core_prov::content_hash(&serde_json::to_vec(&legacy_node.props["rule"]).unwrap()).into();
    let provenance: Provenance = serde_json::from_value(legacy_node.props["prov"].clone()).unwrap();
    let mut expected = Vec::new();
    for (index, reference) in provenance.evidence.iter().enumerate() {
        expected.push((RangeRole::Provenance, index as u32, reference.clone()));
    }
    expected.push((RangeRole::RuleExit, 0, rule.exit_source.clone()));
    for (index, condition) in rule.conditions.iter().enumerate() {
        expected.push((
            RangeRole::ConditionBranch,
            index as u32,
            condition.branch_source.clone(),
        ));
        expected.push((
            RangeRole::ConditionExpression,
            index as u32,
            condition.expression.source.clone(),
        ));
    }
    let LocalExit::Return { value: Some(value) } = &rule.effect else {
        panic!("historical return fixture");
    };
    expected.push((RangeRole::ReturnValue, 0, value.source.clone()));
    for (index, dependency) in rule.dependencies.iter().enumerate() {
        expected.push((
            RangeRole::Dependency,
            index as u32,
            dependency.source.clone(),
        ));
        if let DependencyResolution::Binding { declaration, .. } = &dependency.resolution {
            expected.push((
                RangeRole::DependencyDeclaration,
                index as u32,
                declaration.clone(),
            ));
        }
    }
    for (index, redaction) in rule.redactions.iter().enumerate() {
        expected.push((RangeRole::Redaction, index as u32, redaction.source.clone()));
    }
    for role in [
        RangeRole::ConditionBranch,
        RangeRole::ConditionExpression,
        RangeRole::ReturnValue,
        RangeRole::Dependency,
        RangeRole::DependencyDeclaration,
        RangeRole::Redaction,
    ] {
        assert!(expected.iter().any(|(candidate, _, _)| *candidate == role));
    }
    let mut content = new_receipt.content.clone();
    content.schema_version = 1;
    content.producer_contract = "t0.adapter-ts/direct-lexical-v1".into();
    content.fact_digest = node_digest(&legacy_node).unwrap();
    let file = capture.file("source.ts").unwrap();
    content.ranges = expected
        .iter()
        .map(|(role, index, evidence)| ReceiptRange {
            role: *role,
            index: *index,
            evidence: evidence.clone(),
            captured: file.span(evidence.byte_start, evidence.byte_end).unwrap(),
        })
        .collect();
    let mut canonical_v1 = b"cartograph:ts-primary-receipt:v1\0".to_vec();
    canonical_v1.extend(serde_json::to_vec(&serde_json::to_value(&content).unwrap()).unwrap());
    let receipt_id = format!("ts-primary-v1:{}", core_prov::content_hash(&canonical_v1));
    let legacy = Receipt {
        receipt_id: receipt_id.clone(),
        content,
    };
    let original_wire = serde_json::to_string(&legacy).unwrap();
    let restored = Receipt::from_json(&original_wire).unwrap();
    assert_eq!(restored.id(), receipt_id);
    assert_eq!(restored.to_json().unwrap(), original_wire);
    assert_eq!(
        restored
            .ranges()
            .iter()
            .map(|range| (range.role, range.index, range.evidence.clone()))
            .collect::<Vec<_>>(),
        expected
    );
    assert!(restored.matches_node(&legacy_node));
    assert!(!restored.matches_node(new_node));
    assert!(!restored.matches_inventory(&new_node.props));
    assert!(inventory(&new_node.props, 1).is_err());
}

#[test]
fn captured_definition_receipt_v2_covers_every_source_occurrence() {
    // AC-0168 / AC-0169: the real immutable parser supplies all new citations.
    let (_dir, capture, facts, receipts) = captured(CODE);
    let (node, receipt) = rule_receipt(&facts, &receipts);
    assert_eq!(receipt.content.schema_version, 2);
    assert_eq!(
        receipt.content.producer_contract,
        "t0.adapter-ts/direct-lexical-v2"
    );
    assert!(receipt.id().starts_with("ts-primary-v2:"));
    assert!(receipt.matches_node(node));
    let rule = GuardedExitEvidence::from_value(node.props["rule"].clone()).unwrap();
    let definitions = rule.local_definitions.as_ref().unwrap();
    assert_eq!(definitions.len(), 2);
    let mut expected = Vec::new();
    let (mut use_index, mut expression_index, mut dependency_index) = (0, 0, 0);
    for (index, definition) in definitions.iter().enumerate() {
        expected.push((
            RangeRole::DefinitionDeclaration,
            index as u32,
            definition.declaration.clone(),
        ));
        for usage in &definition.uses {
            expected.push((RangeRole::DefinitionUse, use_index, usage.clone()));
            use_index += 1;
        }
        expected.push((
            RangeRole::DefinitionInitializer,
            index as u32,
            definition.initializer.source.clone(),
        ));
        for node in &definition.expression.nodes {
            expected.push((
                RangeRole::DefinitionExpression,
                expression_index,
                node.expression.source.clone(),
            ));
            expression_index += 1;
        }
        for dependency in &definition.dependencies {
            expected.push((
                RangeRole::DefinitionDependency,
                dependency_index,
                dependency.source.clone(),
            ));
            if let DependencyResolution::Binding { declaration, .. } = &dependency.resolution {
                expected.push((
                    RangeRole::DefinitionDependencyDeclaration,
                    dependency_index,
                    declaration.clone(),
                ));
            }
            dependency_index += 1;
        }
    }
    let new_roles = |role| {
        matches!(
            role,
            RangeRole::DefinitionDeclaration
                | RangeRole::DefinitionUse
                | RangeRole::DefinitionInitializer
                | RangeRole::DefinitionExpression
                | RangeRole::DefinitionDependency
                | RangeRole::DefinitionDependencyDeclaration
        )
    };
    let actual: Vec<_> = receipt
        .ranges()
        .iter()
        .filter(|range| new_roles(range.role))
        .map(|range| (range.role, range.index, range.evidence.clone()))
        .collect();
    assert_eq!(actual, expected);
    for definition in definitions {
        assert!(
            receipt
                .ranges()
                .iter()
                .filter(|range| range.evidence == definition.initializer.source)
                .count()
                >= 2
        );
    }
    let json = receipt.to_json().unwrap();
    assert_eq!(Receipt::from_json(&json).unwrap().to_json().unwrap(), json);
    assert!(!json.contains("input.enabled"));
    let retained = tempfile::tempdir().unwrap();
    let path = retained.path().join("captures.sqlite");
    {
        let mut store =
            source_capture::CaptureStore::open(&path, source_capture::StoreLimits::default())
                .unwrap();
        store.persist(&capture).unwrap();
    }
    let store =
        source_capture::CaptureStore::open(&path, source_capture::StoreLimits::default()).unwrap();
    for range in receipt.ranges() {
        assert_eq!(
            store.read_span(&range.captured).unwrap(),
            CODE[range.evidence.byte_start as usize..range.evidence.byte_end as usize]
        );
    }
}

#[test]
fn receipt_v2_rejects_malformed_groups_and_complete_fact_mismatches() {
    // AC-0169: rehashing an incomplete inventory cannot bind the complete fact.
    let (_dir, _capture, facts, receipts) = captured(CODE);
    let (node, original) = rule_receipt(&facts, &receipts);
    for role in [
        RangeRole::DefinitionDeclaration,
        RangeRole::DefinitionUse,
        RangeRole::DefinitionInitializer,
        RangeRole::DefinitionExpression,
        RangeRole::DefinitionDependencyDeclaration,
    ] {
        let mut invalid = original.clone();
        let range = invalid
            .content
            .ranges
            .iter_mut()
            .find(|range| range.role == role)
            .unwrap();
        range.index += 2;
        invalid.receipt_id = content_id(&invalid.content).unwrap();
        assert!(invalid.validate().is_err(), "{role:?}");
    }
    let mut missing = original.clone();
    missing
        .content
        .ranges
        .retain(|range| range.role != RangeRole::DefinitionInitializer);
    missing.receipt_id = content_id(&missing.content).unwrap();
    assert!(missing.validate().is_err());
    let mut omitted = original.clone();
    omitted.content.ranges.retain(|range| {
        !matches!(
            range.role,
            RangeRole::DefinitionDeclaration
                | RangeRole::DefinitionUse
                | RangeRole::DefinitionInitializer
                | RangeRole::DefinitionExpression
                | RangeRole::DefinitionDependency
                | RangeRole::DefinitionDependencyDeclaration
        )
    });
    omitted.receipt_id = content_id(&omitted.content).unwrap();
    omitted.validate().unwrap();
    assert!(!omitted.matches_node(node));
    let mut changed = node.clone();
    changed.props["rule"]["local_definitions"][0]["initializer"]["display"] = "different".into();
    assert!(!original.matches_node(&changed));
    let mut disguised_v1 = original.clone();
    disguised_v1.content.schema_version = 1;
    disguised_v1.content.producer_contract = "t0.adapter-ts/direct-lexical-v1".into();
    disguised_v1.receipt_id = content_id(&disguised_v1.content).unwrap();
    assert!(disguised_v1.validate().is_err());
}

#[test]
fn receipt_v2_capture_replacement_preserves_original_metadata_and_ranges() {
    // AC-0169: a changed capture cannot silently replace historical source bytes.
    let mut first_code = CODE.to_vec();
    first_code.extend(b" // one");
    let mut second_code = CODE.to_vec();
    second_code.extend(b" // two");
    let (_a, first_capture, first_facts, first_receipts) = captured(&first_code);
    let (_b, second_capture, second_facts, second_receipts) = captured(&second_code);
    let (first_node, first_receipt) = rule_receipt(&first_facts, &first_receipts);
    let (second_node, second_receipt) = rule_receipt(&second_facts, &second_receipts);
    assert_eq!(
        node_digest(first_node).unwrap(),
        node_digest(second_node).unwrap()
    );
    assert_ne!(first_capture.id(), second_capture.id());
    assert_ne!(first_receipt.id(), second_receipt.id());
    let original = first_receipt.to_json().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("captures.sqlite");
    {
        let mut store =
            source_capture::CaptureStore::open(&path, source_capture::StoreLimits::default())
                .unwrap();
        store.persist(&first_capture).unwrap();
        store.persist(&second_capture).unwrap();
    }
    let store =
        source_capture::CaptureStore::open(&path, source_capture::StoreLimits::default()).unwrap();
    let historical = Receipt::from_json(&original).unwrap();
    assert_eq!(historical.to_json().unwrap(), original);
    for range in historical.ranges() {
        assert_eq!(
            store.read_span(&range.captured).unwrap(),
            first_code[range.captured.byte_start as usize..range.captured.byte_end as usize]
        );
    }
}

#[test]
fn receipt_v2_range_and_byte_caps_never_truncate_inventories() {
    // AC-0167 / AC-0169: complete envelopes and inventory counts are hard caps.
    let (_dir, _capture, facts, receipts) = captured(CODE);
    let (_, original) = rule_receipt(&facts, &receipts);
    let mut oversized = original.clone();
    oversized.content.ranges = (0..MAX_RECEIPT_RANGES + 1)
        .map(|index| {
            let mut range = original.ranges()[0].clone();
            range.role = RangeRole::Provenance;
            range.index = index as u32;
            range
        })
        .collect();
    oversized.receipt_id = content_id(&oversized.content).unwrap();
    assert!(matches!(
        oversized.validate(),
        Err(CapturedError::Limit("receipt ranges"))
    ));
    oversized.content.ranges.truncate(MAX_RECEIPT_RANGES);
    oversized.receipt_id = content_id(&oversized.content).unwrap();
    assert!(matches!(
        oversized.validate(),
        Err(CapturedError::Limit("receipt bytes"))
    ));
    assert!(oversized.to_json().is_err());
    assert!(Receipt::from_json(&serde_json::to_string(&oversized).unwrap()).is_err());
    assert_eq!(oversized.content.ranges.len(), MAX_RECEIPT_RANGES);
}
