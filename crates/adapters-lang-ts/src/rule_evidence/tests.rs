use super::*;
use crate::{IncrementalCache, SourceId, extract_dir_incremental, extract_source};

fn recover(source: &str) -> Extraction {
    extract_source(
        source.as_bytes(),
        "src/rules.ts",
        &SourceId {
            repo: "fixture",
            commit: "revision-a",
        },
    )
    .unwrap()
}

fn rules(out: &Extraction) -> Vec<(&Node, GuardedExitEvidence)> {
    out.nodes
        .iter()
        .filter(|node| node.label == "BusinessRule")
        .map(|node| {
            (
                node,
                GuardedExitEvidence::from_value(node.props["rule"].clone()).unwrap(),
            )
        })
        .collect()
}

#[test]
fn guarded_exits_preserve_same_callable_branches_and_local_effects() {
    // AC-0122: outer creation conditions never become nested callback predicates.
    let code = r#"
function create(enabled: boolean) {
  if (enabled) return {
    decide(ready: boolean, quantity: number) {
      if (ready !== false) {
        if (quantity > 0) return "proceed";
        else throw new Error("quantity required");
      } else return false;
    },
    other: (valid: boolean) => { if (valid) return null; return; }
  };
}
"#;
    let out = recover(code);
    let rules = rules(&out);
    assert_eq!(rules.len(), 5);
    let decide: Vec<_> = rules
        .iter()
        .filter(|(_, rule)| rule.owner_id.contains(".decide@"))
        .collect();
    assert_eq!(decide.len(), 3);
    let positive = &decide[0].1;
    assert_eq!(positive.conditions.len(), 2);
    assert_eq!(
        positive.conditions[0].expression.display.as_str(),
        "(ready !== false)"
    );
    assert_eq!(
        positive.conditions[1].expression.display.as_str(),
        "(quantity > 0)"
    );
    assert!(
        positive
            .conditions
            .iter()
            .all(|condition| condition.polarity == BranchPolarity::TruthyBranch)
    );
    assert_eq!(
        decide[1].1.conditions[1].polarity,
        BranchPolarity::FalsyBranch
    );
    assert!(matches!(decide[1].1.effect, LocalExit::Throw { .. }));
    assert_eq!(
        decide[2].1.conditions[0].polarity,
        BranchPolarity::FalsyBranch
    );
    assert!(
        matches!(&decide[2].1.effect, LocalExit::Return { value: Some(value) }
        if value.literal == Some(LiteralEvidence::Known { value: KnownLiteral::Boolean(false) }))
    );
    let owners: BTreeSet<_> = out
        .nodes
        .iter()
        .filter(|node| matches!(node.label.as_str(), "Symbol" | "Component"))
        .map(|node| &node.id)
        .collect();
    for (node, rule) in &rules {
        assert!(owners.contains(&rule.owner_id));
        assert!(out.edges.iter().any(|edge| edge.src == node.id
            && edge.dst == rule.owner_id
            && edge.label == "GOVERNS"));
        let original =
            &code[rule.exit_source.byte_start as usize..rule.exit_source.byte_end as usize];
        assert!(original.starts_with("return") || original.starts_with("throw"));
        assert_eq!(
            rule.interpretation.consumer_effect,
            InterpretationStatus::NotEstablished
        );
        assert_eq!(
            rule.interpretation.execution_predicate,
            InterpretationStatus::NotEstablished
        );
        for condition in &rule.conditions {
            assert_eq!(condition.expression.source.commit_sha, "revision-a");
        }
    }
}

#[test]
fn rule_dependencies_and_control_limits_are_explicit_cited_gaps() {
    // AC-0123: local call proof does not claim the call's behavioral result.
    let out = recover(
        r#"
function helper() { return true; }
function decide(items: number[], enabled: boolean) {
  let total = 0;
  for (const item of items) total += item;
  if (!enabled) return false;
  try { if (total > 0 && helper() && unknown(total)) return "continue"; }
  finally { total++; }
}
"#,
    );
    let rules = rules(&out);
    let (_, last) = rules.last().unwrap();
    let gaps: BTreeMap<_, _> = out
        .nodes
        .iter()
        .filter(|node| node.label == "Gap")
        .map(|node| (node.id.clone(), node))
        .collect();
    let reasons: BTreeSet<_> = last
        .interpretation
        .gap_ids
        .iter()
        .map(|id| gaps[id].props["reason_code"].as_str().unwrap())
        .collect();
    for expected in [
        "execution_predicate_unknown",
        "consumer_semantics_unknown",
        "unresolved_call",
        "unresolved_binding",
        "preceding_exit",
        "mutation",
        "loop_dependency",
        "exception_control",
    ] {
        assert!(reasons.contains(expected), "missing {expected}");
    }
    assert!(
        last.dependencies
            .iter()
            .any(|dependency| matches!(&dependency.resolution,
        DependencyResolution::Target { node_id } if node_id.ends_with("#helper")))
    );
    for (node, rule) in rules {
        for gap_id in rule.interpretation.gap_ids {
            let gap = gaps[&gap_id];
            let prov: core_prov::Provenance =
                serde_json::from_value(gap.props["prov"].clone()).unwrap();
            assert_eq!(prov.confidence_tier, ConfidenceTier::Gap);
            assert!(out.edges.iter().any(|edge| edge.src == node.id
                && edge.dst == gap_id
                && edge.label == "DEPENDS_ON"));
        }
    }
}

