use super::CapturedError;
use crate::SourceId;
use core_graph::rules::{DependencyResolution, GuardedExitEvidence, LocalExit};
use core_graph::source::{FactKey, edge_digest, node_digest};
use core_graph::{Edge, Node};
use core_prov::{EvidenceRef, Provenance};
use serde::{Deserialize, Deserializer, Serialize};
use source_capture::{CaptureFileRef, CaptureSpanRef, CapturedFile};
use std::collections::BTreeMap;

/// Maximum complete serialized receipt, including identity and all ranges.
pub const MAX_RECEIPT_BYTES: usize = 128 * 1024;
/// Maximum complete range inventory. Receipts never contain partial inventories.
pub const MAX_RECEIPT_RANGES: usize = 1024;
const MAX_ID_BYTES: usize = 4096;
const MAX_REPO_BYTES: usize = 256;
const RECEIPT_SCHEMA_VERSION: u32 = 2;

struct Contract {
    prefix: &'static str,
    producer: &'static str,
    domain: &'static [u8],
    grammar_package: &'static str,
    parser_package: &'static str,
}

fn contract(version: u32) -> Result<Contract, CapturedError> {
    match version {
        1 => Ok(Contract {
            prefix: "ts-primary-v1:",
            producer: "t0.adapter-ts/direct-lexical-v1",
            domain: b"cartograph:ts-primary-receipt:v1\0",
            // Historical pins are frozen independently of the current producer.
            grammar_package: "tree-sitter-typescript@0.23.2",
            parser_package: "tree-sitter@0.26.12",
        }),
        2 => Ok(Contract {
            prefix: "ts-primary-v2:",
            producer: "t0.adapter-ts/direct-lexical-v2",
            domain: b"cartograph:ts-primary-receipt:v2\0",
            grammar_package: GRAMMAR_PACKAGE,
            parser_package: PARSER_PACKAGE,
        }),
        _ => Err(CapturedError::Invalid("receipt contract version")),
    }
}
// Exact dependency pins in Cargo.toml keep these producer inputs truthful.
const GRAMMAR_PACKAGE: &str = "tree-sitter-typescript@0.23.2";
const PARSER_PACKAGE: &str = "tree-sitter@0.26.12";

/// Actual grammar selected by the ordinary parser for the retained path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grammar {
    /// Bare .ts files permit TypeScript angle-bracket casts.
    #[serde(rename = "typescript")]
    TypeScript,
    /// Other supported JS/TS extensions use the JSX-aware grammar.
    Tsx,
}

impl Grammar {
    fn for_path(path: &str) -> Self {
        if path.ends_with(".ts") {
            Self::TypeScript
        } else {
            Self::Tsx
        }
    }
}

/// Position in the producer's fixed structural range inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RangeRole {
    /// Primary provenance evidence, in stored provenance order.
    Provenance,
    /// Guarded return/throw source.
    RuleExit,
    /// Enclosing branch statement, indexed by condition.
    ConditionBranch,
    /// Branch expression, indexed by condition.
    ConditionExpression,
    /// Optional return value, index zero.
    ReturnValue,
    /// Throw value, index zero.
    ThrowValue,
    /// Dependency occurrence, indexed by dependency.
    Dependency,
    /// A binding's declaration, indexed by its dependency.
    DependencyDeclaration,
    /// Redacted original source, indexed by redaction.
    Redaction,
    /// Local declaration, indexed by definition (v2 only).
    DefinitionDeclaration,
    /// Original admitted use, using a global lexical per-role counter (v2 only).
    DefinitionUse,
    /// Original initializer, indexed by definition (v2 only).
    DefinitionInitializer,
    /// Arena expression, using a global producer-order counter (v2 only).
    DefinitionExpression,
    /// Initializer prerequisite, using a global per-role counter (v2 only).
    DefinitionDependency,
    /// Binding declaration paired with its initializer dependency index (v2 only).
    DefinitionDependencyDeclaration,
}

/// One unchanged original citation and its exact retained raw-byte binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptRange {
    /// Structural source role, never source text.
    pub role: RangeRole,
    /// Index within the role's original container; never a caller-chosen offset.
    pub index: u32,
    /// Original legacy reference, preserved without changing its wire contract.
    #[serde(deserialize_with = "strict_evidence")]
    pub evidence: EvidenceRef,
    /// Same offsets into the same immutable captured file.
    pub captured: CaptureSpanRef,
}

