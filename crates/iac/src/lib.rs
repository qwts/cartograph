//! IaC extraction — deterministic (T0) Terraform/HCL resource graph plus
//! cloud capability resolution (SPEC-00 §3.1–3.2, US-0003).
//!
//! `hcl-edit` (span-preserving parser from the hcl-rs family, verified at M2)
//! parses each `.tf` file; resources/data/modules become `Resource` nodes,
//! interpolation traversals become `REFERENCES` edges, `depends_on` becomes
//! `DEPENDS_ON`, and the [`registry`] turns mediating resources into
//! `TRIGGERS`/`ROUTES`/`SUBSCRIBES` edges. IAM policy attachments yield
//! `GRANTS` edges to the resources their statements reference.
//!
//! Every fact carries Confirmed/Deterministic provenance with a byte span
//! (AC-0007, AC-0008). This tier never calls an LLM.

pub mod registry;

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use hcl_edit::Span;
use hcl_edit::expr::{Expression, Traversal, TraversalOperator};
use hcl_edit::structure::{Block, BlockLabel, Body};
use hcl_edit::visit::{Visit, visit_expr};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::Path;

/// Extraction errors.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    /// HCL syntax error.
    #[error("hcl parse in {path}: {message}")]
    Parse {
        /// File that failed to parse.
        path: String,
        /// Parser message.
        message: String,
    },
    /// Filesystem failure while walking a directory.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Facts extracted from one file or one directory walk.
#[derive(Debug, Default)]
pub struct Extraction {
    /// Graph nodes (props carry provenance under `prov`).
    pub nodes: Vec<Node>,
    /// Graph edges (props carry provenance under `prov`).
    pub edges: Vec<Edge>,
    policy_documents: BTreeMap<String, PolicyDocument>,
}

#[derive(Debug)]
struct PolicyDocument {
    resource_refs: BTreeSet<String>,
    actions: Vec<String>,
    evidence: EvidenceRef,
}

impl Extraction {
    fn resolve_policy_document_grants(&mut self) {
        let mut resolved = Vec::with_capacity(self.edges.len());
        for edge in std::mem::take(&mut self.edges) {
            if edge.label != "GRANTS" {
                resolved.push(edge);
                continue;
            }

            let Some(document) = self.policy_documents.get(&edge.dst) else {
                // Missing document stays explicit; close_over_endpoints turns
                // it into a placeholder rather than silently dropping it.
                resolved.push(edge);
                continue;
            };

            if document.resource_refs.is_empty() {
                // The document exists but has no T0-resolvable resource refs
                // (for example only var.*). Preserve the honest document hop,
                // while attaching the actions/evidence we did establish.
                let mut fallback = edge;
                fallback.props["actions"] = serde_json::json!(document.actions);
                fallback.props["policy_document"] = serde_json::json!(fallback.dst);
                fallback.props["prov"] = joined_policy_prov(
                    &fallback,
                    document,
                    &format!("GRANTS {} -> {}", fallback.src, fallback.dst),
                );
                resolved.push(fallback);
                continue;
            }

            for target in &document.resource_refs {
                let fact = format!("GRANTS {} -> {target} via {}", edge.src, edge.dst);
                resolved.push(Edge {
                    src: edge.src.clone(),
                    dst: resource_id_from_node(&edge.src, target),
                    label: "GRANTS".into(),
                    props: serde_json::json!({
                        "actions": document.actions,
                        "policy_document": edge.dst,
                        "registry": registry::REGISTRY_VERSION,
                        "prov": joined_policy_prov(&edge, document, &fact),
                    }),
                });
            }
        }
        self.edges = resolved;
    }

    /// Ensure every edge endpoint exists as a node; unresolved targets become
    /// flagged placeholder `Resource` nodes (explicit, never silently dropped).
    pub fn close_over_endpoints(&mut self) {
        let mut known: std::collections::HashSet<String> =
            self.nodes.iter().map(|n| n.id.clone()).collect();
        let mut placeholders = Vec::new();
        for edge in &self.edges {
            for id in [&edge.src, &edge.dst] {
                if known.insert(id.clone()) {
                    placeholders.push(Node {
                        id: id.clone(),
                        label: "Resource".into(),
                        props: serde_json::json!({ "placeholder": true }),
                    });
                }
            }
        }
        self.nodes.extend(placeholders);
    }
}

