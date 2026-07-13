//! Deterministic security projection over explicit auth and IAM facts (US-0015).

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, Provenance, Tier};

const SECURITY_EXTRACTOR_T0: &str = "t0.security-projection";
const SECURITY_EXTRACTOR_T1: &str = "t1.security-projection";
const SECURITY_EXTRACTOR_T2: &str = "t2.security-projection";

fn typed_provenance(props: &serde_json::Value) -> Option<Provenance> {
    serde_json::from_value::<Provenance>(props["prov"].clone())
        .ok()
        .filter(|provenance| provenance.validate().is_ok())
}

fn explicit_auth_state(node: &Node) -> Option<bool> {
    node.props["authenticated"]
        .as_bool()
        .or_else(|| node.props["auth"]["authenticated"].as_bool())
        .or_else(|| {
            let state = node.props["auth"].as_str()?.to_ascii_lowercase();
            match state.as_str() {
                "none" | "public" | "unauthenticated" => Some(false),
                "required" | "authenticated" | "protected" => Some(true),
                _ => None,
            }
        })
}

fn strings(value: &serde_json::Value) -> Vec<String> {
    let mut values = value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect::<Vec<_>>();
    if let Some(value) = value.as_str() {
        values.push(value.to_string());
    }
    values.sort();
    values.dedup();
    values
}

fn finding_provenance(support: &Provenance, canonical: &[u8]) -> Option<Provenance> {
    if support.confidence_tier == ConfidenceTier::Gap || support.evidence.is_empty() {
        return None;
    }
    let tier = match support.confidence_tier {
        ConfidenceTier::Confirmed => support.tier,
        ConfidenceTier::InferredStrong | ConfidenceTier::InferredWeak => Tier::Semantic,
        ConfidenceTier::Gap => unreachable!("gap support was filtered above"),
    };
    let extractor = match tier {
        Tier::Deterministic => SECURITY_EXTRACTOR_T0,
        Tier::Dynamic => SECURITY_EXTRACTOR_T1,
        Tier::Semantic | Tier::Agentic => SECURITY_EXTRACTOR_T2,
    };
    Provenance::new(
        tier,
        support.confidence_tier,
        support.evidence.clone(),
        extractor,
        canonical,
    )
    .ok()
}

fn endpoint_name(node: &Node) -> String {
    match (node.props["method"].as_str(), node.props["path"].as_str()) {
        (Some(method), Some(path)) => format!("{method} {path}"),
        _ => node.id.clone(),
    }
}

fn finding_node(
    provenance: Provenance,
    title: String,
    category: &str,
    ac_id: &str,
    subject_id: String,
    resource_scope: Vec<String>,
    actions: Vec<String>,
) -> Node {
    let id = format!("finding:security:{}", &provenance.content_hash[..16]);
    Node {
        id,
        label: "Finding".into(),
        props: serde_json::json!({
            "kind": "security",
            "category": category,
            "severity": "high",
            "title": title,
            "subject_id": subject_id,
            "resource_scope": resource_scope,
            "actions": actions,
            "us_id": "US-0015",
            "ac_id": ac_id,
            "prov": provenance,
        }),
    }
}

