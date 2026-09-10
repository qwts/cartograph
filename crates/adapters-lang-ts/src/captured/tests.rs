use super::*;
use core_graph::source::FactKey;
use source_capture::{CaptureLimits, SourceId as CaptureSourceId, capture_working_tree};
use std::fs;

const SOURCE: &str = "src_11111111111111111111111111111111";
const REPO: &str = "local/src_11111111111111111111111111111111";

fn parser_id() -> SourceId<'static> {
    SourceId {
        repo: REPO,
        commit: "workdir",
    }
}

fn capture(root: &Path, paths: &[String]) -> Capture {
    capture_working_tree(
        root,
        &CaptureSourceId::new(SOURCE).unwrap(),
        paths,
        CaptureLimits::default(),
    )
    .unwrap()
}

fn fixture(source: &[u8], path: &str) -> (tempfile::TempDir, Capture) {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, source).unwrap();
    let captured = capture(directory.path(), &[path.into()]);
    (directory, captured)
}

fn rule_receipt(receipts: &[Receipt]) -> &Receipt {
    receipts
        .iter()
        .find(
            |receipt| matches!(receipt.fact_key(), FactKey::Node { id } if id.starts_with("rule:")),
        )
        .unwrap()
}

#[test]
fn captured_parser_uses_original_bytes_after_target_mutation() {
    // AC-0148: capture is the actual producer input, not a later file lookup.
    let source = b"function ready(enabled: boolean) { if (enabled) return false; }\n// retained-only-canary\n";
    let (directory, capture) = fixture(source, "source.ts");
    fs::write(
        directory.path().join("source.ts"),
        b"function changed() { return true; }",
    )
    .unwrap();
    let (facts, receipts) = extract_file(capture.file("source.ts").unwrap(), &parser_id()).unwrap();
    let ordinary = crate::extract_source(source, "source.ts", &parser_id()).unwrap();
    assert_eq!(facts.nodes, ordinary.nodes);
    assert_eq!(facts.edges, ordinary.edges);
    assert_eq!(receipts.len(), 3, "rule, actual owner, and GOVERNS only");
    fs::remove_file(directory.path().join("source.ts")).unwrap();
    for receipt in &receipts {
        assert_eq!(receipt.source_id().as_str(), SOURCE);
        assert_eq!(receipt.repo_key(), REPO);
        for range in receipt.ranges() {
            assert_eq!(
                capture.read_span(&range.captured).unwrap(),
                &source[range.evidence.byte_start as usize..range.evidence.byte_end as usize]
            );
        }
        let wire = receipt.to_json().unwrap();
        assert!(!wire.contains("retained-only-canary"));
        assert!(!format!("{receipt:?}").contains("retained-only-canary"));
    }
    assert!(
        facts
            .nodes
            .iter()
            .all(|node| node.props.get("receipt_id").is_none())
    );
    let rule = rule_receipt(&receipts);
    assert!(
        capture
            .read_text_span(&rule.ranges()[0].captured)
            .unwrap()
            .contains("return false")
    );
}

#[test]
fn equal_complete_facts_keep_distinct_raw_capture_receipts() {
    // AC-0149: even full fact equality cannot select the producing raw input.
    let before = b"function stable(ready: boolean) { if (ready) return false; }\n// comment-A\n";
    let after = b"function stable(ready: boolean) { if (ready) return false; }\n// comment-B\n";
    assert_eq!(before.len(), after.len());
    let (directory, first) = fixture(before, "source.ts");
    fs::write(directory.path().join("source.ts"), after).unwrap();
    let second = capture(directory.path(), &["source.ts".into()]);
    let (first_facts, first_receipts) =
        extract_file(first.file("source.ts").unwrap(), &parser_id()).unwrap();
    let (second_facts, second_receipts) =
        extract_file(second.file("source.ts").unwrap(), &parser_id()).unwrap();
    assert_eq!(first_facts.nodes, second_facts.nodes);
    assert_eq!(first_facts.edges, second_facts.edges);
    assert_ne!(first.id(), second.id());
    for (a, b) in first_receipts.iter().zip(&second_receipts) {
        assert_eq!(a.fact_key(), b.fact_key());
        assert_eq!(a.fact_digest(), b.fact_digest());
        assert_ne!(a.id(), b.id());
        assert_ne!(a.file().digest, b.file().digest);
    }
}