/// Identity of the source being extracted; lands in every `EvidenceRef`.
pub struct SourceId<'a> {
    /// Repository identifier (e.g. `owner/name`, or `local` for a bare dir).
    pub repo: &'a str,
    /// Commit SHA, or `workdir` when extracting an unversioned tree.
    pub commit: &'a str,
}

const EXTRACTOR_ID: &str = "t0.iac-terraform";

/// Terraform resource types whose `policy` attribute grants IAM permissions.
const POLICY_TYPES: &[&str] = &["aws_iam_policy", "aws_iam_role_policy", "aws_iam_role"];

fn prov(id: &SourceId, path: &str, span: &Range<usize>, fact: &str) -> serde_json::Value {
    let p = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        vec![EvidenceRef {
            repo: id.repo.into(),
            path: path.into(),
            byte_start: span.start as u64,
            byte_end: span.end as u64,
            commit_sha: id.commit.into(),
        }],
        EXTRACTOR_ID,
        fact.as_bytes(),
    )
    .expect("Deterministic/Confirmed is always within ceiling");
    serde_json::to_value(p).expect("provenance serializes")
}

fn label_text(label: &BlockLabel) -> String {
    match label {
        BlockLabel::Ident(i) => i.as_str().to_string(),
        BlockLabel::String(s) => s.value().to_string(),
    }
}

/// Terraform address of a traversal (`aws_s3_bucket.uploads.arn` →
/// `aws_s3_bucket.uploads`); `None` for vars/locals/builtins.
fn traversal_address(t: &Traversal) -> Option<String> {
    let Expression::Variable(root) = &t.expr else {
        return None;
    };
    let root = root.as_str();
    let attrs: Vec<String> = t
        .operators
        .iter()
        .map_while(|op| match op.value() {
            TraversalOperator::GetAttr(ident) => Some(ident.as_str().to_string()),
            _ => None,
        })
        .collect();
    match root {
        "var" | "local" | "each" | "count" | "path" | "terraform" | "self" => None,
        "module" => attrs.first().map(|m| format!("module.{m}")),
        "data" => (attrs.len() >= 2).then(|| format!("data.{}.{}", attrs[0], attrs[1])),
        _ if root.contains('_') => attrs.first().map(|name| format!("{root}.{name}")),
        _ => None,
    }
}

#[derive(Default)]
struct RefCollector {
    refs: BTreeSet<String>,
}

impl Visit for RefCollector {
    fn visit_traversal(&mut self, node: &Traversal) {
        if let Some(addr) = traversal_address(node) {
            self.refs.insert(addr);
        }
        hcl_edit::visit::visit_traversal(self, node);
    }
}

fn refs_in_expr(expr: &Expression) -> BTreeSet<String> {
    let mut c = RefCollector::default();
    visit_expr(&mut c, expr);
    c.refs
}

fn refs_in_body(body: &Body) -> BTreeSet<String> {
    let mut c = RefCollector::default();
    c.visit_body(body);
    c.refs
}

/// References found in the attribute (or single nested block) named `name`.
fn refs_for_attr(block: &Block, name: &str) -> BTreeSet<String> {
    for attr in block.body.attributes() {
        if attr.key.as_str() == name {
            return refs_in_expr(&attr.value);
        }
    }
    for nested in block.body.blocks() {
        if nested.ident.as_str() == name {
            return refs_in_body(&nested.body);
        }
    }
    BTreeSet::new()
}

fn block_name_matches(actual: &str, selector: &str) -> bool {
    selector
        .strip_prefix('*')
        .map_or(actual == selector, |suffix| actual.ends_with(suffix))
}

fn dynamic_block_matches(block: &Block, selector: &str) -> bool {
    block.ident.as_str() == "dynamic"
        && block
            .labels
            .first()
            .map(label_text)
            .is_some_and(|name| block_name_matches(&name, selector))
}

