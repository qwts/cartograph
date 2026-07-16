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
use hcl_edit::expr::{Expression, ObjectKey, ObjectValue, Traversal, TraversalOperator};
use hcl_edit::structure::{Block, BlockLabel, Body};
use hcl_edit::visit::{Visit, visit_expr};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

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
#[derive(Debug, Clone, Default)]
pub struct Extraction {
    /// Graph nodes (props carry provenance under `prov`).
    pub nodes: Vec<Node>,
    /// Graph edges (props carry provenance under `prov`).
    pub edges: Vec<Edge>,
    policy_documents: BTreeMap<String, PolicyDocument>,
    module_declarations: Vec<ModuleDeclaration>,
}

#[derive(Debug, Clone)]
struct PolicyDocument {
    statements: Vec<PolicyStatement>,
    evidence: EvidenceRef,
}

#[derive(Debug, Clone)]
struct PolicyStatement {
    resource_refs: BTreeSet<String>,
    resource_scopes: BTreeSet<String>,
    actions: BTreeSet<String>,
}

#[derive(Debug, Clone)]
struct ModuleDeclaration {
    address: String,
    node_id: String,
    source: String,
    declaring_path: String,
    evidence: EvidenceRef,
    ancestors: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
struct CachedFile {
    source_hash: String,
    extraction: Extraction,
}

/// Reusable per-file/per-module-context T0 parse cache (AC-0040).
#[derive(Debug, Default)]
pub struct IncrementalCache {
    files: BTreeMap<String, CachedFile>,
}

/// Terraform extraction contexts parsed or reused during one ingest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IncrementalStats {
    /// New or byte-changed file contexts parsed in this run.
    pub recomputed_files: u64,
    /// Unchanged file contexts reused by content hash.
    pub reused_files: u64,
    /// Cached contexts no longer reachable from the ingest root/module DAG.
    pub deleted_files: u64,
}

struct CacheRun<'a> {
    cache: &'a mut IncrementalCache,
    active: BTreeSet<String>,
    stats: IncrementalStats,
}

fn retarget_props_commit(props: &mut serde_json::Value, commit: &str) {
    let Ok(mut provenance) =
        serde_json::from_value::<Provenance>(props.get("prov").cloned().unwrap_or_default())
    else {
        return;
    };
    for evidence in &mut provenance.evidence {
        evidence.commit_sha = commit.to_string();
    }
    props["prov"] = serde_json::to_value(provenance).expect("provenance serializes");
}

fn retarget_commit(extraction: &mut Extraction, commit: &str) {
    for node in &mut extraction.nodes {
        retarget_props_commit(&mut node.props, commit);
    }
    for edge in &mut extraction.edges {
        retarget_props_commit(&mut edge.props, commit);
    }
    for document in extraction.policy_documents.values_mut() {
        document.evidence.commit_sha = commit.to_string();
    }
    for declaration in &mut extraction.module_declarations {
        declaration.evidence.commit_sha = commit.to_string();
    }
}

fn cached_file_key(path: &str, address_prefix: Option<&str>) -> String {
    format!("{}@{path}", address_prefix.unwrap_or_default())
}

fn extract_file_incremental(
    root: &Path,
    path: &str,
    id: &SourceId,
    address_prefix: Option<&str>,
    module_ancestors: &[PathBuf],
    run: &mut CacheRun<'_>,
) -> Result<Extraction, ExtractError> {
    let source = std::fs::read_to_string(root.join(path))?;
    let source_hash = core_prov::content_hash(source.as_bytes());
    let key = cached_file_key(path, address_prefix);
    run.active.insert(key.clone());
    let extraction = if let Some(cached) = run
        .cache
        .files
        .get(&key)
        .filter(|cached| cached.source_hash == source_hash)
    {
        run.stats.reused_files += 1;
        let mut extraction = cached.extraction.clone();
        retarget_commit(&mut extraction, id.commit);
        extraction
    } else {
        run.stats.recomputed_files += 1;
        extract_source_unresolved(&source, path, id, address_prefix, module_ancestors)?
    };
    run.cache.files.insert(
        key,
        CachedFile {
            source_hash,
            extraction: extraction.clone(),
        },
    );
    Ok(extraction)
}