/// Derive security findings without mutating graph input. Endpoint findings
/// require an explicit negative auth fact; absence alone is not evidence.
pub(crate) fn derive_security_findings(nodes: &[Node], edges: &[Edge]) -> Vec<Node> {
    let mut findings = Vec::new();

    for endpoint in nodes
        .iter()
        .filter(|node| node.label == "Endpoint" && explicit_auth_state(node) == Some(false))
    {
        let Some(support) = typed_provenance(&endpoint.props) else {
            continue;
        };
        let name = endpoint_name(endpoint);
        let canonical = serde_json::to_vec(&("unauthenticated-endpoint", &endpoint.id, &name))
            .expect("security finding identity serializes");
        let Some(provenance) = finding_provenance(&support, &canonical) else {
            continue;
        };
        findings.push(finding_node(
            provenance,
            format!("Unauthenticated endpoint: {name}"),
            "unauthenticated_endpoint",
            "AC-0041",
            endpoint.id.clone(),
            vec![name],
            vec![],
        ));
    }

    for grant in edges.iter().filter(|edge| edge.label == "GRANTS") {
        let actions = strings(&grant.props["actions"]);
        let mut scopes = strings(&grant.props["resource_scopes"]);
        if scopes.is_empty() {
            scopes.push(grant.dst.clone());
        }
        let wildcard_action = actions.iter().any(|action| action.contains('*'));
        let wildcard_scope = scopes.iter().any(|scope| scope.contains('*'));
        if !wildcard_action && !wildcard_scope {
            continue;
        }
        let Some(support) = typed_provenance(&grant.props) else {
            continue;
        };
        let canonical = serde_json::to_vec(&(
            "over-broad-grant",
            &grant.src,
            &grant.dst,
            &actions,
            &scopes,
        ))
        .expect("security finding identity serializes");
        let Some(provenance) = finding_provenance(&support, &canonical) else {
            continue;
        };
        findings.push(finding_node(
            provenance,
            format!(
                "Over-broad IAM grant: {} → {}",
                grant.src,
                scopes.join(", ")
            ),
            "over_broad_grant",
            "AC-0042",
            format!("{} GRANTS {}", grant.src, grant.dst),
            scopes,
            actions,
        ));
    }

    findings.sort_by(|left, right| left.id.cmp(&right.id));
    findings.dedup_by(|left, right| left.id == right.id);
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_prov::{EvidenceRef, Provenance};

    fn prov(fact: &str) -> serde_json::Value {
        serde_json::to_value(
            Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
                vec![EvidenceRef {
                    repo: "local/shop".into(),
                    path: "src.tf".into(),
                    byte_start: 0,
                    byte_end: 20,
                    commit_sha: "abc123".into(),
                }],
                "t0.test",
                fact.as_bytes(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn explicit_auth_and_wildcard_grants_map_to_security_findings() {
        // AC-0041/AC-0042 (T-0041/T-0042): only explicit negative auth and
        // deterministic wildcard grants become mapped, cited findings.
        let nodes = vec![
            Node {
                id: "ep:admin".into(),
                label: "Endpoint".into(),
                props: serde_json::json!({
                    "method": "GET",
                    "path": "/admin",
                    "auth": "none",
                    "prov": prov("admin endpoint"),
                }),
            },
            Node {
                id: "ep:unknown".into(),
                label: "Endpoint".into(),
                props: serde_json::json!({
                    "method": "GET",
                    "path": "/unknown",
                    "prov": prov("unknown auth endpoint"),
                }),
            },
            Node {
                id: "ep:uncited".into(),
                label: "Endpoint".into(),
                props: serde_json::json!({
                    "method": "GET",
                    "path": "/uncited",
                    "authenticated": false,
                    "prov": Provenance::new(
                        Tier::Deterministic,
                        ConfidenceTier::Confirmed,
                        vec![],
                        "t0.test",
                        b"uncited endpoint",
                    ).unwrap(),
                }),
            },
        ];
        let edges = vec![Edge {
            src: "res:policy".into(),
            dst: "res:bucket".into(),
            label: "GRANTS".into(),
            props: serde_json::json!({
                "actions": ["s3:Get*"],
                "resource_scopes": ["arn:aws:s3:::orders/*"],
                "prov": prov("wildcard grant"),
            }),
        }];

        let findings = derive_security_findings(&nodes, &edges);
        assert_eq!(findings.len(), 2);
        assert!(findings.iter().any(|finding| {
            finding.props["ac_id"] == "AC-0041" && finding.props["subject_id"] == "ep:admin"
        }));
        assert!(findings.iter().any(|finding| {
            finding.props["ac_id"] == "AC-0042"
                && finding.props["actions"][0] == "s3:Get*"
                && finding.props["resource_scope"][0] == "arn:aws:s3:::orders/*"
        }));
        assert!(findings.iter().all(|finding| {
            finding.props["us_id"] == "US-0015"
                && finding.props["prov"]["confidence_tier"] == "Confirmed"
        }));
    }
}