/// References selected by a path through nested blocks to a final attribute
/// (or final nested block). A leading `*` on a block segment means suffix
/// matching; registry paths use that only for Terraform's parallel
/// `default_cache_behavior` / `ordered_cache_behavior` shapes.
fn refs_for_path(body: &Body, path: &[&str]) -> BTreeSet<String> {
    let Some((head, tail)) = path.split_first() else {
        return BTreeSet::new();
    };

    if tail.is_empty() {
        let mut refs = BTreeSet::new();
        for attr in body.attributes() {
            if attr.key.as_str() == *head {
                refs.extend(refs_in_expr(&attr.value));
            }
        }
        for nested in body.blocks() {
            if block_name_matches(nested.ident.as_str(), head) {
                refs.extend(refs_in_body(&nested.body));
            } else if dynamic_block_matches(nested, head) {
                for content in nested.body.blocks() {
                    if content.ident.as_str() == "content" {
                        refs.extend(refs_in_body(&content.body));
                    }
                }
            }
        }
        return refs;
    }

    let mut refs = BTreeSet::new();
    for nested in body.blocks() {
        if block_name_matches(nested.ident.as_str(), head) {
            refs.extend(refs_for_path(&nested.body, tail));
        } else if dynamic_block_matches(nested, head) {
            for content in nested.body.blocks() {
                if content.ident.as_str() == "content" {
                    refs.extend(refs_for_path(&content.body, tail));
                }
            }
        }
    }
    refs
}

fn refs_for_selector(
    block: &Block,
    selector: registry::EndpointSelector,
    resource_address: &str,
) -> BTreeSet<String> {
    match selector {
        registry::EndpointSelector::Resource => BTreeSet::from([resource_address.to_string()]),
        registry::EndpointSelector::Path(path) => refs_for_path(&block.body, path),
    }
}

/// IAM action strings (`"s3:GetObject"`) appearing literally in the raw text
/// of a policy block — used as edge annotations on GRANTS, never invented.
fn literal_actions(raw: &str) -> Vec<String> {
    let mut actions = BTreeSet::new();
    for quoted in raw.split('"').skip(1).step_by(2) {
        if let Some((svc, action)) = quoted.split_once(':')
            && !svc.is_empty()
            && svc
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            && !action.is_empty()
            && action
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '*')
        {
            actions.insert(quoted.to_string());
        }
    }
    actions.into_iter().collect()
}

/// Resource ids are repo-namespaced (US-0001 slice 2): the same Terraform
/// address in two repos must never merge into one node.
fn resource_id(repo: &str, address: &str) -> String {
    format!("res:{repo}@{address}")
}

fn resource_id_from_node(source_node_id: &str, address: &str) -> String {
    let repo = source_node_id
        .strip_prefix("res:")
        .and_then(|rest| rest.split_once('@'))
        .map(|(repo, _)| repo)
        .expect("IaC resource node id has a repository namespace");
    resource_id(repo, address)
}

fn joined_policy_prov(edge: &Edge, document: &PolicyDocument, fact: &str) -> serde_json::Value {
    let source_prov: Provenance = serde_json::from_value(edge.props["prov"].clone())
        .expect("IaC GRANTS edge carries valid provenance");
    let mut evidence = source_prov.evidence;
    if !evidence.contains(&document.evidence) {
        evidence.push(document.evidence.clone());
    }
    let provenance = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        evidence,
        EXTRACTOR_ID,
        fact.as_bytes(),
    )
    .expect("Deterministic/Confirmed is always within ceiling");
    serde_json::to_value(provenance).expect("provenance serializes")
}