impl Extraction {
    fn absorb(&mut self, mut other: Extraction) {
        self.nodes.append(&mut other.nodes);
        self.edges.append(&mut other.edges);
        self.policy_documents.extend(other.policy_documents);
        self.module_declarations
            .append(&mut other.module_declarations);
    }

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

            let mut targets = BTreeMap::<String, (BTreeSet<String>, BTreeSet<String>)>::new();
            let mut unresolved_actions = BTreeSet::new();
            let mut unresolved_scopes = BTreeSet::new();
            let mut has_unresolved_statement = document.statements.is_empty();
            for statement in &document.statements {
                if statement.resource_refs.is_empty() {
                    has_unresolved_statement = true;
                    unresolved_actions.extend(statement.actions.iter().cloned());
                    unresolved_scopes.extend(statement.resource_scopes.iter().cloned());
                    continue;
                }
                for target in &statement.resource_refs {
                    let (actions, scopes) = targets.entry(target.clone()).or_default();
                    actions.extend(statement.actions.iter().cloned());
                    scopes.extend(statement.resource_scopes.iter().cloned());
                }
            }

            if has_unresolved_statement {
                // A statement with no T0-resolvable resource ref (for example
                // only `*` or var.*) stays as an honest document hop. Its
                // annotations include only that unresolved statement set.
                let mut fallback = edge.clone();
                fallback.props["actions"] = serde_json::json!(unresolved_actions);
                fallback.props["resource_scopes"] = serde_json::json!(unresolved_scopes);
                fallback.props["policy_document"] = serde_json::json!(fallback.dst);
                fallback.props["prov"] = joined_policy_prov(
                    &fallback,
                    document,
                    &format!("GRANTS {} -> {}", fallback.src, fallback.dst),
                );
                resolved.push(fallback);
            }