#[test]
fn excessive_rule_dependencies_stop_with_a_bounded_explicit_gap() {
    // AC-0123: a very wide predicate cannot silently omit dependencies.
    let condition = (0..500)
        .map(|index| format!("input{index}"))
        .collect::<Vec<_>>()
        .join(" && ");
    let out = recover(&format!(
        "function run() {{ if ({condition}) return false; }}"
    ));
    let rules = rules(&out);
    let rule = &rules[0].1;
    assert!(rule.dependencies.len() <= MAX_DEPENDENCIES + 1);
    assert!(
        out.nodes
            .iter()
            .any(|node| node.props["reason_code"] == "dependency_limit")
    );
    assert!(rule.interpretation.gap_ids.len() <= 10);
}

#[test]
fn nested_and_oversized_rule_capture_is_bounded_with_visible_omissions() {
    // AC-0123: nested guards and huge values must not amplify capture without bound.
    let deeply_nested = format!(
        "function run(x:boolean) {{ {} return false; {} }}",
        "if(x){".repeat(100),
        "}".repeat(100)
    );
    let out = recover(&deeply_nested);
    assert!(rules(&out).is_empty());
    assert_eq!(
        out.nodes
            .iter()
            .filter(|node| node.props["reason_code"] == "analysis_limit")
            .count(),
        1
    );

    let huge_value = format!(
        "function run(x:boolean) {{ if(x) return '{}'; }}",
        "synthetic ".repeat(2000)
    );
    let out = recover(&huge_value);
    assert_eq!(rules(&out).len(), 1);
    assert!(
        out.nodes
            .iter()
            .any(|node| node.props["reason_code"] == "analysis_limit")
    );
    assert!(serde_json::to_vec(&out.nodes).unwrap().len() < MAX_RULE_BYTES);
}

#[test]
fn source_rule_literals_are_sanitized_before_complete_graph_serialization() {
    // AC-0124: decoded tokens, short sensitive values, and comments never land
    // in any newly emitted rule property, diagnostic, or secret-only digest.
    let code = r#"function check(enabled: boolean) {
      if (enabled /* comment-canary-private */) return { password: "tiny", apiKey: "abc", token: "ghp_abcdefgh1234", escaped: "\x67hp_abcdefgh5678", allowed: false };
      if (!enabled) return false;
    }"#;
    let out = recover(code);
    let serialized = serde_json::to_string(&(&out.nodes, &out.edges)).unwrap();
    for canary in [
        "comment-canary-private",
        "tiny",
        "abc\"",
        "ghp_abcdefgh1234",
        "ghp_abcdefgh5678",
        "\\x67hp_abcdefgh5678",
    ] {
        assert!(
            !serialized.contains(canary),
            "stored source canary: {canary}"
        );
    }
    let rules = rules(&out);
    assert!(!rules[0].1.redactions.is_empty());
    assert!(rules.iter().any(
        |(_, rule)| matches!(&rule.effect, LocalExit::Return { value: Some(value) }
        if value.literal == Some(LiteralEvidence::Known { value: KnownLiteral::Boolean(false) }))
    ));
}