fn strict_evidence<'de, D: Deserializer<'de>>(deserializer: D) -> Result<EvidenceRef, D::Error> {
    // The legacy EvidenceRef decoder is permissive. The new receipt envelope
    // permits exactly its unchanged five fields, so ignored source-bearing
    // fields cannot survive as an alternate wire representation of a receipt.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct WireEvidence {
        repo: String,
        path: String,
        byte_start: u64,
        byte_end: u64,
        commit_sha: String,
    }
    let wire = WireEvidence::deserialize(deserializer)?;
    Ok(EvidenceRef {
        repo: wire.repo,
        path: wire.path,
        byte_start: wire.byte_start,
        byte_end: wire.byte_end,
        commit_sha: wire.commit_sha,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SourceScope {
    PrimarySourceOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum InputClosure {
    InputClosureNotEstablished,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Content {
    schema_version: u32,
    source_id: source_capture::SourceId,
    repo_key: String,
    file: CaptureFileRef,
    producer_contract: String,
    grammar: Grammar,
    grammar_package: String,
    parser_package: String,
    fact_key: FactKey,
    fact_digest: String,
    ranges: Vec<ReceiptRange>,
    source_scope: SourceScope,
    input_closure: InputClosure,
}

/// Immutable metadata-only producer receipt, separate from graph properties.
/// Public decoding/validation establishes internal content consistency only;
/// it cannot prove that caller-supplied JSON came from this producer invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Receipt {
    receipt_id: String,
    content: Content,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReceipt {
    receipt_id: String,
    content: Content,
}

impl<'de> Deserialize<'de> for Receipt {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = WireReceipt::deserialize(deserializer)
            .map_err(|_| serde::de::Error::custom("invalid primary source receipt metadata"))?;
        let receipt = Self {
            receipt_id: wire.receipt_id,
            content: wire.content,
        };
        receipt.validate().map_err(serde::de::Error::custom)?;
        Ok(receipt)
    }
}

fn bounded_identity(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn hash_has_prefix(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

pub(super) fn validate_source_repo(
    source: &source_capture::SourceId,
    repo: &str,
) -> Result<(), CapturedError> {
    let id = source.as_str();
    if !id.strip_prefix("src_").is_some_and(|value| {
        value.len() == 32
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }) || !bounded_identity(repo, MAX_REPO_BYTES)
        || repo.strip_prefix("local/").is_some_and(|local| local != id)
    {
        return Err(CapturedError::Invalid("registered source identity"));
    }
    Ok(())
}

pub(super) fn validate_identity(file: &CaptureFileRef, repo: &str) -> Result<(), CapturedError> {
    validate_source_repo(&file.source_id, repo)?;
    // Capture IDs are versioned hashes; CaptureStore independently verifies
    // membership and the actual raw object before a source read.
    if !hash_has_prefix(&file.capture_id, "capture-v1:")
        || !hash_has_prefix(&file.digest, "")
        || file.byte_len > source_capture::MAX_FILE_BYTES
        || !super::valid_path(&file.path)
        || !super::supported(&file.path)
    {
        return Err(CapturedError::Invalid("capture file binding"));
    }
    Ok(())
}

fn canonical(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(object) => serde_json::Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, canonical(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(canonical).collect())
        }
        other => other,
    }
}

fn content_id(content: &Content) -> Result<String, CapturedError> {
    let value = serde_json::to_value(content)
        .map_err(|_| CapturedError::Invalid("receipt serialization"))?;
    let bytes = serde_json::to_vec(&canonical(value))
        .map_err(|_| CapturedError::Invalid("receipt serialization"))?;
    let contract = contract(content.schema_version)?;
    let mut input = contract.domain.to_vec();
    input.extend(bytes);
    Ok(format!(
        "{}{}",
        contract.prefix,
        core_prov::content_hash(&input)
    ))
}

fn inventory(
    props: &serde_json::Value,
    version: u32,
) -> Result<Vec<(RangeRole, u32, EvidenceRef)>, CapturedError> {
    contract(version)?;
    let provenance: Provenance = serde_json::from_value(
        props
            .get("prov")
            .cloned()
            .ok_or(CapturedError::Invalid("missing provenance"))?,
    )
    .map_err(|_| CapturedError::Invalid("malformed provenance"))?;
    provenance
        .validate()
        .map_err(|_| CapturedError::Invalid("invalid provenance"))?;
    let mut ranges = Vec::new();
    let mut push = |role, index: usize, evidence: &EvidenceRef| -> Result<(), CapturedError> {
        if ranges.len() == MAX_RECEIPT_RANGES {
            return Err(CapturedError::Limit("receipt ranges"));
        }
        ranges.push((
            role,
            u32::try_from(index).map_err(|_| CapturedError::Limit("range index"))?,
            evidence.clone(),
        ));
        Ok(())
    };
    for (index, evidence) in provenance.evidence.iter().enumerate() {
        push(RangeRole::Provenance, index, evidence)?;
    }
    if let Some(value) = props.get("rule") {
        let rule = GuardedExitEvidence::from_value(value.clone())
            .map_err(|_| CapturedError::Invalid("rule source inventory"))?;
        if rule.schema_version != version {
            return Err(CapturedError::Invalid("rule and receipt version"));
        }
        push(RangeRole::RuleExit, 0, &rule.exit_source)?;
        for (index, condition) in rule.conditions.iter().enumerate() {
            push(RangeRole::ConditionBranch, index, &condition.branch_source)?;
            push(
                RangeRole::ConditionExpression,
                index,
                &condition.expression.source,
            )?;
        }
        match &rule.effect {
            LocalExit::Return { value: Some(value) } => {
                push(RangeRole::ReturnValue, 0, &value.source)?
            }
            LocalExit::Return { value: None } => {}
            LocalExit::Throw { value } => push(RangeRole::ThrowValue, 0, &value.source)?,
        }
        for (index, dependency) in rule.dependencies.iter().enumerate() {
            push(RangeRole::Dependency, index, &dependency.source)?;
            if let DependencyResolution::Binding { declaration, .. } = &dependency.resolution {
                push(RangeRole::DependencyDeclaration, index, declaration)?;
            }
        }
        if let Some(definitions) = &rule.local_definitions {
            let mut use_index = 0;
            let mut expression_index = 0;
            let mut dependency_index = 0;
            for (index, definition) in definitions.iter().enumerate() {
                push(
                    RangeRole::DefinitionDeclaration,
                    index,
                    &definition.declaration,
                )?;
                for usage in &definition.uses {
                    push(RangeRole::DefinitionUse, use_index, usage)?;
                    use_index += 1;
                }
                push(
                    RangeRole::DefinitionInitializer,
                    index,
                    &definition.initializer.source,
                )?;
                for node in &definition.expression.nodes {
                    push(
                        RangeRole::DefinitionExpression,
                        expression_index,
                        &node.expression.source,
                    )?;
                    expression_index += 1;
                }
                for dependency in &definition.dependencies {
                    push(
                        RangeRole::DefinitionDependency,
                        dependency_index,
                        &dependency.source,
                    )?;
                    if let DependencyResolution::Binding { declaration, .. } =
                        &dependency.resolution
                    {
                        push(
                            RangeRole::DefinitionDependencyDeclaration,
                            dependency_index,
                            declaration,
                        )?;
                    }
                    dependency_index += 1;
                }
            }
        }
        for (index, redaction) in rule.redactions.iter().enumerate() {
            push(RangeRole::Redaction, index, &redaction.source)?;
        }
    }
    Ok(ranges)
}

fn valid_inventory_shape(ranges: &[ReceiptRange], version: u32) -> bool {
    let mut next = 0;
    let mut take = |role, index| {
        if ranges
            .get(next)
            .is_some_and(|range| range.role == role && range.index == index)
        {
            next += 1;
            true
        } else {
            false
        }
    };
    let mut index = 0;
    while take(RangeRole::Provenance, index) {
        index += 1;
    }
    if index == 0 {
        return false;
    }
    // Owner/relationship receipts contain primary provenance alone. Rule
    // inventories add the fixed structural traversal without grouping away
    // duplicate source references or substituting arbitrary JSON pointers.
    if take(RangeRole::RuleExit, 0) {
        index = 0;
        while take(RangeRole::ConditionBranch, index) {
            if !take(RangeRole::ConditionExpression, index) {
                return false;
            }
            index += 1;
        }
        if !take(RangeRole::ReturnValue, 0) {
            take(RangeRole::ThrowValue, 0);
        }
        index = 0;
        while take(RangeRole::Dependency, index) {
            take(RangeRole::DependencyDeclaration, index);
            index += 1;
        }
        if version == 2 {
            let mut definition = 0;
            let mut usage = 0;
            let mut expression = 0;
            let mut dependency = 0;
            while take(RangeRole::DefinitionDeclaration, definition) {
                let previous_uses = usage;
                while take(RangeRole::DefinitionUse, usage) {
                    usage += 1;
                }
                if usage == previous_uses || !take(RangeRole::DefinitionInitializer, definition) {
                    return false;
                }
                let previous_expressions = expression;
                while take(RangeRole::DefinitionExpression, expression) {
                    expression += 1;
                }
                if expression == previous_expressions {
                    return false;
                }
                while take(RangeRole::DefinitionDependency, dependency) {
                    take(RangeRole::DefinitionDependencyDeclaration, dependency);
                    dependency += 1;
                }
                definition += 1;
            }
            if definition as usize > core_graph::rules::MAX_LOCAL_DEFINITIONS
                || expression as usize > core_graph::rules::MAX_DEFINITION_NODES
                || dependency as usize > core_graph::rules::MAX_INITIALIZER_DEPENDENCIES
            {
                return false;
            }
        }
        index = 0;
        while take(RangeRole::Redaction, index) {
            index += 1;
        }
    }
    next == ranges.len()
}

impl Receipt {
    fn mint(
        file: &CapturedFile,
        id: &SourceId,
        fact_key: FactKey,
        fact_digest: String,
        props: &serde_json::Value,
    ) -> Result<Self, CapturedError> {
        let ranges = inventory(props, RECEIPT_SCHEMA_VERSION)?
            .into_iter()
            .map(|(role, index, evidence)| {
                let captured = file.span(evidence.byte_start, evidence.byte_end)?;
                Ok(ReceiptRange {
                    role,
                    index,
                    evidence,
                    captured,
                })
            })
            .collect::<Result<Vec<_>, CapturedError>>()?;
        let content = Content {
            schema_version: RECEIPT_SCHEMA_VERSION,
            source_id: file.reference().source_id.clone(),
            repo_key: id.repo.into(),
            file: file.reference().clone(),
            producer_contract: contract(RECEIPT_SCHEMA_VERSION)?.producer.into(),
            grammar: Grammar::for_path(&file.reference().path),
            grammar_package: contract(RECEIPT_SCHEMA_VERSION)?.grammar_package.into(),
            parser_package: contract(RECEIPT_SCHEMA_VERSION)?.parser_package.into(),
            fact_key,
            fact_digest,
            ranges,
            source_scope: SourceScope::PrimarySourceOnly,
            input_closure: InputClosure::InputClosureNotEstablished,
        };
        let receipt = Self {
            receipt_id: content_id(&content)?,
            content,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    pub(super) fn for_node(
        file: &CapturedFile,
        id: &SourceId,
        node: &Node,
    ) -> Result<Self, CapturedError> {
        Self::mint(
            file,
            id,
            FactKey::Node {
                id: node.id.clone(),
            },
            node_digest(node).map_err(|_| CapturedError::Invalid("fact digest"))?,
            &node.props,
        )
    }

    pub(super) fn for_edge(
        file: &CapturedFile,
        id: &SourceId,
        edge: &Edge,
    ) -> Result<Self, CapturedError> {
        Self::mint(
            file,
            id,
            FactKey::Edge {
                source: edge.src.clone(),
                label: edge.label.clone(),
                destination: edge.dst.clone(),
            },
            edge_digest(edge).map_err(|_| CapturedError::Invalid("fact digest"))?,
            &edge.props,
        )
    }

    /// Immutable receipt identity, distinct from the complete emitted fact hash.
    pub fn id(&self) -> &str {
        &self.receipt_id
    }
    /// Registered logical source; not physical directory continuity.
    pub fn source_id(&self) -> &source_capture::SourceId {
        &self.content.source_id
    }
    /// Exact registered graph namespace.
    pub fn repo_key(&self) -> &str {
        &self.content.repo_key
    }
    /// Full primary-file capture binding.
    pub fn file(&self) -> &CaptureFileRef {
        &self.content.file
    }
    /// Typed node or directed-edge identity.
    pub fn fact_key(&self) -> &FactKey {
        &self.content.fact_key
    }
    /// Canonical digest of the complete original emitted fact.
    pub fn fact_digest(&self) -> &str {
        &self.content.fact_digest
    }
    /// Complete ordered source inventory; no adjustable caller ranges.
    pub fn ranges(&self) -> &[ReceiptRange] {
        &self.content.ranges
    }
    /// Grammar used for these actual retained bytes.
    pub fn grammar(&self) -> Grammar {
        self.content.grammar
    }

    /// Bounded wire reader with fixed diagnostics, including validating serde.
    pub fn from_json(json: &str) -> Result<Self, CapturedError> {
        if json.len() > MAX_RECEIPT_BYTES {
            return Err(CapturedError::Limit("receipt bytes"));
        }
        serde_json::from_str(json).map_err(|_| CapturedError::Invalid("receipt metadata"))
    }

    /// Serialize the complete immutable record, never original source text.
    pub fn to_json(&self) -> Result<String, CapturedError> {
        self.validate()?;
        serde_json::to_string(self).map_err(|_| CapturedError::Invalid("receipt serialization"))
    }

    /// Validate content identity/shape only, not producer trust or semantic truth.
    pub fn validate(&self) -> Result<(), CapturedError> {
        let content = &self.content;
        validate_identity(&content.file, &content.repo_key)?;
        let contract = contract(content.schema_version)?;
        if content.source_id != content.file.source_id
            || content.producer_contract != contract.producer
            || content.grammar_package != contract.grammar_package
            || content.parser_package != contract.parser_package
            || content.grammar != Grammar::for_path(&content.file.path)
        {
            return Err(CapturedError::Invalid("receipt contract"));
        }
        let (identities, digest_prefix) = match &content.fact_key {
            FactKey::Node { id } => (vec![id.as_str()], "node-v1:"),
            FactKey::Edge {
                source,
                label,
                destination,
            } => (
                vec![source.as_str(), label.as_str(), destination.as_str()],
                "edge-v1:",
            ),
        };
        if identities
            .iter()
            .any(|value| !bounded_identity(value, MAX_ID_BYTES))
            || !hash_has_prefix(&content.fact_digest, digest_prefix)
            || !hash_has_prefix(&self.receipt_id, contract.prefix)
        {
            return Err(CapturedError::Invalid("receipt fact identity"));
        }
        if content.ranges.is_empty() || content.ranges.len() > MAX_RECEIPT_RANGES {
            return Err(CapturedError::Limit("receipt ranges"));
        }
        if !valid_inventory_shape(&content.ranges, content.schema_version) {
            return Err(CapturedError::Invalid("receipt range inventory order"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for range in &content.ranges {
            let evidence = &range.evidence;
            if range.captured.file != content.file
                || evidence.repo != content.repo_key
                || evidence.path != content.file.path
                || !bounded_identity(&evidence.commit_sha, MAX_ID_BYTES)
                || evidence.commit_sha != content.ranges[0].evidence.commit_sha
                || evidence.byte_start != range.captured.byte_start
                || evidence.byte_end != range.captured.byte_end
                || evidence.byte_start >= evidence.byte_end
                || evidence.byte_end > content.file.byte_len
                || evidence.byte_end - evidence.byte_start > source_capture::MAX_SPAN_BYTES
                || !seen.insert((range.role, range.index))
            {
                return Err(CapturedError::Invalid("receipt source range"));
            }
        }
        if serde_json::to_vec(self)
            .map_err(|_| CapturedError::Invalid("receipt serialization"))?
            .len()
            > MAX_RECEIPT_BYTES
        {
            return Err(CapturedError::Limit("receipt bytes"));
        }
        if self.receipt_id != content_id(content)? {
            return Err(CapturedError::Invalid("receipt content identity"));
        }
        Ok(())
    }

    fn matches_inventory(&self, props: &serde_json::Value) -> bool {
        inventory(props, self.content.schema_version).is_ok_and(|ranges| {
            ranges.len() == self.content.ranges.len()
                && ranges.iter().zip(&self.content.ranges).all(
                    |((role, index, evidence), range)| {
                        *role == range.role && *index == range.index && *evidence == range.evidence
                    },
                )
        })
    }

    /// Full content/range match after directory or host enrichment; no trust grant.
    pub fn matches_node(&self, node: &Node) -> bool {
        self.content.fact_key
            == (FactKey::Node {
                id: node.id.clone(),
            })
            && node_digest(node).is_ok_and(|digest| digest == self.content.fact_digest)
            && self.matches_inventory(&node.props)
    }

    /// Full content/range match for the original directed relationship.
    pub fn matches_edge(&self, edge: &Edge) -> bool {
        self.content.fact_key
            == (FactKey::Edge {
                source: edge.src.clone(),
                label: edge.label.clone(),
                destination: edge.dst.clone(),
            })
            && edge_digest(edge).is_ok_and(|digest| digest == self.content.fact_digest)
            && self.matches_inventory(&edge.props)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rehashed_invalid_receipt_contracts_and_ranges_are_rejected() {
        // AC-0149: validation rejects invalid content even when its hash has
        // been recomputed; a self-consistent digest is not producer authority.
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("source.ts"),
            b"function run(ok: boolean) { if (ok) return false; }",
        )
        .unwrap();
        let source = source_capture::SourceId::new("src_11111111111111111111111111111111").unwrap();
        let capture = source_capture::capture_working_tree(
            root.path(),
            &source,
            &["source.ts".into()],
            source_capture::CaptureLimits::default(),
        )
        .unwrap();
        let (_, receipts) = super::super::extract_file(
            capture.file("source.ts").unwrap(),
            &SourceId {
                repo: "acme/project",
                commit: "workdir",
            },
        )
        .unwrap();
        let original = receipts
            .iter()
            .find(|receipt| receipt.ranges().len() > 1)
            .unwrap();
        let cases: [fn(&mut Content); 9] = [
            |content| content.producer_contract = "t0.adapter-ts/direct-lexical-v1".into(),
            |content| content.grammar = Grammar::Tsx,
            |content| content.ranges[0].index = 1,
            |content| content.ranges.swap(0, 1),
            |content| {
                content.ranges[0].evidence.byte_end = content.file.byte_len + 1;
                content.ranges[0].captured.byte_end = content.file.byte_len + 1;
            },
            |content| content.ranges[0].captured.file.digest = "0".repeat(64),
            |content| content.ranges[1].evidence.commit_sha = "different".into(),
            |content| content.ranges = vec![content.ranges[0].clone(); MAX_RECEIPT_RANGES + 1],
            |content| {
                content.fact_key = FactKey::Node {
                    id: "x".repeat(MAX_ID_BYTES + 1),
                }
            },
        ];
        for change in cases {
            let mut receipt = original.clone();
            change(&mut receipt.content);
            receipt.receipt_id = content_id(&receipt.content).unwrap();
            assert!(receipt.validate().is_err());
            assert!(Receipt::from_json(&serde_json::to_string(&receipt).unwrap()).is_err());
        }
        // Omission also fails against the actual complete fact, even if an
        // internally valid provenance-only inventory was rehashed by a caller.
        let (facts, _) = super::super::extract_file(
            capture.file("source.ts").unwrap(),
            &SourceId {
                repo: "acme/project",
                commit: "workdir",
            },
        )
        .unwrap();
        let mut omitted = original.clone();
        omitted
            .content
            .ranges
            .retain(|range| range.role == RangeRole::Provenance);
        omitted.receipt_id = content_id(&omitted.content).unwrap();
        omitted.validate().unwrap();
        let node = facts
            .nodes
            .iter()
            .find(|node| FactKey::from_node(node) == *omitted.fact_key())
            .unwrap();
        assert!(!omitted.matches_node(node));
    }
}

#[cfg(test)]
mod v2_tests;