#[test]
fn captured_rule_inventory_binds_every_nested_source_and_redaction() {
    // AC-0149: duplicate and nested citations retain their structural roles;
    // literals and source text never enter the metadata-only receipt.
    let source = br#"function checkout(enabled: boolean, quantity: number) {
  const minimum = 1;
  if (enabled) {
    if (quantity < minimum) return {password: "tiny-secret-canary"};
    throw new Error("business outcome");
  }
}"#;
    let (_, capture) = fixture(source, "source.ts");
    let (facts, receipts) = extract_file(capture.file("source.ts").unwrap(), &parser_id()).unwrap();
    let mut all_roles = std::collections::BTreeSet::new();
    for receipt in &receipts {
        for range in receipt.ranges() {
            all_roles.insert(range.role);
            assert_eq!(range.captured.file, *receipt.file());
            assert_eq!(
                capture.read_span(&range.captured).unwrap(),
                &source[range.evidence.byte_start as usize..range.evidence.byte_end as usize]
            );
        }
        if let FactKey::Node { id } = receipt.fact_key() {
            let node = facts.nodes.iter().find(|node| &node.id == id).unwrap();
            if let Some(value) = node.props.get("rule") {
                let mut rule =
                    core_graph::rules::GuardedExitEvidence::from_value(value.clone()).unwrap();
                let mut nested = Vec::new();
                rule.visit_sources_mut(|source| nested.push(source.clone()));
                let retained: Vec<_> = receipt
                    .ranges()
                    .iter()
                    .filter(|range| range.role != RangeRole::Provenance)
                    .map(|range| range.evidence.clone())
                    .collect();
                assert_eq!(retained, nested);
            }
        }
        let wire = receipt.to_json().unwrap();
        assert!(!wire.contains("tiny-secret-canary"));
        assert!(!wire.contains("business outcome"));
        assert!(wire.contains("primary_source_only"));
        assert!(wire.contains("input_closure_not_established"));
        assert_eq!(Receipt::from_json(&wire).unwrap(), *receipt);
    }
    for role in [
        RangeRole::ConditionBranch,
        RangeRole::ConditionExpression,
        RangeRole::ReturnValue,
        RangeRole::ThrowValue,
        RangeRole::DependencyDeclaration,
        RangeRole::Redaction,
    ] {
        assert!(all_roles.contains(&role), "missing inventory role {role:?}");
    }
}

#[test]
fn captured_directory_preserves_completion_but_excludes_eval_and_derived_facts() {
    // AC-0148: the common directory pass is reused; participation is granted
    // only by direct emission, never by matching synthetic or derived metadata.
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("pages")).unwrap();
    fs::write(
        directory.path().join("pages/index.tsx"),
        br#"export default function Page(ready: boolean) {
  if (ready) return null;
  eval("function synthetic(ok) { if (ok) return false; }");
  return <div/>;
}"#,
    )
    .unwrap();
    let paths = enumerate_paths(directory.path()).unwrap();
    let capture = capture(directory.path(), &paths);
    let ordinary = crate::extract_dir(directory.path(), &parser_id()).unwrap();
    let mut visited = Vec::new();
    let (captured, receipts, stats) =
        extract_captured_dir(directory.path(), &parser_id(), &capture, &mut |path| {
            visited.push(path.to_string())
        })
        .unwrap();
    assert_eq!(ordinary.nodes, captured.nodes);
    assert_eq!(ordinary.edges, captured.edges);
    assert_eq!(visited, paths);
    assert_eq!(stats.recomputed_files, 1);
    assert_eq!(stats.reused_files, 0);
    assert!(
        captured
            .nodes
            .iter()
            .any(|node| node.props["via"] == "eval")
    );
    assert!(captured.nodes.iter().any(|node| node.label == "Screen"));
    assert_eq!(receipts.len(), 3);
    for receipt in &receipts {
        match receipt.fact_key() {
            FactKey::Node { id } => {
                let node = captured.nodes.iter().find(|node| &node.id == id).unwrap();
                assert!(node.props.get("via").is_none());
                assert!(matches!(node.label.as_str(), "BusinessRule" | "Component"));
            }
            FactKey::Edge { label, .. } => assert_eq!(label, "GOVERNS"),
        }
    }
    // The retained primary files are parsed on every captured invocation.
    fs::write(
        directory.path().join("pages/index.tsx"),
        b"export default 7;",
    )
    .unwrap();
    let (again, again_receipts, stats) =
        extract_captured_dir(directory.path(), &parser_id(), &capture, &mut |_| {}).unwrap();
    assert_eq!(captured.nodes, again.nodes);
    assert_eq!(captured.edges, again.edges);
    assert_eq!(receipts, again_receipts);
    assert_eq!(stats.recomputed_files, 1);
    assert_eq!(stats.reused_files, 0);
}