#[test]
fn eval_rules_defer_unmapped_spans_without_losing_existing_symbols() {
    // AC-0123: synthetic offsets must never masquerade as original source.
    let out = recover(r#"function run() { eval("function decide(x) { if (x) return false; }"); }"#);
    assert!(rules(&out).is_empty());
    assert!(
        out.nodes
            .iter()
            .any(|node| node.id.contains("#eval@") && node.id.ends_with(".decide"))
    );
    let gap = out
        .nodes
        .iter()
        .find(|node| node.props["reason_code"] == "eval_source_mapping_unknown")
        .unwrap();
    assert!(
        out.edges
            .iter()
            .any(|edge| edge.dst == gap.id && edge.label == "DEPENDS_ON")
    );
}

#[test]
fn cached_rule_evidence_matches_fresh_revision_in_every_nested_reference() {
    // AC-0121/AC-0122: incremental reuse must not leave stale nested citations.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("rules.ts"),
        r#"function decide(enabled: boolean) { if (enabled) return false; }"#,
    )
    .unwrap();
    let mut cache = IncrementalCache::default();
    let first_id = SourceId {
        repo: "fixture",
        commit: "revision-a",
    };
    let next_id = SourceId {
        repo: "fixture",
        commit: "revision-b",
    };
    extract_dir_incremental(dir.path(), &first_id, &mut cache).unwrap();
    let (cached, stats) = extract_dir_incremental(dir.path(), &next_id, &mut cache).unwrap();
    assert_eq!(stats.reused_files, 1);
    let (fresh, _) =
        extract_dir_incremental(dir.path(), &next_id, &mut IncrementalCache::default()).unwrap();
    assert_eq!(
        serde_json::to_value((&cached.nodes, &cached.edges)).unwrap(),
        serde_json::to_value((&fresh.nodes, &fresh.edges)).unwrap()
    );
    assert!(
        !serde_json::to_string(&cached.nodes)
            .unwrap()
            .contains("revision-a")
    );
    assert_eq!(rules(&cached).len(), 1);
}

#[test]
fn rule_hashes_track_sanitized_predicates_and_repeat_deterministically() {
    // AC-0121/AC-0122: unchanged positions alone cannot hide a changed operator.
    let first = recover("function run(x:number) { if (x > 0) return false; }");
    let changed = recover("function run(x:number) { if (x < 0) return false; }");
    let repeated = recover("function run(x:number) { if (x > 0) return false; }");
    assert_eq!(
        serde_json::to_value((&first.nodes, &first.edges)).unwrap(),
        serde_json::to_value((&repeated.nodes, &repeated.edges)).unwrap()
    );
    assert_ne!(
        rules(&first)[0].0.props["prov"]["content_hash"],
        rules(&changed)[0].0.props["prov"]["content_hash"]
    );
}

#[test]
fn exit_value_dependencies_keep_creation_work_and_exclude_deferred_execution() {
    // AC-0123: computed keys/decorators are creation-time prerequisites; method
    // bodies, parameter defaults and instance initializers execute later.
    let code = r#"
function key() { return 'run'; }
function decorate() { return () => {}; }
function deferred() { return 1; }
function staticWork() {}
function objectFactory(ok: boolean) {
  if (ok) return { [key()](key = deferred()) { deferred(); } };
}
function classFactory(ok: boolean) {
  if (ok) return class {
    [key()]() { deferred(); }
    @decorate() method(@decorate() value = deferred()) { deferred(); }
    [key()] = deferred();
    callback = () => deferred();
    static value = staticWork();
    static { staticWork(); }
  };
}
"#;
    let out = recover(code);
    let recovered = rules(&out);
    assert_eq!(recovered.len(), 2);
    for (_, rule) in &recovered {
        let targets: BTreeSet<_> = rule
            .dependencies
            .iter()
            .filter_map(|dependency| match &dependency.resolution {
                DependencyResolution::Target { node_id }
                    if dependency.role == DependencyRole::ExitValue =>
                {
                    Some(node_id.as_str())
                }
                _ => None,
            })
            .collect();
        assert!(targets.contains("sym:fixture@src/rules.ts#key"));
        assert!(!targets.contains("sym:fixture@src/rules.ts#deferred"));
        assert!(!targets.contains("sym:fixture@src/rules.ts#staticWork"));
        if rule.owner_id.ends_with("#classFactory") {
            assert!(targets.contains("sym:fixture@src/rules.ts#decorate"));
            let dependency_calls: Vec<_> = rule
                .dependencies
                .iter()
                .filter(|dependency| {
                    matches!(dependency.resolution, DependencyResolution::Target { .. })
                })
                .map(|dependency| {
                    &code
                        [dependency.source.byte_start as usize..dependency.source.byte_end as usize]
                })
                .collect();
            assert_eq!(
                dependency_calls
                    .iter()
                    .filter(|call| **call == "key()")
                    .count(),
                2
            );
            assert_eq!(
                dependency_calls
                    .iter()
                    .filter(|call| **call == "decorate()")
                    .count(),
                2
            );
            assert!(rule.dependencies.iter().any(|dependency| {
                dependency.role == DependencyRole::ExitValue
                    && matches!(
                        dependency.resolution,
                        DependencyResolution::Unresolved { .. }
                    )
                    && code
                        [dependency.source.byte_start as usize..dependency.source.byte_end as usize]
                        .starts_with("static {")
            }));
            assert!(rule.dependencies.iter().any(|dependency| {
                dependency.role == DependencyRole::ExitValue
                    && matches!(
                        dependency.resolution,
                        DependencyResolution::Unresolved { .. }
                    )
                    && &code
                        [dependency.source.byte_start as usize..dependency.source.byte_end as usize]
                        == "staticWork()"
            }));
        }
    }
}
