//! Found/recovered ADR facts and deterministic ADR/code drift (US-0013).

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier, content_hash};
use flowtracer::Flow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Deterministic extractor id for Markdown ADR/RFC files.
pub const FOUND_ADR_EXTRACTOR_ID: &str = "t0.adr-markdown";
/// Local semantic projection id for recovered decision drafts.
pub const RECOVERED_ADR_EXTRACTOR_ID: &str = "t2.adr-recovery";

/// ADR-related graph facts produced by extraction or compilation.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AdrFacts {
    /// Found/recovered ADR and Drift nodes.
    pub nodes: Vec<Node>,
    /// DECIDES/CONFLICTS/DRIFTS_FROM edges.
    pub edges: Vec<Edge>,
}

fn collect_adr_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if file_type.is_dir() {
            if name.starts_with('.') || matches!(name.as_str(), "node_modules" | "target" | "dist")
            {
                continue;
            }
            collect_adr_files(root, &path, out)?;
        } else if file_type.is_file() && name.ends_with(".md") {
            let relative = path.strip_prefix(root).expect("walk remains under root");
            let in_decision_dir = relative.components().any(|component| {
                matches!(
                    component
                        .as_os_str()
                        .to_string_lossy()
                        .to_ascii_lowercase()
                        .as_str(),
                    "adr" | "adrs" | "decision" | "decisions" | "rfc" | "rfcs"
                )
            });
            let named_decision = name.starts_with("adr-") || name.starts_with("rfc-");
            if in_decision_dir || named_decision {
                out.push(path);
            }
        }
    }
    Ok(())
}

