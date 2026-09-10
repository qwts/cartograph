use super::*;
use core_prov::{EvidenceRef, Tier};
use serde_json::json;

fn node(id: &str, label: &str, props: Value) -> Node {
    Node {
        id: id.into(),
        label: label.into(),
        props,
    }
}

fn edge(source: &str, label: &str, destination: &str, props: Value) -> Edge {
    Edge {
        src: source.into(),
        label: label.into(),
        dst: destination.into(),
        props,
    }
}

fn provenance(tier: Tier, confidence: ConfidenceTier) -> Provenance {
    Provenance::new(
        tier,
        confidence,
        vec![EvidenceRef {
            repo: "example/shop".into(),
            path: "src/cart.ts".into(),
            byte_start: 10,
            byte_end: 25,
            commit_sha: "a".repeat(40),
        }],
        "context.test@1",
        b"recovered cart guard",
    )
    .unwrap()
}

fn references(response: &QueryResponse) -> Vec<FactReference> {
    response
        .facts
        .iter()
        .map(|fact| fact.reference.clone())
        .collect()
}

fn node_ref(id: &str) -> FactReference {
    FactReference::Node { id: id.into() }
}

fn edge_ref(source: &str, label: &str, destination: &str) -> FactReference {
    FactReference::Edge {
        source: source.into(),
        label: label.into(),
        destination: destination.into(),
    }
}