fn extract_source_unresolved(
    source: &str,
    path: &str,
    id: &SourceId,
) -> Result<Extraction, ExtractError> {
    let body: Body = source
        .parse()
        .map_err(|e: hcl_edit::parser::Error| ExtractError::Parse {
            path: path.into(),
            message: e.to_string(),
        })?;
    let mut out = Extraction::default();

    for block in body.blocks() {
        let kind = block.ident.as_str();
        let labels: Vec<String> = block.labels.iter().map(label_text).collect();
        let span = block.span().unwrap_or(0..source.len());

        // Address + node per block kind (outputs/vars/providers are not
        // resources; they join when locals/var resolution lands).
        let address = match (kind, labels.as_slice()) {
            ("resource", [rtype, name]) => format!("{rtype}.{name}"),
            ("data", [rtype, name]) => format!("data.{rtype}.{name}"),
            ("module", [name]) => format!("module.{name}"),
            _ => continue,
        };
        let node_id = resource_id(id.repo, &address);
        let rtype = if kind == "module" {
            "module".to_string()
        } else {
            labels[0].clone()
        };
        let provider = rtype.split('_').next().unwrap_or("").to_string();
        if kind == "data" && labels[0] == "aws_iam_policy_document" {
            out.policy_documents.insert(
                node_id.clone(),
                PolicyDocument {
                    resource_refs: refs_for_path(&block.body, &["statement", "resources"]),
                    actions: literal_actions(&source[span.clone()]),
                    evidence: EvidenceRef {
                        repo: id.repo.into(),
                        path: path.into(),
                        byte_start: span.start as u64,
                        byte_end: span.end as u64,
                        commit_sha: id.commit.into(),
                    },
                },
            );
        }
        out.nodes.push(Node {
            id: node_id.clone(),
            label: "Resource".into(),
            props: serde_json::json!({
                "type": rtype,
                "logical_id": address,
                "provider": provider,
                "kind": kind,
                "prov": prov(id, path, &span, &format!("Resource {address}")),
            }),
        });

        // depends_on -> DEPENDS_ON (explicit ordering intent).
        for dep in refs_for_attr(block, "depends_on") {
            out.edges.push(Edge {
                src: node_id.clone(),
                dst: resource_id(id.repo, &dep),
                label: "DEPENDS_ON".into(),
                props: serde_json::json!({
                    "prov": prov(id, path, &span, &format!("DEPENDS_ON {address} -> {dep}")),
                }),
            });
        }

        // All other traversals -> REFERENCES (the interpolation DAG).
        let deps: BTreeSet<String> = refs_for_attr(block, "depends_on");
        for referenced in refs_in_body(&block.body) {
            if referenced == address || deps.contains(&referenced) {
                continue;
            }
            out.edges.push(Edge {
                src: node_id.clone(),
                dst: resource_id(id.repo, &referenced),
                label: "REFERENCES".into(),
                props: serde_json::json!({
                    "prov": prov(id, path, &span, &format!("REFERENCES {address} -> {referenced}")),
                }),
            });
        }

        if kind != "resource" {
            continue;
        }

        // Capability Registry: mediating resource -> semantic edge.
        for cap in registry::capabilities_for(&labels[0]) {
            let sources = refs_for_selector(block, cap.source, &address);
            let targets = refs_for_selector(block, cap.target, &address);
            for s in &sources {
                for t in &targets {
                    out.edges.push(Edge {
                        src: resource_id(id.repo, s),
                        dst: resource_id(id.repo, t),
                        label: cap.kind.edge_label().into(),
                        props: serde_json::json!({
                            "via": node_id,
                            "registry": registry::REGISTRY_VERSION,
                            "prov": prov(id, path, &span, &format!(
                                "{} {s} -> {t} via {address}", cap.kind.edge_label()
                            )),
                        }),
                    });
                }
            }
        }

        // IAM: policy attribute -> GRANTS(policy resource -> referenced resource).
        if POLICY_TYPES.contains(&labels[0].as_str()) {
            let raw = &source[span.clone()];
            let actions = literal_actions(raw);
            for target in refs_for_attr(block, "policy") {
                out.edges.push(Edge {
                    src: node_id.clone(),
                    dst: resource_id(id.repo, &target),
                    label: "GRANTS".into(),
                    props: serde_json::json!({
                        "actions": actions,
                        "registry": registry::REGISTRY_VERSION,
                        "prov": prov(id, path, &span, &format!("GRANTS {address} -> {target}")),
                    }),
                });
            }
        }
    }

    Ok(out)
}

/// Extract facts from one Terraform file.
pub fn extract_source(source: &str, path: &str, id: &SourceId) -> Result<Extraction, ExtractError> {
    let mut out = extract_source_unresolved(source, path, id)?;
    out.resolve_policy_document_grants();
    Ok(out)
}

/// Extract facts from every `.tf` file under `root` (skipping `.terraform`
/// and hidden dirs), with edge endpoints closed over placeholders.
pub fn extract_dir(root: &Path, id: &SourceId) -> Result<Extraction, ExtractError> {
    let mut files = Vec::new();
    collect_tf_files(root, root, &mut files)?;
    files.sort(); // deterministic order (US-0014)
    let mut out = Extraction::default();
    for rel in &files {
        let source = std::fs::read_to_string(root.join(rel))?;
        let ex = extract_source_unresolved(&source, rel, id)?;
        out.nodes.extend(ex.nodes);
        out.edges.extend(ex.edges);
        out.policy_documents.extend(ex.policy_documents);
    }
    out.resolve_policy_document_grants();
    out.close_over_endpoints();
    Ok(out)
}

fn collect_tf_files(root: &Path, dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name.starts_with('.') || name == "node_modules" {
                continue;
            }
            collect_tf_files(root, &path, out)?;
        } else if name.ends_with(".tf") {
            let rel = path
                .strip_prefix(root)
                .expect("entry under root")
                .to_string_lossy()
                .replace('\\', "/");
            out.push(rel);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