#[test]
fn receipt_grammar_and_complete_fact_changes_are_explicit() {
    // AC-0149: package/grammar and every property/provenance field are bound;
    // later enrichment loses the receipt instead of relabeling it in place.
    for (path, grammar) in [
        ("source.ts", Grammar::TypeScript),
        ("source.js", Grammar::Tsx),
        ("source.tsx", Grammar::Tsx),
    ] {
        let (_, capture) = fixture(
            b"function ready(ok: boolean) { if (ok) return false; }",
            path,
        );
        let (mut facts, mut receipts) =
            extract_file(capture.file(path).unwrap(), &parser_id()).unwrap();
        assert!(receipts.iter().all(|receipt| receipt.grammar() == grammar));
        let original = rule_receipt(&receipts).clone();
        let node = facts
            .nodes
            .iter_mut()
            .find(|node| original.fact_key() == &FactKey::from_node(node))
            .unwrap();
        assert!(original.matches_node(node));
        node.props["extra_enrichment"] = serde_json::json!(true);
        assert!(!original.matches_node(node));
        retain_matching(&mut receipts, &facts);
        assert!(!receipts.iter().any(|receipt| receipt.id() == original.id()));
        let mut changed_revision = facts
            .nodes
            .iter()
            .find(|node| node.label == "Symbol")
            .unwrap()
            .clone();
        let owner = receipts
            .iter()
            .find(|receipt| receipt.fact_key() == &FactKey::from_node(&changed_revision))
            .unwrap();
        changed_revision.props["prov"]["evidence"][0]["commit_sha"] =
            serde_json::json!("new-revision");
        assert!(!owner.matches_node(&changed_revision));
    }
}

#[test]
fn live_config_changes_remain_outside_primary_receipt_closure() {
    // AC-0148/0149: directory configuration is intentionally not frozen by a
    // primary-file receipt; changed resolution must not gain that attestation.
    let directory = tempfile::tempdir().unwrap();
    for name in ["one.ts", "two.ts"] {
        fs::write(
            directory.path().join(name),
            b"export function target() {}\n",
        )
        .unwrap();
    }
    fs::write(directory.path().join("main.ts"), b"import { target } from '@lib';\nexport function run(ok: boolean) { if (ok) return false; return target(); }\n").unwrap();
    let paths = enumerate_paths(directory.path()).unwrap();
    let capture = capture(directory.path(), &paths);
    let run = |target: &str| {
        fs::write(
            directory.path().join("tsconfig.json"),
            serde_json::json!({"compilerOptions":{"baseUrl":".","paths":{"@lib":[target]}}})
                .to_string(),
        )
        .unwrap();
        extract_captured_dir(directory.path(), &parser_id(), &capture, &mut |_| {}).unwrap()
    };
    let (first, first_receipts, _) = run("one.ts");
    let (second, second_receipts, _) = run("two.ts");
    let first_import = first
        .edges
        .iter()
        .find(|edge| edge.label == "IMPORTS" && edge.src.ends_with("@main.ts"))
        .unwrap();
    let second_import = second
        .edges
        .iter()
        .find(|edge| edge.label == "IMPORTS" && edge.src.ends_with("@main.ts"))
        .unwrap();
    assert!(first_import.dst.ends_with("@one.ts"));
    assert!(second_import.dst.ends_with("@two.ts"));
    assert_eq!(first_receipts, second_receipts);
    assert!(
        !first_receipts
            .iter()
            .any(|receipt| receipt.fact_key() == &FactKey::from_edge(first_import))
    );
    assert!(
        first_receipts
            .iter()
            .all(|receipt| receipt.file().path == "main.ts")
    );
}

#[test]
fn captured_receipts_reject_tampering_versions_and_bounds() {
    // AC-0149: unsupported contracts and unbounded/malformed records fail with
    // fixed diagnostics; public decoding is integrity validation, not trust.
    let (_, capture) = fixture(
        b"function ready(ok: boolean) { if (ok) return false; }",
        "source.ts",
    );
    let (_, receipts) = extract_file(capture.file("source.ts").unwrap(), &parser_id()).unwrap();
    let receipt = rule_receipt(&receipts);
    let value = serde_json::to_value(receipt).unwrap();
    for pointer in [
        "/content/schema_version",
        "/content/file/digest",
        "/content/repo_key",
        "/content/grammar_package",
        "/content/ranges/0/evidence/byte_end",
        "/receipt_id",
    ] {
        let mut tampered = value.clone();
        *tampered.pointer_mut(pointer).unwrap() =
            serde_json::json!("untrusted-source-value-canary");
        let message = Receipt::from_json(&tampered.to_string())
            .unwrap_err()
            .to_string();
        assert!(!message.contains("untrusted-source-value-canary"));
        assert!(serde_json::from_value::<Receipt>(tampered).is_err());
    }
    let mut extra = value.clone();
    extra["unexpected"] = serde_json::json!(true);
    assert!(Receipt::from_json(&extra.to_string()).is_err());
    let mut nested_extra = value.clone();
    nested_extra["content"]["ranges"][0]["evidence"]["source-canary"] =
        serde_json::json!("raw-source-canary");
    assert!(
        !serde_json::from_value::<Receipt>(nested_extra.clone())
            .unwrap_err()
            .to_string()
            .contains("source-canary")
    );
    assert!(Receipt::from_json(&nested_extra.to_string()).is_err());
    assert!(Receipt::from_json(&" ".repeat(MAX_RECEIPT_BYTES + 1)).is_err());
    let wrong_source = SourceId {
        repo: "local/src_22222222222222222222222222222222",
        commit: "workdir",
    };
    assert!(extract_file(capture.file("source.ts").unwrap(), &wrong_source).is_err());
}