            for (target, (actions, resource_scopes)) in targets {
                let fact = format!("GRANTS {} -> {target} via {}", edge.src, edge.dst);
                resolved.push(Edge {
                    src: edge.src.clone(),
                    dst: resource_id_from_node(&edge.src, &target),
                    label: "GRANTS".into(),
                    props: serde_json::json!({
                        "actions": actions,
                        "resource_scopes": resource_scopes,
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
        vec![evidence_ref(id, path, span)],
        EXTRACTOR_ID,
        fact.as_bytes(),
    )
    .expect("Deterministic/Confirmed is always within ceiling");
    serde_json::to_value(p).expect("provenance serializes")
}

fn evidence_ref(id: &SourceId, path: &str, span: &Range<usize>) -> EvidenceRef {
    EvidenceRef {
        repo: id.repo.into(),
        path: path.into(),
        byte_start: span.start as u64,
        byte_end: span.end as u64,
        commit_sha: id.commit.into(),
    }
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

fn string_attr<'a>(block: &'a Block, name: &str) -> Option<&'a str> {
    block
        .body
        .attributes()
        .find(|attr| attr.key.as_str() == name)
        .and_then(|attr| attr.value.as_str())
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

#[derive(Default)]
struct StaticStringCollector {
    values: BTreeSet<String>,
}

impl Visit for StaticStringCollector {
    fn visit_string(&mut self, node: &hcl_edit::Decorated<String>) {
        self.values.insert(node.value().clone());
    }
}

fn static_strings_in_expr(expr: &Expression) -> BTreeSet<String> {
    let mut collector = StaticStringCollector::default();
    visit_expr(&mut collector, expr);
    collector.values
}

/// Static string values selected by the same nested-block path rules used for
/// references. Security projection needs literal IAM resource scopes such as
/// `*`; keeping them on GRANTS avoids reconstructing policy text later.
fn static_strings_for_path(body: &Body, path: &[&str]) -> BTreeSet<String> {
    let Some((head, tail)) = path.split_first() else {
        return BTreeSet::new();
    };

    if tail.is_empty() {
        let mut values = BTreeSet::new();
        for attr in body.attributes() {
            if attr.key.as_str() == *head {
                values.extend(static_strings_in_expr(&attr.value));
            }
        }
        return values;
    }

    let mut values = BTreeSet::new();
    for nested in body.blocks() {
        if block_name_matches(nested.ident.as_str(), head) {
            values.extend(static_strings_for_path(&nested.body, tail));
        } else if dynamic_block_matches(nested, head) {
            for content in nested.body.blocks() {
                if content.ident.as_str() == "content" {
                    values.extend(static_strings_for_path(&content.body, tail));
                }
            }
        }
    }
    values
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

fn action_key(key: &ObjectKey) -> bool {
    let name = key.as_ident().map(ToString::to_string).or_else(|| {
        key.as_expr()
            .and_then(Expression::as_str)
            .map(str::to_string)
    });
    name.is_some_and(|name| {
        name.eq_ignore_ascii_case("action") || name.eq_ignore_ascii_case("actions")
    })
}

#[derive(Default)]
struct IamActionCollector {
    values: BTreeSet<String>,
}

impl Visit for IamActionCollector {
    fn visit_object_item(&mut self, key: &ObjectKey, value: &ObjectValue) {
        if action_key(key) {
            self.values.extend(static_strings_in_expr(value.expr()));
        } else {
            visit_expr(self, value.expr());
        }
    }
}

/// Literal IAM actions from exact `Action`/`action`/`actions` object keys.
/// `NotAction`/`not_actions` has different semantics and is intentionally
/// excluded instead of substring-scanning raw policy text.
fn literal_actions(expr: &Expression) -> BTreeSet<String> {
    let mut collector = IamActionCollector::default();
    visit_expr(&mut collector, expr);
    collector.values
}

fn statement_actions(body: &Body) -> BTreeSet<String> {
    let mut actions = BTreeSet::new();
    for attr in body.attributes() {
        if matches!(attr.key.as_str(), "action" | "actions") {
            actions.extend(static_strings_in_expr(&attr.value));
        }
    }
    actions
}

fn policy_statements(body: &Body, address_prefix: Option<&str>) -> Vec<PolicyStatement> {
    let mut statements = Vec::new();
    for nested in body.blocks() {
        let statement_body = if nested.ident.as_str() == "statement" {
            Some(&nested.body)
        } else if dynamic_block_matches(nested, "statement") {
            nested
                .body
                .blocks()
                .find(|content| content.ident.as_str() == "content")
                .map(|content| &content.body)
        } else {
            None
        };
        let Some(statement_body) = statement_body else {
            continue;
        };
        statements.push(PolicyStatement {
            resource_refs: scoped_refs(
                refs_for_path(statement_body, &["resources"]),
                address_prefix,
            ),
            resource_scopes: static_strings_for_path(statement_body, &["resources"]),
            actions: statement_actions(statement_body),
        });
    }
    statements
}

/// Resource ids are repo-namespaced (US-0001 slice 2): the same Terraform
/// address in two repos must never merge into one node.
fn resource_id(repo: &str, address: &str) -> String {
    format!("res:{repo}@{address}")
}

fn scoped_address(prefix: Option<&str>, address: &str) -> String {
    prefix.map_or_else(
        || address.to_string(),
        |prefix| format!("{prefix}.{address}"),
    )
}

fn scoped_refs(refs: BTreeSet<String>, prefix: Option<&str>) -> BTreeSet<String> {
    refs.into_iter()
        .map(|address| scoped_address(prefix, &address))
        .collect()
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
    address_prefix: Option<&str>,
    module_ancestors: &[PathBuf],
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
        let local_address = match (kind, labels.as_slice()) {
            ("resource", [rtype, name]) => format!("{rtype}.{name}"),
            ("data", [rtype, name]) => format!("data.{rtype}.{name}"),
            ("module", [name]) => format!("module.{name}"),
            _ => continue,
        };
        let address = scoped_address(address_prefix, &local_address);
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
                    statements: policy_statements(&block.body, address_prefix),
                    evidence: evidence_ref(id, path, &span),
                },
            );
        }
        if kind == "module"
            && let Some(module_source) = string_attr(block, "source")
        {
            out.module_declarations.push(ModuleDeclaration {
                address: address.clone(),
                node_id: node_id.clone(),
                source: module_source.to_string(),
                declaring_path: path.to_string(),
                evidence: evidence_ref(id, path, &span),
                ancestors: module_ancestors.to_vec(),
            });
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
        let raw_deps = refs_for_attr(block, "depends_on");
        for dep in scoped_refs(raw_deps.clone(), address_prefix) {
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
        for raw_reference in refs_in_body(&block.body) {
            if raw_reference == local_address || raw_deps.contains(&raw_reference) {
                continue;
            }
            let referenced = scoped_address(address_prefix, &raw_reference);
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
            let sources = scoped_refs(
                refs_for_selector(block, cap.source, &local_address),
                address_prefix,
            );
            let targets = scoped_refs(
                refs_for_selector(block, cap.target, &local_address),
                address_prefix,
            );
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
            let actions = block
                .body
                .attributes()
                .find(|attr| attr.key.as_str() == "policy")
                .map(|attr| literal_actions(&attr.value))
                .unwrap_or_default();
            for target in scoped_refs(refs_for_attr(block, "policy"), address_prefix) {
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
    let mut out = extract_source_unresolved(source, path, id, None, &[])?;
    out.resolve_policy_document_grants();
    Ok(out)
}

/// Extract facts from every `.tf` file under `root` (skipping `.terraform`
/// and hidden dirs), with edge endpoints closed over placeholders.
pub fn extract_dir(root: &Path, id: &SourceId) -> Result<Extraction, ExtractError> {
    extract_dir_incremental(root, id, &mut IncrementalCache::default()).map(|(facts, _)| facts)
}

/// Extract Terraform while parsing only changed file/module contexts. Global
/// module expansion and policy-document resolution rerun deterministically so
/// dependents of a changed declaration are refreshed without stale edges.
pub fn extract_dir_incremental(
    root: &Path,
    id: &SourceId,
    cache: &mut IncrementalCache,
) -> Result<(Extraction, IncrementalStats), ExtractError> {
    extract_dir_incremental_with_progress(root, id, cache, &mut |_| {})
}

/// Same as [`extract_dir_incremental`], calling `on_file` with each file's
/// repo-relative path as it's read (#209 live progress hook).
pub fn extract_dir_incremental_with_progress(
    root: &Path,
    id: &SourceId,
    cache: &mut IncrementalCache,
    on_file: &mut dyn FnMut(&str),
) -> Result<(Extraction, IncrementalStats), ExtractError> {
    let root = std::fs::canonicalize(root)?;
    let mut files = Vec::new();
    collect_tf_files(&root, &root, &mut files)?;
    files.sort(); // deterministic order (US-0014)
    let old_keys = cache.files.keys().cloned().collect::<BTreeSet<_>>();
    let mut run = CacheRun {
        cache,
        active: BTreeSet::new(),
        stats: IncrementalStats::default(),
    };
    let mut out = Extraction::default();
    for rel in &files {
        on_file(rel);
        out.absorb(extract_file_incremental(
            &root,
            rel,
            id,
            None,
            &[],
            &mut run,
        )?);
    }
    expand_local_modules(&root, id, &mut out, &mut run)?;
    out.resolve_policy_document_grants();
    out.close_over_endpoints();
    run.stats.deleted_files = old_keys.difference(&run.active).count() as u64;
    run.cache.files.retain(|key, _| run.active.contains(key));
    Ok((out, run.stats))
}

/// Count physical Terraform source files using the same confined walk as
/// extraction. Module instantiations do not inflate this count.
pub fn terraform_file_count(root: &Path) -> Result<u64, ExtractError> {
    let root = std::fs::canonicalize(root)?;
    let mut files = Vec::new();
    collect_tf_files(&root, &root, &mut files)?;
    Ok(files.len() as u64)
}

fn expand_local_modules(
    root: &Path,
    id: &SourceId,
    out: &mut Extraction,
    run: &mut CacheRun<'_>,
) -> Result<(), ExtractError> {
    let mut pending = std::mem::take(&mut out.module_declarations);
    let mut expanded = BTreeSet::new();

    while !pending.is_empty() {
        pending.sort_by(|a, b| {
            (&a.address, &a.declaring_path, &a.source).cmp(&(
                &b.address,
                &b.declaring_path,
                &b.source,
            ))
        });
        let declaration = pending.remove(0);
        let Some(source_dir) = resolve_local_module_source(root, &declaration) else {
            continue;
        };

        let mut ancestors = declaration.ancestors.clone();
        if ancestors.is_empty()
            && let Some(parent) = root.join(&declaration.declaring_path).parent()
            && let Ok(parent) = std::fs::canonicalize(parent)
            && parent.starts_with(root)
        {
            ancestors.push(parent);
        }
        if ancestors.contains(&source_dir) {
            // Recursive source cycle: the nested module remains an explicit
            // leaf rather than reading the same directory indefinitely.
            continue;
        }
        if !expanded.insert((declaration.node_id.clone(), source_dir.clone())) {
            continue;
        }
        ancestors.push(source_dir.clone());

        let mut module_extraction = Extraction::default();
        for rel in collect_direct_tf_files(root, &source_dir)? {
            module_extraction.absorb(extract_file_incremental(
                root,
                &rel,
                id,
                Some(&declaration.address),
                &ancestors,
                run,
            )?);
        }

        let child_ids: BTreeSet<String> = module_extraction
            .nodes
            .iter()
            .map(|node| node.id.clone())
            .collect();
        for child_id in child_ids {
            let fact = format!("REFERENCES {} -> {child_id}", declaration.address);
            let provenance = Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
                vec![declaration.evidence.clone()],
                EXTRACTOR_ID,
                fact.as_bytes(),
            )
            .expect("Deterministic/Confirmed is always within ceiling");
            out.edges.push(Edge {
                src: declaration.node_id.clone(),
                dst: child_id,
                label: "REFERENCES".into(),
                props: serde_json::json!({
                    "module_source": declaration.source,
                    "relation": "MODULE_CONTAINS",
                    "prov": provenance,
                }),
            });
        }

        pending.append(&mut module_extraction.module_declarations);
        out.absorb(module_extraction);
    }

    Ok(())
}

fn resolve_local_module_source(root: &Path, declaration: &ModuleDeclaration) -> Option<PathBuf> {
    let source = Path::new(&declaration.source);
    let local_literal = source.is_absolute()
        || declaration.source == "."
        || declaration.source == ".."
        || declaration.source.starts_with("./")
        || declaration.source.starts_with("../")
        || declaration.source.starts_with(".\\")
        || declaration.source.starts_with("..\\");
    if !local_literal {
        return None;
    }

    let declaring_dir = root
        .join(&declaration.declaring_path)
        .parent()?
        .to_path_buf();
    let candidate = if source.is_absolute() {
        source.to_path_buf()
    } else {
        declaring_dir.join(source)
    };
    let candidate = normalize_lexically(&candidate);
    if !candidate.starts_with(root) {
        return None;
    }
    let canonical = std::fs::canonicalize(candidate).ok()?;
    (canonical.starts_with(root) && canonical.is_dir()).then_some(canonical)
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

fn collect_direct_tf_files(root: &Path, dir: &Path) -> std::io::Result<Vec<String>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() || !file_type.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".tf") {
            files.push(
                entry
                    .path()
                    .strip_prefix(root)
                    .expect("confined module entry is under the ingest root")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    files.sort();
    Ok(files)
}

fn collect_tf_files(root: &Path, dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if name.starts_with('.') || name == "node_modules" {
                continue;
            }
            collect_tf_files(root, &path, out)?;
        } else if file_type.is_file() && name.ends_with(".tf") {
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