fn heading(markdown: &str, fallback: &str) -> String {
    markdown
        .lines()
        .find_map(|line| line.strip_prefix("# ").map(str::trim))
        .filter(|title| !title.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn field(markdown: &str, name: &str) -> Option<String> {
    let needle = format!("{}:", name.to_ascii_lowercase());
    markdown.lines().find_map(|line| {
        let normalized = line
            .trim()
            .trim_start_matches(['-', '*', ' '])
            .replace("**", "");
        let lower = normalized.to_ascii_lowercase();
        lower
            .starts_with(&needle)
            .then(|| normalized[needle.len()..].trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn list_field(markdown: &str, name: &str) -> Vec<String> {
    field(markdown, name)
        .into_iter()
        .flat_map(|value| {
            value
                .split([',', ';'])
                .map(|item| {
                    item.trim()
                        .trim_matches(['`', '[', ']', '(', ')', ' '])
                        .to_string()
                })
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn field_target_span(markdown: &str, name: &str, target: &str) -> Option<(usize, usize)> {
    let needle = format!("{}:", name.to_ascii_lowercase());
    let mut offset = 0;
    for line in markdown.split_inclusive('\n') {
        let normalized = line
            .trim()
            .trim_start_matches(['-', '*', ' '])
            .replace("**", "");
        if normalized.to_ascii_lowercase().starts_with(&needle) {
            let start = line.find(target).map(|local| offset + local)?;
            return Some((start, start + target.len()));
        }
        offset += line.len();
    }
    None
}

fn inline_code(markdown: &str) -> Vec<(String, usize, usize)> {
    let bytes = markdown.as_bytes();
    let mut spans = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let Some(open) = markdown[cursor..].find('`').map(|offset| cursor + offset) else {
            break;
        };
        let Some(close) = markdown[open + 1..]
            .find('`')
            .map(|offset| open + 1 + offset)
        else {
            break;
        };
        if close > open + 1 {
            spans.push((markdown[open + 1..close].to_string(), open + 1, close));
        }
        cursor = close + 1;
    }
    spans
}

fn evidence(repo: &str, path: &str, commit: &str, start: usize, end: usize) -> EvidenceRef {
    EvidenceRef {
        repo: repo.into(),
        path: path.into(),
        byte_start: start as u64,
        byte_end: end as u64,
        commit_sha: commit.into(),
    }
}

/// Parse Markdown ADR/RFC files under `root` and confirm only governed target
/// ids that are explicitly declared with `Governs:` or cited in backticks.
pub fn extract_found_adrs(
    root: &Path,
    repo: &str,
    commit: &str,
    candidates: &[Node],
) -> std::io::Result<AdrFacts> {
    let mut files = Vec::new();
    collect_adr_files(root, root, &mut files)?;
    files.sort();
    let candidates: BTreeMap<&str, &Node> = candidates
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let mut facts = AdrFacts::default();
    for file in files {
        if file.metadata()?.len() > 1024 * 1024 {
            continue;
        }
        let markdown = std::fs::read_to_string(&file)?;
        let relative = file
            .strip_prefix(root)
            .expect("ADR file remains under root")
            .to_string_lossy()
            .replace('\\', "/");
        let title = heading(
            &markdown,
            file.file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("Untitled decision"),
        );
        let adr_id = format!("adr:{repo}@{relative}");
        let file_evidence = evidence(repo, &relative, commit, 0, markdown.len());
        let provenance = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![file_evidence.clone()],
            FOUND_ADR_EXTRACTOR_ID,
            serde_json::to_vec(&(&adr_id, &markdown))
                .expect("ADR identity serializes")
                .as_slice(),
        )
        .expect("Confirmed is within the deterministic confidence ceiling");
        let forbids = list_field(&markdown, "Forbids")
            .into_iter()
            .filter(|label| {
                label
                    .chars()
                    .all(|character| character.is_ascii_uppercase() || character == '_')
            })
            .collect::<Vec<_>>();
        facts.nodes.push(Node {
            id: adr_id.clone(),
            label: "ADR".into(),
            props: serde_json::json!({
                "title": title,
                "status": field(&markdown, "Status").unwrap_or_else(|| "Found".into()),
                "body": markdown,
                "path": relative,
                "origin": "found",
                "recovered": false,
                "forbids": forbids,
                "prov": provenance,
            }),
        });

        let mut mentions: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for target in list_field(&markdown, "Governs") {
            if candidates.contains_key(target.as_str())
                && let Some(span) = field_target_span(&markdown, "Governs", &target)
            {
                mentions.insert(target, span);
            }
        }
        for (target, start, end) in inline_code(&markdown) {
            if candidates.contains_key(target.as_str()) {
                mentions.entry(target).or_insert((start, end));
            }
        }
        for (target, (start, end)) in mentions {
            let edge_provenance = Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
                vec![evidence(repo, &relative, commit, start, end)],
                FOUND_ADR_EXTRACTOR_ID,
                format!("{adr_id} DECIDES {target}").as_bytes(),
            )
            .expect("Confirmed is within the deterministic confidence ceiling");
            facts.edges.push(Edge {
                src: adr_id.clone(),
                dst: target,
                label: "DECIDES".into(),
                props: serde_json::json!({"prov": edge_provenance}),
            });
        }
    }
    facts.nodes.sort_by(|left, right| left.id.cmp(&right.id));
    facts.edges.sort_by(|left, right| {
        (&left.src, &left.dst, &left.label).cmp(&(&right.src, &right.dst, &right.label))
    });
    Ok(facts)
}

fn typed_provenance(value: &serde_json::Value) -> Option<Provenance> {
    serde_json::from_value::<Provenance>(value["prov"].clone())
        .ok()
        .filter(|provenance| provenance.validate().is_ok())
}

fn combined_evidence<'a>(
    provenances: impl IntoIterator<Item = &'a Provenance>,
) -> Vec<EvidenceRef> {
    let mut evidence = provenances
        .into_iter()
        .flat_map(|provenance| provenance.evidence.clone())
        .collect::<Vec<_>>();
    evidence.sort_by(|left, right| {
        (
            &left.repo,
            &left.path,
            left.byte_start,
            left.byte_end,
            &left.commit_sha,
        )
            .cmp(&(
                &right.repo,
                &right.path,
                right.byte_start,
                right.byte_end,
                &right.commit_sha,
            ))
    });
    evidence.dedup();
    evidence
}

fn recovered_adrs(nodes: &[Node], edges: &[Edge]) -> AdrFacts {
    let governed: BTreeSet<&str> = edges
        .iter()
        .filter(|edge| edge.label == "DECIDES")
        .filter(|edge| {
            nodes.iter().any(|node| {
                node.id == edge.src
                    && node.label == "ADR"
                    && node.props["origin"].as_str() == Some("found")
            })
        })
        .map(|edge| edge.dst.as_str())
        .collect();
    let mut facts = AdrFacts::default();
    for channel in nodes.iter().filter(|node| node.label == "Channel") {
        if governed.contains(channel.id.as_str()) {
            continue;
        }
        let related = edges
            .iter()
            .filter(|edge| {
                edge.dst == channel.id
                    && matches!(edge.label.as_str(), "PUBLISHES" | "SUBSCRIBES" | "BACKS")
            })
            .filter_map(|edge| {
                typed_provenance(&edge.props)
                    .filter(|provenance| provenance.confidence_tier != ConfidenceTier::Gap)
                    .map(|provenance| (edge, provenance))
            })
            .collect::<Vec<_>>();
        if related.is_empty() {
            continue;
        }
        let confidence = if related
            .iter()
            .any(|(_, provenance)| provenance.confidence_tier == ConfidenceTier::InferredWeak)
        {
            ConfidenceTier::InferredWeak
        } else {
            ConfidenceTier::InferredStrong
        };
        let mut provenances = related
            .iter()
            .map(|(_, provenance)| provenance.clone())
            .collect::<Vec<_>>();
        if let Some(channel_provenance) = typed_provenance(&channel.props) {
            provenances.push(channel_provenance);
        }
        let cited = combined_evidence(provenances.iter());
        if cited.is_empty() {
            continue;
        }
        let mut edge_keys = related
            .iter()
            .map(|(edge, _)| (&edge.src, &edge.label, &edge.dst))
            .collect::<Vec<_>>();
        edge_keys.sort();
        let canonical = serde_json::to_vec(&("async-messaging", &channel.id, edge_keys))
            .expect("recovered ADR identity serializes");
        let hash = content_hash(&canonical);
        let adr_id = format!("adr:recovered:async:{}", &hash[..16]);
        let name = channel.props["identity"]
            .as_str()
            .or_else(|| channel.props["name"].as_str())
            .unwrap_or(&channel.id);
        let title = format!("Recovered: use {name} for asynchronous messaging");
        let provenance = Provenance::new(
            Tier::Semantic,
            confidence,
            cited.clone(),
            RECOVERED_ADR_EXTRACTOR_ID,
            &canonical,
        )
        .expect("recovered confidence is within the semantic confidence ceiling");
        facts.nodes.push(Node {
            id: adr_id.clone(),
            label: "ADR".into(),
            props: serde_json::json!({
                "title": title,
                "status": "Recovered / Inferred",
                "body": format!("The recovered graph uses channel `{}` for asynchronous messaging. This is a proposed decision, not a confirmed author statement.", channel.id),
                "origin": "recovered",
                "recovered": true,
                "prov": provenance,
            }),
        });
        let edge_provenance = Provenance::new(
            Tier::Semantic,
            confidence,
            cited,
            RECOVERED_ADR_EXTRACTOR_ID,
            format!("{adr_id} DECIDES {}", channel.id).as_bytes(),
        )
        .expect("recovered confidence is within the semantic confidence ceiling");
        facts.edges.push(Edge {
            src: adr_id,
            dst: channel.id.clone(),
            label: "DECIDES".into(),
            props: serde_json::json!({"prov": edge_provenance}),
        });
    }
    facts
}

fn drift_facts(nodes: &[Node], edges: &[Edge], flows: &[Flow]) -> AdrFacts {
    let by_id: BTreeMap<&str, &Node> = nodes.iter().map(|node| (node.id.as_str(), node)).collect();
    let mut facts = AdrFacts::default();
    for adr in nodes
        .iter()
        .filter(|node| node.label == "ADR" && node.props["origin"].as_str() == Some("found"))
    {
        let forbidden = adr.props["forbids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect::<BTreeSet<_>>();
        if forbidden.is_empty() {
            continue;
        }
        let governed = edges
            .iter()
            .filter(|edge| edge.label == "DECIDES" && edge.src == adr.id)
            .map(|edge| edge.dst.as_str())
            .collect::<BTreeSet<_>>();
        for edge in edges.iter().filter(|edge| {
            forbidden.contains(edge.label.as_str())
                && (governed.contains(edge.src.as_str()) || governed.contains(edge.dst.as_str()))
        }) {
            let Some(edge_provenance) = typed_provenance(&edge.props) else {
                continue;
            };
            if edge_provenance.confidence_tier == ConfidenceTier::Gap {
                continue;
            }
            let Some(adr_provenance) = typed_provenance(&adr.props) else {
                continue;
            };
            let cited = combined_evidence([&adr_provenance, &edge_provenance]);
            if cited.is_empty() {
                continue;
            }
            let canonical =
                serde_json::to_vec(&("adr-drift", &adr.id, &edge.src, &edge.label, &edge.dst))
                    .expect("Drift identity serializes");
            let hash = content_hash(&canonical);
            let drift_id = format!("drift:{}", &hash[..16]);
            let flow_triggers = flows
                .iter()
                .filter(|flow| {
                    flow.hops.iter().any(|hop| {
                        hop.src == edge.src && hop.label == edge.label && hop.dst == edge.dst
                    })
                })
                .map(|flow| flow.trigger.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let adr_title = adr.props["title"].as_str().unwrap_or(&adr.id);
            let title = format!(
                "{adr_title} forbids {} but graph contains {} → {}",
                edge.label, edge.src, edge.dst
            );
            let drift_tier = match edge_provenance.confidence_tier {
                ConfidenceTier::Confirmed => edge_provenance.tier,
                ConfidenceTier::InferredStrong | ConfidenceTier::InferredWeak => Tier::Semantic,
                ConfidenceTier::Gap => unreachable!("gap facts were filtered above"),
            };
            let provenance = Provenance::new(
                drift_tier,
                edge_provenance.confidence_tier,
                cited,
                match drift_tier {
                    Tier::Deterministic => "t0.adr-drift",
                    Tier::Dynamic => "t1.adr-drift",
                    Tier::Semantic => "t2.adr-drift",
                    Tier::Agentic => "t3.adr-drift",
                },
                &canonical,
            )
            .expect("Drift retains the offending fact confidence ceiling");
            facts.nodes.push(Node {
                id: drift_id.clone(),
                label: "Drift".into(),
                props: serde_json::json!({
                    "kind": "drift",
                    "title": title,
                    "adr_id": adr.id,
                    "offending_edge": format!("{} {} {}", edge.src, edge.label, edge.dst),
                    "flow_triggers": flow_triggers,
                    "prov": provenance,
                }),
            });
            for (label, target) in [
                ("DRIFTS_FROM", adr.id.as_str()),
                ("CONFLICTS", edge.dst.as_str()),
            ] {
                if by_id.contains_key(target) {
                    facts.edges.push(Edge {
                        src: drift_id.clone(),
                        dst: target.into(),
                        label: label.into(),
                        props: serde_json::json!({"prov": provenance}),
                    });
                }
            }
        }
    }
    facts
}

/// Add evidence-backed recovered ADR drafts and explicit ADR/code drift to a
/// disposable compilation projection. Confirmed graph input is never mutated.
pub fn derive_adr_facts(nodes: &[Node], edges: &[Edge], flows: &[Flow]) -> AdrFacts {
    let mut recovered = recovered_adrs(nodes, edges);
    let mut projected_nodes = nodes.to_vec();
    projected_nodes.extend(recovered.nodes.clone());
    let mut projected_edges = edges.to_vec();
    projected_edges.extend(recovered.edges.clone());
    let drift = drift_facts(&projected_nodes, &projected_edges, flows);
    recovered.nodes.extend(drift.nodes);
    recovered.edges.extend(drift.edges);
    recovered
        .nodes
        .sort_by(|left, right| left.id.cmp(&right.id));
    recovered.edges.sort_by(|left, right| {
        (&left.src, &left.dst, &left.label).cmp(&(&right.src, &right.dst, &right.label))
    });
    recovered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prov(path: &str, fact: &str) -> serde_json::Value {
        serde_json::to_value(
            Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
                vec![evidence("local/shop", path, "abc123", 0, 10)],
                "t0.test",
                fact.as_bytes(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn weak_prov(path: &str, fact: &str) -> serde_json::Value {
        serde_json::to_value(
            Provenance::new(
                Tier::Semantic,
                ConfidenceTier::InferredWeak,
                vec![evidence("local/shop", path, "abc123", 0, 10)],
                "t2.test",
                fact.as_bytes(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn found_adr_links_only_explicit_existing_targets() {
        // AC-0036 (T-0036): found Markdown decisions and governed targets are
        // Confirmed with exact file/span provenance.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs/adr")).unwrap();
        std::fs::write(
            dir.path().join("docs/adr/ADR-0001-orders.md"),
            "# Queue orders\n\nThe chan:orders identifier is discussed here.\n\n- **Status:** Accepted\n- **Governs:** `chan:orders`\n- **Forbids:** CALLS\n",
        )
        .unwrap();
        let targets = vec![Node {
            id: "chan:orders".into(),
            label: "Channel".into(),
            props: serde_json::json!({"prov": prov("orders.ts", "channel")}),
        }];
        let facts = extract_found_adrs(dir.path(), "local/shop", "abc123", &targets).unwrap();
        assert_eq!(facts.nodes.len(), 1);
        assert_eq!(facts.edges.len(), 1);
        assert_eq!(facts.edges[0].label, "DECIDES");
        assert_eq!(facts.edges[0].dst, "chan:orders");
        assert_eq!(facts.nodes[0].props["prov"]["confidence_tier"], "Confirmed");
        assert_eq!(facts.nodes[0].props["forbids"][0], "CALLS");
        let expected_start =
            std::fs::read_to_string(dir.path().join("docs/adr/ADR-0001-orders.md"))
                .unwrap()
                .find("`chan:orders`")
                .unwrap()
                + 1;
        assert_eq!(
            facts.edges[0].props["prov"]["evidence"][0]["byte_start"],
            expected_start as u64
        );
    }

    #[test]
    fn recovered_adrs_and_drift_are_cited_and_mapped() {
        // AC-0037/AC-0038 (T-0037/T-0038): recovered ADRs remain inferred;
        // explicit ADR/code conflicts name the offending edge and flow.
        let channel = Node {
            id: "chan:orders".into(),
            label: "Channel".into(),
            props: serde_json::json!({"identity": "orders", "prov": prov("publisher.ts", "channel")}),
        };
        let producer = Node {
            id: "sym:publish".into(),
            label: "Symbol".into(),
            props: serde_json::json!({"prov": prov("publisher.ts", "producer")}),
        };
        let found = Node {
            id: "adr:found:no-sync".into(),
            label: "ADR".into(),
            props: serde_json::json!({
                "title": "No synchronous calls",
                "origin": "found",
                "forbids": ["CALLS"],
                "prov": prov("docs/adr/no-sync.md", "adr")
            }),
        };
        let target = Node {
            id: "sym:handler".into(),
            label: "Symbol".into(),
            props: serde_json::json!({"prov": prov("handler.ts", "handler")}),
        };
        let callee = Node {
            id: "sym:remote".into(),
            label: "Symbol".into(),
            props: serde_json::json!({"prov": prov("remote.ts", "remote")}),
        };
        let edges = vec![
            Edge {
                src: producer.id.clone(),
                dst: channel.id.clone(),
                label: "PUBLISHES".into(),
                props: serde_json::json!({"prov": prov("publisher.ts", "publishes")}),
            },
            Edge {
                src: found.id.clone(),
                dst: target.id.clone(),
                label: "DECIDES".into(),
                props: serde_json::json!({"prov": prov("docs/adr/no-sync.md", "decides")}),
            },
            Edge {
                src: target.id.clone(),
                dst: callee.id.clone(),
                label: "CALLS".into(),
                props: serde_json::json!({"prov": prov("handler.ts", "calls")}),
            },
        ];
        let flow = Flow {
            trigger: "ep:orders".into(),
            trigger_kind: "Endpoint".into(),
            trigger_name: "POST /orders".into(),
            hops: vec![flowtracer::Hop {
                label: "CALLS".into(),
                src: target.id.clone(),
                dst: callee.id.clone(),
                src_name: "handler".into(),
                dst_name: "remote".into(),
                tier: "Deterministic".into(),
                confidence: "Confirmed".into(),
                evidence: Some("handler.ts bytes 0..10".into()),
                provenance: serde_json::from_value(prov("handler.ts", "calls")).unwrap(),
                gap_reason: None,
                attempted_tiers: vec![],
            }],
            status: flowtracer::FlowStatus::Verified,
            score: 1.0,
            depth_limited: false,
        };
        let facts = derive_adr_facts(&[channel, producer, found, target, callee], &edges, &[flow]);
        let recovered = facts
            .nodes
            .iter()
            .find(|node| node.props["origin"] == "recovered")
            .unwrap();
        assert_eq!(recovered.props["prov"]["tier"], "Semantic");
        assert_eq!(recovered.props["prov"]["confidence_tier"], "InferredStrong");
        let drift = facts
            .nodes
            .iter()
            .find(|node| node.label == "Drift")
            .unwrap();
        assert_eq!(
            drift.props["offending_edge"],
            "sym:handler CALLS sym:remote"
        );
        assert_eq!(drift.props["flow_triggers"][0], "ep:orders");
        assert!(
            facts
                .edges
                .iter()
                .any(|edge| edge.src == drift.id && edge.label == "CONFLICTS")
        );
    }

    #[test]
    fn recovered_adr_never_upgrades_weak_support() {
        // AC-0037 (T-0037): a recovered decision inherits the weakest
        // supporting confidence instead of making an inferred edge stronger.
        let channel = Node {
            id: "chan:orders".into(),
            label: "Channel".into(),
            props: serde_json::json!({"prov": prov("orders.ts", "channel")}),
        };
        let producer = Node {
            id: "sym:publish".into(),
            label: "Symbol".into(),
            props: serde_json::json!({"prov": prov("orders.ts", "producer")}),
        };
        let edge = Edge {
            src: producer.id.clone(),
            dst: channel.id.clone(),
            label: "PUBLISHES".into(),
            props: serde_json::json!({"prov": weak_prov("orders.ts", "publishes")}),
        };

        let facts = derive_adr_facts(&[channel, producer], &[edge], &[]);
        let recovered = facts
            .nodes
            .iter()
            .find(|node| node.props["origin"] == "recovered")
            .unwrap();
        assert_eq!(recovered.props["prov"]["tier"], "Semantic");
        assert_eq!(recovered.props["prov"]["confidence_tier"], "InferredWeak");
        assert!(
            facts
                .edges
                .iter()
                .all(|edge| { edge.props["prov"]["confidence_tier"] == "InferredWeak" })
        );
    }
}