#[test]
fn oversized_primary_ranges_remain_without_truncated_receipts() {
    // AC-0148/0149: a huge owner span remains an ordinary fact, without a
    // shortened citation or receipt. Its bounded local exit still participates.
    let source = format!(
        "function large(ok: boolean) {{ /*{}*/ if (ok) return false; }}",
        "x".repeat(source_capture::MAX_SPAN_BYTES as usize)
    );
    let (_, capture) = fixture(source.as_bytes(), "source.ts");
    let (facts, receipts) = extract_file(capture.file("source.ts").unwrap(), &parser_id()).unwrap();
    let owner = facts
        .nodes
        .iter()
        .find(|node| node.label == "Symbol")
        .unwrap();
    assert!(
        !receipts
            .iter()
            .any(|receipt| receipt.fact_key() == &FactKey::from_node(owner))
    );
    assert!(receipts.iter().any(
        |receipt| matches!(receipt.fact_key(), FactKey::Edge { label, .. } if label == "GOVERNS")
    ));
    assert!(receipts.iter().all(|receipt| {
        receipt.ranges().iter().all(|range| {
            range.evidence.byte_end - range.evidence.byte_start <= source_capture::MAX_SPAN_BYTES
        })
    }));
}

#[test]
fn captured_enumeration_preserves_selection_and_enforces_budgets() {
    // AC-0148: all visited entries count toward the walk budget; supported
    // primary membership is exact, sorted, bounded, and never silently partial.
    let directory = tempfile::tempdir().unwrap();
    for path in [
        "src/z.js",
        "src/a.ts",
        "src/c.tsx",
        "types.d.ts",
        "notes.txt",
        "node_modules/vendor.ts",
        "dist/generated.ts",
        ".git/hidden.ts",
        "src/.hidden/ignored.ts",
        "src/.visible.ts",
    ] {
        let file = directory.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, b"").unwrap();
    }
    assert_eq!(
        enumerate_paths(directory.path()).unwrap(),
        ["src/.visible.ts", "src/a.ts", "src/c.tsx", "src/z.js"]
    );
    let root = Dir::open_ambient_dir(directory.path(), cap_std::ambient_authority()).unwrap();
    assert!(matches!(
        enumerate_with_limits(&root, 0, 64, 8192),
        Err(CapturedError::Limit("visited entries"))
    ));
    assert!(matches!(
        enumerate_with_limits(&root, 100, 0, 8192),
        Err(CapturedError::Limit("directory depth"))
    ));
    assert!(matches!(
        enumerate_with_limits(&root, 100, 64, 1),
        Err(CapturedError::Limit("selected files"))
    ));
}

#[cfg(unix)]
#[test]
fn captured_enumeration_rejects_symlinks_and_non_utf8_paths() {
    // AC-0148: no-follow handles prevent traversal into a substituted directory;
    // lossy path conversion may never collapse two primary source identities.
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("hidden.ts"), b"external source").unwrap();
    symlink(outside.path(), directory.path().join("linked")).unwrap();
    assert!(enumerate_paths(directory.path()).is_err());
    fs::remove_file(directory.path().join("linked")).unwrap();
    symlink(
        outside.path().join("hidden.ts"),
        directory.path().join("source.ts"),
    )
    .unwrap();
    assert!(enumerate_paths(directory.path()).is_err());
    fs::remove_file(directory.path().join("source.ts")).unwrap();
    // macOS rejects creation of this filename with EILSEQ. Linux permits the
    // raw directory entry, so it exercises the adapter's exact-UTF-8 rejection.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStringExt;
        fs::write(
            directory
                .path()
                .join(std::ffi::OsString::from_vec(b"invalid-\xff.ts".to_vec())),
            b"",
        )
        .unwrap();
        assert!(enumerate_paths(directory.path()).is_err());
    }
    assert_eq!(
        fs::read(outside.path().join("hidden.ts")).unwrap(),
        b"external source"
    );
}