#[test]
fn snapshot_identity_tracks_full_content_and_ignores_input_order() {
    // AC-0101: identity covers all original content, not just stored prov hashes.
    let prov = provenance(Tier::Deterministic, ConfidenceTier::Confirmed);
    let nodes = vec![
        node("b", "Symbol", json!({"prov": prov, "name": "cart"})),
        node(
            "a",
            "Module",
            serde_json::from_str(r#"{"nested":{"z":2,"a":1},"array":[1,2]}"#).unwrap(),
        ),
    ];
    let edges = vec![
        edge("b", "DEFINED_IN", "a", json!({"position": [10, 20]})),
        edge("b", "CALLS", "b", json!({"recursive": true})),
    ];
    let baseline = ContextSnapshot::new(nodes.clone(), edges.clone()).unwrap();
    let mut reordered_nodes = nodes.clone();
    reordered_nodes.reverse();
    reordered_nodes[0].props =
        serde_json::from_str(r#"{"array":[1,2],"nested":{"a":1,"z":2}}"#).unwrap();
    let mut reordered_edges = edges.clone();
    reordered_edges.reverse();
    let reordered = ContextSnapshot::new(reordered_nodes, reordered_edges).unwrap();
    assert!(baseline.id().starts_with("context-v1:"));
    assert_eq!(baseline.id(), reordered.id());
    assert_eq!(
        baseline.query(QueryRequest::default()).unwrap(),
        reordered.query(QueryRequest::default()).unwrap()
    );

    let mut changed = nodes.clone();
    changed[1].props["nested"]["z"] = json!(3);
    assert_ne!(
        baseline.id(),
        ContextSnapshot::new(changed, edges.clone()).unwrap().id()
    );
    let mut changed = nodes.clone();
    changed[1].props["array"] = json!([2, 1]);
    assert_ne!(
        baseline.id(),
        ContextSnapshot::new(changed, edges.clone()).unwrap().id()
    );
    let mut changed = nodes.clone();
    changed[0].props["name"] = json!("payment");
    assert_eq!(changed[0].props["prov"], nodes[0].props["prov"]);
    assert_ne!(
        baseline.id(),
        ContextSnapshot::new(changed, edges.clone()).unwrap().id()
    );
    let mut changed = nodes.clone();
    changed[0].props["prov"]["evidence"][0]["commit_sha"] = json!("b".repeat(40));
    assert_ne!(
        baseline.id(),
        ContextSnapshot::new(changed, edges.clone()).unwrap().id()
    );
    let mut changed = edges.clone();
    changed[0].props["position"] = json!([11, 20]);
    assert_ne!(
        baseline.id(),
        ContextSnapshot::new(nodes, changed).unwrap().id()
    );
}

#[test]
fn duplicate_fact_identities_are_rejected() {
    // AC-0101: conflicting properties do not disambiguate a duplicate identity.
    for props in [json!({}), json!({"name": "different"})] {
        let duplicate = ContextSnapshot::new(
            vec![node("a", "Symbol", json!({})), node("a", "Module", props)],
            vec![],
        );
        assert!(matches!(duplicate, Err(ContextError::DuplicateNode(id)) if id == "a"));
    }
    for props in [json!({}), json!({"line": 2})] {
        let duplicate = ContextSnapshot::new(
            vec![],
            vec![
                edge("a", "CALLS", "b", json!({})),
                edge("a", "CALLS", "b", props),
            ],
        );
        assert!(
            matches!(duplicate, Err(ContextError::DuplicateEdge(reference))
            if reference == edge_ref("a", "CALLS", "b"))
        );
    }
    let distinct = ContextSnapshot::new(
        vec![
            node("a", "Symbol", json!({})),
            node("b", "Symbol", json!({})),
        ],
        vec![
            edge("a", "CALLS", "b", json!({})),
            edge("b", "CALLS", "a", json!({})),
            edge("a", "IMPORTS", "b", json!({})),
        ],
    )
    .unwrap();
    assert_eq!(
        distinct
            .query(QueryRequest::default())
            .unwrap()
            .total_selected,
        5
    );
}

#[test]
fn cursors_bind_snapshot_and_normalized_selection() {
    // AC-0101: paging can change budgets, never source content or selection.
    let nodes = vec![
        node("a", "Module", json!({})),
        node("b", "Symbol", json!({})),
        node("c", "Symbol", json!({})),
    ];
    let snapshot = ContextSnapshot::new(nodes.clone(), vec![]).unwrap();
    let selection = QueryRequest {
        labels: vec!["Symbol".into(), "Module".into(), "Symbol".into()],
        max_facts: 1,
        ..QueryRequest::default()
    };
    let first = snapshot.query(selection.clone()).unwrap();
    assert_eq!(references(&first), vec![node_ref("a")]);
    let cursor = first.next_cursor.unwrap();
    let continuation = QueryRequest {
        labels: vec!["Module".into(), "Symbol".into()],
        max_facts: 2,
        max_bytes: MAX_RESPONSE_BYTES / 2,
        cursor: Some(cursor.clone()),
        ..QueryRequest::default()
    };
    let rest = snapshot.query(continuation.clone()).unwrap();
    assert_eq!(references(&rest), vec![node_ref("b"), node_ref("c")]);
    assert_eq!(rest.total_selected, 3);
    assert!(rest.next_cursor.is_none());

    let mut changed_nodes = nodes;
    changed_nodes[2].props = json!({"new_property": true});
    let changed_snapshot = ContextSnapshot::new(changed_nodes, vec![]).unwrap();
    assert!(matches!(
        changed_snapshot.query(continuation.clone()),
        Err(ContextError::CursorSnapshotMismatch)
    ));
    for changed in [
        QueryRequest {
            labels: vec!["Symbol".into()],
            ..continuation.clone()
        },
        QueryRequest {
            kind: Some(FactKind::Node),
            ..continuation.clone()
        },
        QueryRequest {
            scope: QueryScope::Neighborhood {
                anchor: "a".into(),
                hops: 1,
            },
            ..continuation.clone()
        },
    ] {
        assert!(matches!(
            snapshot.query(changed),
            Err(ContextError::CursorSelectionMismatch)
        ));
    }
    let mut invalid_cursor = cursor.clone();
    invalid_cursor.offset = 4;
    assert!(matches!(
        snapshot.query(QueryRequest {
            cursor: Some(invalid_cursor),
            ..continuation.clone()
        }),
        Err(ContextError::InvalidCursorOffset {
            offset: 4,
            total_selected: 3
        })
    ));
    let mut end_cursor = cursor;
    end_cursor.offset = 3;
    let end = snapshot
        .query(QueryRequest {
            cursor: Some(end_cursor),
            ..continuation
        })
        .unwrap();
    assert!(end.facts.is_empty());
    assert!(end.next_cursor.is_none());
    assert_eq!(end.total_selected, 3);
}

#[test]
fn paging_honors_item_and_entire_response_byte_budgets() {
    // AC-0102: compact UTF-8 bytes include metadata and the continuation cursor.
    let nodes = (0..5)
        .rev()
        .map(|index| {
            node(
                &format!("n{index}"),
                "Symbol",
                json!({"text": "é📍\n\"".repeat(100)}),
            )
        })
        .collect();
    let snapshot = ContextSnapshot::new(nodes, vec![]).unwrap();
    let first = snapshot
        .query(QueryRequest {
            max_facts: 1,
            ..QueryRequest::default()
        })
        .unwrap();
    let first_json = serde_json::to_string(&first).unwrap();
    assert!(first_json.len() > first_json.chars().count());
    let page_budget = first_json.len();
    let mut cursor = None;
    let mut found = Vec::new();
    loop {
        let page = snapshot
            .query(QueryRequest {
                max_facts: 2,
                max_bytes: page_budget,
                cursor,
                ..QueryRequest::default()
            })
            .unwrap();
        assert_eq!(page.snapshot_id, snapshot.id());
        assert_eq!(page.view, ContextView::RecoveredGraph);
        assert_eq!(page.total_selected, 5);
        assert_eq!(page.facts.len(), 1);
        assert!(serde_json::to_vec(&page).unwrap().len() <= page_budget);
        found.extend(references(&page));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        found,
        (0..5)
            .map(|index| node_ref(&format!("n{index}")))
            .collect::<Vec<_>>()
    );
    assert!(matches!(
        snapshot.query(QueryRequest { max_facts: 1, max_bytes: page_budget - 1, ..QueryRequest::default() }),
        Err(ContextError::ResponseBudgetExceeded { required_bytes, .. }) if required_bytes == page_budget
    ));

    let empty = ContextSnapshot::new(vec![], vec![]).unwrap();
    let empty_page = empty.query(QueryRequest::default()).unwrap();
    let envelope_size = serde_json::to_vec(&empty_page).unwrap().len();
    assert_eq!(
        empty
            .query(QueryRequest {
                max_bytes: envelope_size,
                ..QueryRequest::default()
            })
            .unwrap(),
        empty_page
    );
    assert!(matches!(
        empty.query(QueryRequest { max_bytes: envelope_size - 1, ..QueryRequest::default() }),
        Err(ContextError::ResponseBudgetExceeded { required_bytes, .. }) if required_bytes == envelope_size
    ));
}

#[test]
fn oversized_next_fact_returns_an_explicit_budget_error() {
    // AC-0102: a later small fact never hides an oversized first/continuing fact.
    let huge = node(
        "b",
        "Symbol",
        json!({"text": "x".repeat(MAX_RESPONSE_BYTES)}),
    );
    let snapshot =
        ContextSnapshot::new(vec![huge.clone(), node("c", "Symbol", json!({}))], vec![]).unwrap();
    assert!(matches!(
        snapshot.query(QueryRequest::default()),
        Err(ContextError::ResponseBudgetExceeded { max_bytes: MAX_RESPONSE_BYTES, required_bytes })
            if required_bytes > MAX_RESPONSE_BYTES
    ));

    let snapshot = ContextSnapshot::new(
        vec![
            node("a", "Symbol", json!({})),
            huge,
            node("c", "Symbol", json!({})),
        ],
        vec![],
    )
    .unwrap();
    let first = snapshot.query(QueryRequest::default()).unwrap();
    assert_eq!(references(&first), vec![node_ref("a")]);
    assert_eq!(first.next_cursor.as_ref().unwrap().offset, 1);
    assert!(matches!(
        snapshot.query(QueryRequest { cursor: first.next_cursor, ..QueryRequest::default() }),
        Err(ContextError::ResponseBudgetExceeded { required_bytes, .. }) if required_bytes > MAX_RESPONSE_BYTES
    ));
}

#[test]
fn invalid_provenance_is_gap_and_never_leaks_raw_metadata() {
    // AC-0103: deserializing a plausible payload alone cannot establish confidence.
    let valid =
        serde_json::to_value(provenance(Tier::Agentic, ConfidenceTier::InferredWeak)).unwrap();
    let mut unknown_tier = valid.clone();
    unknown_tier["tier"] = json!("FutureTier");
    let mut unknown_confidence = valid.clone();
    unknown_confidence["confidence_tier"] = json!("Certain");
    let mut above_ceiling = valid;
    above_ceiling["confidence_tier"] = json!("Confirmed");
    let cases = [
        (json!({"payload": "kept"}), ProvenanceProblem::Missing),
        (
            json!({"payload": "kept", "prov": null}),
            ProvenanceProblem::Malformed,
        ),
        (
            json!({"payload": "kept", "prov": {"tier": "Deterministic"}}),
            ProvenanceProblem::Malformed,
        ),
        (
            json!({"payload": "kept", "prov": unknown_tier}),
            ProvenanceProblem::Malformed,
        ),
        (
            json!({"payload": "kept", "prov": unknown_confidence}),
            ProvenanceProblem::Malformed,
        ),
        (
            json!({"payload": "kept", "prov": above_ceiling}),
            ProvenanceProblem::AboveCeiling,
        ),
    ];
    for (props, problem) in cases {
        let snapshot = ContextSnapshot::new(
            vec![node("a", "Symbol", props.clone())],
            vec![edge("a", "CALLS", "a", props)],
        )
        .unwrap();
        for fact in snapshot.query(QueryRequest::default()).unwrap().facts {
            assert_eq!(fact.confidence_tier, ConfidenceTier::Gap);
            assert_eq!(fact.provenance_problem, Some(problem));
            assert!(fact.provenance.is_none());
            assert_eq!(fact.properties, json!({"payload": "kept"}));
            assert!(fact.properties.get("prov").is_none());
        }
    }
    for props in [Value::Null, json!([1, 2]), json!("not an object")] {
        let snapshot =
            ContextSnapshot::new(vec![node("a", "Symbol", props.clone())], vec![]).unwrap();
        let response = snapshot.query(QueryRequest::default()).unwrap();
        assert_eq!(
            response.facts[0].provenance_problem,
            Some(ProvenanceProblem::Missing)
        );
        assert_eq!(response.facts[0].confidence_tier, ConfidenceTier::Gap);
        assert_eq!(response.facts[0].properties, props);
    }
}

#[test]
fn inferred_provenance_remains_inferred() {
    // AC-0103: evidence survives projection; producing tiers are never upgraded.
    for (tier, confidence) in [
        (Tier::Deterministic, ConfidenceTier::Confirmed),
        (Tier::Dynamic, ConfidenceTier::Confirmed),
        (Tier::Semantic, ConfidenceTier::InferredStrong),
        (Tier::Agentic, ConfidenceTier::InferredWeak),
        (Tier::Agentic, ConfidenceTier::Gap),
    ] {
        let expected = provenance(tier, confidence);
        let snapshot = ContextSnapshot::new(
            vec![node(
                "a",
                "Symbol",
                json!({"prov": expected, "name": "cart"}),
            )],
            vec![edge(
                "a",
                "CALLS",
                "a",
                json!({"prov": expected, "name": "cart"}),
            )],
        )
        .unwrap();
        for fact in snapshot.query(QueryRequest::default()).unwrap().facts {
            assert_eq!(fact.provenance, Some(expected.clone()));
            assert!(fact.provenance_problem.is_none());
            assert_eq!(fact.confidence_tier, confidence);
            assert_eq!(fact.properties, json!({"name": "cart"}));
        }
    }
}

#[test]
fn neighborhood_is_bounded_undirected_and_induced() {
    // AC-0104: incoming links expand scope; all links within scope are retained.
    let nodes = ["e", "d", "c", "b", "a"]
        .into_iter()
        .map(|id| node(id, "Symbol", json!({})))
        .collect();
    let edges = vec![
        edge("outside", "CALLS", "a", json!({})),
        edge("d", "CALLS", "e", json!({})),
        edge("c", "CALLS", "d", json!({})),
        edge("b", "IMPORTS", "c", json!({})),
        edge("b", "CALLS", "a", json!({})),
        edge("a", "CALLS", "c", json!({})),
    ];
    let snapshot = ContextSnapshot::new(nodes, edges).unwrap();
    let request = QueryRequest {
        scope: QueryScope::Neighborhood {
            anchor: "a".into(),
            hops: 1,
        },
        ..QueryRequest::default()
    };
    let first = snapshot.query(request.clone()).unwrap();
    assert_eq!(
        references(&first),
        vec![
            node_ref("a"),
            node_ref("b"),
            node_ref("c"),
            edge_ref("a", "CALLS", "c"),
            edge_ref("b", "CALLS", "a"),
            edge_ref("b", "IMPORTS", "c"),
        ]
    );
    let imports = snapshot
        .query(QueryRequest {
            kind: Some(FactKind::Edge),
            labels: vec!["IMPORTS".into()],
            ..request.clone()
        })
        .unwrap();
    assert_eq!(references(&imports), vec![edge_ref("b", "IMPORTS", "c")]);
    for (hops, expected_nodes, expected_edges) in [(2, 4, 4), (3, 5, 5)] {
        let response = snapshot
            .query(QueryRequest {
                scope: QueryScope::Neighborhood {
                    anchor: "a".into(),
                    hops,
                },
                ..QueryRequest::default()
            })
            .unwrap();
        assert_eq!(
            response
                .facts
                .iter()
                .filter(|fact| matches!(fact.reference, FactReference::Node { .. }))
                .count(),
            expected_nodes
        );
        assert_eq!(
            response
                .facts
                .iter()
                .filter(|fact| matches!(fact.reference, FactReference::Edge { .. }))
                .count(),
            expected_edges
        );
        assert!(!references(&response).contains(&node_ref("outside")));
        assert!(!references(&response).contains(&edge_ref("outside", "CALLS", "a")));
    }
    assert!(
        references(&snapshot.query(QueryRequest::default()).unwrap())
            .contains(&edge_ref("outside", "CALLS", "a"))
    );
}

#[test]
fn unsupported_domain_labels_return_no_fabricated_facts() {
    // AC-0104: business-sounding paths are not evidence of recovered domains.
    let snapshot = ContextSnapshot::new(
        vec![
            node(
                "src/orders/cart.ts",
                "File",
                json!({"name": "Order domain"}),
            ),
            node("cart", "Symbol", json!({"name": "Cart feature"})),
        ],
        vec![],
    )
    .unwrap();
    for label in [
        "Domain",
        "Capability",
        "BusinessRule",
        "Actor",
        "symbol",
        "Symbol ",
    ] {
        let response = snapshot
            .query(QueryRequest {
                labels: vec![label.into()],
                ..QueryRequest::default()
            })
            .unwrap();
        assert_eq!(response.total_selected, 0);
        assert!(response.facts.is_empty());
        assert!(response.next_cursor.is_none());
    }
    let exact = snapshot
        .query(QueryRequest {
            kind: Some(FactKind::Node),
            labels: vec!["Symbol".into()],
            ..QueryRequest::default()
        })
        .unwrap();
    assert_eq!(references(&exact), vec![node_ref("cart")]);
}

#[test]
fn invalid_limits_and_missing_anchors_fail_explicitly() {
    // AC-0102 / AC-0104: limits and absent anchors fail even for empty selections.
    let snapshot = ContextSnapshot::new(vec![node("known", "Symbol", json!({}))], vec![]).unwrap();
    for (name, maximum) in [
        ("max_facts", MAX_FACTS),
        ("max_bytes", MAX_RESPONSE_BYTES),
        ("hops", MAX_NEIGHBORHOOD_HOPS),
    ] {
        for requested in [0, maximum + 1] {
            let mut request = QueryRequest::default();
            match name {
                "max_facts" => request.max_facts = requested,
                "max_bytes" => request.max_bytes = requested,
                _ => {
                    request.scope = QueryScope::Neighborhood {
                        anchor: "known".into(),
                        hops: requested,
                    }
                }
            }
            assert!(
                matches!(snapshot.query(request), Err(ContextError::InvalidLimit { name: actual_name, requested: actual_requested, maximum: actual_maximum })
                if actual_name == name && actual_requested == requested && actual_maximum == maximum)
            );
        }
    }
    assert!(matches!(snapshot.query(QueryRequest {
        scope: QueryScope::Neighborhood { anchor: "missing".into(), hops: 1 },
        labels: vec!["Capability".into()],
        ..QueryRequest::default()
    }), Err(ContextError::UnknownAnchor(anchor)) if anchor == "missing"));
}

#[test]
fn typed_requests_and_responses_round_trip_delimiter_containing_identities() {
    // AC-0101 / AC-0104: node/edge references remain distinct on the JSON wire.
    let nodes = ["a b", "d", "a", "c d", "a b c d", "quote\"📍\n"]
        .into_iter()
        .map(|id| node(id, "Symbol", json!({})))
        .collect();
    let snapshot = ContextSnapshot::new(
        nodes,
        vec![
            edge("a b", "c", "d", json!({})),
            edge("a", "b", "c d", json!({})),
        ],
    )
    .unwrap();
    let request = QueryRequest {
        max_facts: 1,
        ..QueryRequest::default()
    };
    let wire_request = serde_json::to_vec(&request).unwrap();
    assert_eq!(
        serde_json::from_slice::<QueryRequest>(&wire_request).unwrap(),
        request
    );
    let page = snapshot.query(request).unwrap();
    let continuation = QueryRequest {
        cursor: page.next_cursor,
        ..QueryRequest::default()
    };
    let round_trip: QueryRequest =
        serde_json::from_value(serde_json::to_value(&continuation).unwrap()).unwrap();
    assert_eq!(round_trip, continuation);
    let response = snapshot.query(round_trip).unwrap();
    let wire_response = serde_json::to_value(&response).unwrap();
    assert_eq!(wire_response["view"], "recovered_graph");
    assert_eq!(
        serde_json::from_value::<QueryResponse>(wire_response).unwrap(),
        response
    );
    let refs = references(&response);
    assert!(refs.contains(&node_ref("a b c d")));
    assert!(refs.contains(&node_ref("quote\"📍\n")));
    assert!(refs.contains(&edge_ref("a b", "c", "d")));
    assert!(refs.contains(&edge_ref("a", "b", "c d")));
    assert_eq!(refs.iter().collect::<BTreeSet<_>>().len(), refs.len());
    let neighborhood = QueryRequest {
        scope: QueryScope::Neighborhood {
            anchor: "quote\"📍\n".into(),
            hops: 1,
        },
        kind: Some(FactKind::Node),
        labels: vec!["Symbol".into()],
        ..QueryRequest::default()
    };
    assert_eq!(
        serde_json::from_value::<QueryRequest>(serde_json::to_value(&neighborhood).unwrap())
            .unwrap(),
        neighborhood
    );
}
