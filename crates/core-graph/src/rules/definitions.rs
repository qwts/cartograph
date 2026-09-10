//! Bounded source structure for directly initialized local bindings (SPEC-08).

use super::*;
use std::collections::{BTreeMap, BTreeSet};

/// Maximum observed local declarations per rule.
pub const MAX_LOCAL_DEFINITIONS: usize = 16;
/// Maximum arena nodes across every definition in one rule.
pub const MAX_DEFINITION_NODES: usize = 64;
/// Maximum initializer dependencies across every definition in one rule.
pub const MAX_INITIALIZER_DEPENDENCIES: usize = 128;
/// Maximum expression root-to-leaf depth, counting the root as one.
pub const MAX_DEFINITION_EXPRESSION_DEPTH: usize = 8;
/// Maximum definition chain, counting the condition's direct definition as one.
pub const MAX_DEFINITION_CHAIN_DEPTH: usize = 4;
/// Maximum complete serialized v2 payload, including all source metadata.
pub const MAX_RULE_PAYLOAD_BYTES: usize = 64 * 1024;

const MAX_TEXT_BYTES: usize = 8 * 1024;
const MAX_ID_BYTES: usize = 4096;
const MAX_USES: usize = 256;

/// An initializer as written, not a substituted value or execution predicate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalDefinition {
    /// Existing scope/declaration identity, never a source-derived value.
    pub binding_id: String,
    /// Original directly initialized declaration.
    pub declaration: EvidenceRef,
    /// Original condition/initializer uses, in strict lexical order.
    pub uses: Vec<EvidenceRef>,
    /// Original sanitized initializer, equal to the arena root expression.
    pub initializer: SourceExpression,
    /// Bounded source structure without evaluation or textual substitution.
    pub expression: DefinitionExpression,
    /// Original initializer inputs; the container supplies their role.
    pub dependencies: Vec<DefinitionDependency>,
}

/// A prerequisite observed at its original initializer occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefinitionDependency {
    /// Original prerequisite occurrence.
    pub source: EvidenceRef,
    /// Lexical binding/target knowledge or a cited unresolved graph Gap.
    pub resolution: DependencyResolution,
}

/// A flat, reachable, acyclic source-expression arena.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefinitionExpression {
    /// Index of the initializer root.
    pub root: u32,
    /// Original expression nodes in deterministic producer order.
    pub nodes: Vec<DefinitionExpressionNode>,
}

/// A cited sanitized expression and its exact supported source shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefinitionExpressionNode {
    /// Display, literal knowledge and original source span for this node.
    pub expression: SourceExpression,
    /// Supported syntax; index references belong to this definition only.
    pub kind: DefinitionExpressionKind,
}

/// Closed source forms. No variant establishes the runtime value at use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DefinitionExpressionKind {
    /// The node's literal field retains a typed known or withheld scalar.
    Literal,
    /// An identifier or runtime input such as `this`.
    Identifier {
        /// Index of its original initializer prerequisite.
        dependency: u32,
    },
    /// A noncomputed member key; display is already sanitized.
    PropertyName,
    /// A member read, whose value remains explicitly unresolved.
    Member {
        /// Object expression index.
        object: u32,
        /// Property expression/key index.
        property: u32,
        /// Whether the property is computed with bracket syntax.
        computed: bool,
        /// Whether this member uses optional access syntax.
        optional: bool,
        /// Index of the unresolved member-value prerequisite.
        dependency: u32,
    },
    /// Parentheses retained without flattening source grouping.
    Parenthesized {
        /// Enclosed expression index.
        value: u32,
    },
    /// A supported unary source operator.
    Unary {
        /// Closed operator spelling/meaning category.
        operator: UnaryOperator,
        /// Operand expression index.
        operand: u32,
    },
    /// A supported binary source operator, without constant folding.
    Binary {
        /// Closed operator category.
        operator: BinaryOperator,
        /// Left operand index.
        left: u32,
        /// Right operand index.
        right: u32,
    },
    /// A logical source operator retaining left/right short-circuit structure.
    Logical {
        /// Closed logical operator category.
        operator: LogicalOperator,
        /// Left operand index.
        left: u32,
        /// Right operand index.
        right: u32,
    },
    /// Explicitly unsupported source, never a silently truncated complete tree.
    Unsupported {
        /// An interpretation Gap explaining the unsupported source form.
        gap_id: String,
    },
}

/// Supported unary JavaScript syntax; delete and updates are not admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnaryOperator {
    /// Logical `!`.
    Not,
    /// Unary `+`.
    Plus,
    /// Unary `-`.
    Minus,
    /// Bitwise `~`.
    BitwiseNot,
    /// `typeof`.
    Typeof,
    /// `void`.
    Void,
}

/// Supported binary JavaScript source operators; none evaluates the operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BinaryOperator {
    /// Coercive `==`, distinct from strict equality.
    LooseEqual,
    /// Coercive `!=`, distinct from strict inequality.
    LooseNotEqual,
    /// Strict `===`.
    StrictEqual,
    /// Strict `!==`.
    StrictNotEqual,
    /// `<`.
    LessThan,
    /// `<=`.
    LessThanOrEqual,
    /// `>`.
    GreaterThan,
    /// `>=`.
    GreaterThanOrEqual,
    /// `+`, without choosing numeric or string behavior.
    Add,
    /// `-`.
    Subtract,
    /// `*`.
    Multiply,
    /// `/`.
    Divide,
    /// `%`.
    Remainder,
    /// `**`.
    Exponent,
    /// `<<`.
    LeftShift,
    /// `>>`.
    RightShift,
    /// `>>>`.
    UnsignedRightShift,
    /// `&`.
    BitwiseAnd,
    /// `|`.
    BitwiseOr,
    /// `^`.
    BitwiseXor,
    /// `in`, without interpreting property membership.
    In,
    /// `instanceof`, without interpreting constructor behavior.
    Instanceof,
}

/// Source short-circuit forms, without rewriting truthiness or nullishness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogicalOperator {
    /// `&&`.
    And,
    /// `||`.
    Or,
    /// `??`.
    Nullish,
}

pub(super) fn present_definitions<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<LocalDefinition>>, D::Error> {
    // Missing uses serde(default). A present null must not normalize to absence.
    Vec::<LocalDefinition>::deserialize(deserializer).map(Some)
}

fn invalid(field: &'static str) -> RuleValidationError {
    RuleValidationError::InvalidDefinition(field)
}

fn limit(field: &'static str) -> RuleValidationError {
    RuleValidationError::Limit(field)
}

/// Counts serialized bytes without allocating a second full payload.
struct ByteBudget(usize);
impl std::io::Write for ByteBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| std::io::Error::other("rule byte limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn serialized_bound(value: &impl Serialize) -> Result<(), RuleValidationError> {
    serde_json::to_writer(ByteBudget(MAX_RULE_PAYLOAD_BYTES), value)
        .map_err(|_| limit("payload_bytes"))
}

/// Check a v2 Value before allocating its typed collections or cloning strings.
pub(super) fn preflight(value: &serde_json::Value) -> Result<(), RuleValidationError> {
    let mut pending = vec![(value, 0_usize)];
    let mut visited = 0_usize;
    while let Some((value, depth)) = pending.pop() {
        visited += 1;
        if depth > 32 || visited > 16_384 {
            return Err(limit("wire_shape"));
        }
        match value {
            serde_json::Value::String(text) if text.len() > MAX_TEXT_BYTES => {
                return Err(limit("wire_text"));
            }
            serde_json::Value::Array(values) => {
                if values.len() > 1024 || visited + pending.len() + values.len() > 16_384 {
                    return Err(limit("wire_shape"));
                }
                pending.extend(values.iter().map(|value| (value, depth + 1)));
            }
            serde_json::Value::Object(values) => {
                if values.len() > 64 || values.keys().any(|key| key.len() > 128) {
                    return Err(limit("wire_shape"));
                }
                if (values.contains_key("byte_start") || values.contains_key("byte_end"))
                    && (values.len() != 5
                        || ["repo", "path", "byte_start", "byte_end", "commit_sha"]
                            .iter()
                            .any(|key| !values.contains_key(*key)))
                {
                    return Err(invalid("source_wire"));
                }
                pending.extend(values.values().map(|value| (value, depth + 1)));
            }
            _ => {}
        }
    }
    let definitions = value
        .get("local_definitions")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| invalid("local_definitions"))?;
    if definitions.len() > MAX_LOCAL_DEFINITIONS {
        return Err(limit("definitions"));
    }
    let mut nodes = 0;
    let mut dependencies = 0;
    let mut uses = 0;
    for definition in definitions {
        nodes += definition
            .get("expression")
            .and_then(|value| value.get("nodes"))
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len);
        dependencies += definition
            .get("dependencies")
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len);
        uses += definition
            .get("uses")
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len);
    }
    if nodes > MAX_DEFINITION_NODES
        || dependencies > MAX_INITIALIZER_DEPENDENCIES
        || uses > MAX_USES
    {
        return Err(limit("definition_collections"));
    }
    serialized_bound(value)
}

pub(super) fn collection_bounds(rule: &GuardedExitEvidence) -> Result<(), RuleValidationError> {
    let definitions = rule
        .local_definitions
        .as_ref()
        .ok_or_else(|| invalid("local_definitions"))?;
    if definitions.len() > MAX_LOCAL_DEFINITIONS
        || rule.conditions.len() > 32
        || rule.dependencies.len() > 128
        || rule.redactions.len() > 1024
        || rule.interpretation.gap_ids.len() > 128
    {
        return Err(limit("rule_collections"));
    }
    let nodes: usize = definitions.iter().map(|d| d.expression.nodes.len()).sum();
    let dependencies: usize = definitions.iter().map(|d| d.dependencies.len()).sum();
    let uses: usize = definitions.iter().map(|d| d.uses.len()).sum();
    if nodes > MAX_DEFINITION_NODES
        || dependencies > MAX_INITIALIZER_DEPENDENCIES
        || uses > MAX_USES
    {
        return Err(limit("definition_collections"));
    }
    Ok(())
}

fn bounded_id(value: &str) -> Result<(), RuleValidationError> {
    if value.is_empty() || value.len() > MAX_ID_BYTES || value.chars().any(char::is_control) {
        return Err(invalid("identity"));
    }
    Ok(())
}

fn same_source(a: &EvidenceRef, b: &EvidenceRef) -> bool {
    a.repo == b.repo && a.path == b.path && a.commit_sha == b.commit_sha
}

fn contains(parent: &EvidenceRef, child: &EvidenceRef) -> bool {
    same_source(parent, child)
        && parent.byte_start <= child.byte_start
        && child.byte_end <= parent.byte_end
}

fn checked_source(
    reference: &EvidenceRef,
    primary: &EvidenceRef,
) -> Result<(), RuleValidationError> {
    span(reference, "definition.source")?;
    for identity in [&reference.repo, &reference.path, &reference.commit_sha] {
        bounded_id(identity)?;
    }
    if !same_source(reference, primary) {
        return Err(invalid("source_identity"));
    }
    Ok(())
}

fn checked_expression(
    value: &SourceExpression,
    primary: &EvidenceRef,
) -> Result<(), RuleValidationError> {
    expression(value)?;
    checked_source(&value.source, primary)?;
    if value.syntax_kind.is_empty()
        || value.syntax_kind.len() > 64
        || value.display.as_str().len() > MAX_TEXT_BYTES
    {
        return Err(limit("expression_text"));
    }
    if let Some(LiteralEvidence::Known {
        value: KnownLiteral::String(value),
    }) = &value.literal
        && value.len() > MAX_TEXT_BYTES
    {
        return Err(limit("literal_text"));
    }
    if let Some(LiteralEvidence::Withheld { reasons, .. }) = &value.literal
        && reasons.len() > 16
    {
        return Err(limit("literal_reasons"));
    }
    Ok(())
}

fn checked_resolution(
    value: &DependencyResolution,
    primary: &EvidenceRef,
    gaps: &BTreeSet<&str>,
) -> Result<(), RuleValidationError> {
    match value {
        DependencyResolution::Binding {
            binding_id,
            declaration,
        } => {
            bounded_id(binding_id)?;
            checked_source(declaration, primary)?;
        }
        DependencyResolution::Target { node_id } => bounded_id(node_id)?,
        DependencyResolution::Unresolved { gap_id } => {
            bounded_id(gap_id)?;
            if !gaps.contains(gap_id.as_str()) {
                return Err(invalid("gap_reference"));
            }
        }
    }
    Ok(())
}

pub(super) fn validate(rule: &GuardedExitEvidence) -> Result<(), RuleValidationError> {
    collection_bounds(rule)?;
    bounded_id(&rule.owner_id)?;
    checked_source(&rule.exit_source, &rule.exit_source)?;
    let mut gaps = BTreeSet::new();
    for gap in &rule.interpretation.gap_ids {
        bounded_id(gap)?;
        if !gaps.insert(gap.as_str()) {
            return Err(invalid("duplicate_gap"));
        }
    }
    for condition in &rule.conditions {
        checked_source(&condition.branch_source, &rule.exit_source)?;
        checked_expression(&condition.expression, &rule.exit_source)?;
        if !contains(&condition.branch_source, &condition.expression.source) {
            return Err(invalid("condition_span"));
        }
    }
    match &rule.effect {
        LocalExit::Return { value: Some(value) } | LocalExit::Throw { value } => {
            checked_expression(value, &rule.exit_source)?
        }
        LocalExit::Return { value: None } => {}
    }
    for dependency in &rule.dependencies {
        checked_source(&dependency.source, &rule.exit_source)?;
        checked_resolution(&dependency.resolution, &rule.exit_source, &gaps)?;
    }
    for redaction in &rule.redactions {
        checked_source(&redaction.source, &rule.exit_source)?;
    }
    let definitions = rule
        .local_definitions
        .as_deref()
        .ok_or_else(|| invalid("local_definitions"))?;
    let mut indices = BTreeMap::new();
    let mut declarations = BTreeSet::new();
    for (index, definition) in definitions.iter().enumerate() {
        bounded_id(&definition.binding_id)?;
        if indices
            .insert(definition.binding_id.as_str(), index)
            .is_some()
            || !declarations.insert((
                definition.declaration.byte_start,
                definition.declaration.byte_end,
            ))
        {
            return Err(invalid("duplicate_definition"));
        }
        checked_source(&definition.declaration, &rule.exit_source)?;
        checked_expression(&definition.initializer, &rule.exit_source)?;
        if !contains(&definition.declaration, &definition.initializer.source) {
            return Err(invalid("initializer_span"));
        }
        if definition.uses.is_empty() {
            return Err(invalid("definition_uses"));
        }
        let mut previous = None;
        for usage in &definition.uses {
            checked_source(usage, &rule.exit_source)?;
            let key = (usage.byte_start, usage.byte_end);
            if previous.is_some_and(|prior| prior >= key)
                || definition.declaration.byte_end > usage.byte_start
            {
                return Err(invalid("definition_uses"));
            }
            previous = Some(key);
        }
        for dependency in &definition.dependencies {
            checked_source(&dependency.source, &rule.exit_source)?;
            if !contains(&definition.initializer.source, &dependency.source) {
                return Err(invalid("dependency_span"));
            }
            checked_resolution(&dependency.resolution, &rule.exit_source, &gaps)?;
        }
        arena(definition, &rule.exit_source, &gaps)?;
    }
    links(rule, definitions, &indices)?;
    serialized_bound(rule)
}

fn arena(
    definition: &LocalDefinition,
    primary: &EvidenceRef,
    gaps: &BTreeSet<&str>,
) -> Result<(), RuleValidationError> {
    let arena = &definition.expression;
    let root = usize::try_from(arena.root).map_err(|_| invalid("arena_root"))?;
    if arena
        .nodes
        .get(root)
        .is_none_or(|node| node.expression != definition.initializer)
    {
        return Err(invalid("initializer_root"));
    }
    let mut active = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut used_dependencies = BTreeSet::new();
    let mut pending = vec![(root, 1_usize, false)];
    while let Some((index, depth, leaving)) = pending.pop() {
        if leaving {
            active.remove(&index);
            continue;
        }
        if depth > MAX_DEFINITION_EXPRESSION_DEPTH {
            return Err(limit("expression_depth"));
        }
        if !active.insert(index) || !visited.insert(index) {
            return Err(invalid("arena_cycle_or_reuse"));
        }
        let node = arena
            .nodes
            .get(index)
            .ok_or_else(|| invalid("arena_index"))?;
        checked_expression(&node.expression, primary)?;
        let mut dependency = |index: u32| -> Result<&DefinitionDependency, RuleValidationError> {
            if !used_dependencies.insert(index as usize) {
                return Err(invalid("reused_dependency"));
            }
            let dependency = definition
                .dependencies
                .get(index as usize)
                .ok_or_else(|| invalid("dependency_index"))?;
            if dependency.source != node.expression.source {
                return Err(invalid("dependency_occurrence"));
            }
            Ok(dependency)
        };
        let mut children = Vec::new();
        let syntax_ok = match &node.kind {
            DefinitionExpressionKind::Literal => matches!(
                node.expression.syntax_kind.as_str(),
                "true" | "false" | "null" | "string" | "number"
            ),
            DefinitionExpressionKind::Identifier { .. } => matches!(
                node.expression.syntax_kind.as_str(),
                "identifier" | "shorthand_property_identifier" | "this"
            ),
            DefinitionExpressionKind::PropertyName => matches!(
                node.expression.syntax_kind.as_str(),
                "property_identifier" | "identifier"
            ),
            DefinitionExpressionKind::Member { computed, .. } => {
                node.expression.syntax_kind
                    == if *computed {
                        "subscript_expression"
                    } else {
                        "member_expression"
                    }
            }
            DefinitionExpressionKind::Parenthesized { .. } => {
                node.expression.syntax_kind == "parenthesized_expression"
            }
            DefinitionExpressionKind::Unary { .. } => {
                node.expression.syntax_kind == "unary_expression"
            }
            DefinitionExpressionKind::Binary { .. } | DefinitionExpressionKind::Logical { .. } => {
                node.expression.syntax_kind == "binary_expression"
            }
            DefinitionExpressionKind::Unsupported { .. } => true,
        };
        if !syntax_ok {
            return Err(invalid("expression_kind"));
        }
        match &node.kind {
            DefinitionExpressionKind::Literal => {
                let coherent = match &node.expression.literal {
                    Some(LiteralEvidence::Known { value }) => match value {
                        KnownLiteral::Boolean(true) => node.expression.syntax_kind == "true",
                        KnownLiteral::Boolean(false) => node.expression.syntax_kind == "false",
                        KnownLiteral::String(_) => node.expression.syntax_kind == "string",
                        KnownLiteral::Number(_) => node.expression.syntax_kind == "number",
                        KnownLiteral::Null => node.expression.syntax_kind == "null",
                    },
                    Some(LiteralEvidence::Withheld { kind, .. }) => match kind {
                        LiteralKind::Boolean => {
                            matches!(node.expression.syntax_kind.as_str(), "true" | "false")
                        }
                        LiteralKind::String => node.expression.syntax_kind == "string",
                        LiteralKind::Number => node.expression.syntax_kind == "number",
                        LiteralKind::Null => node.expression.syntax_kind == "null",
                    },
                    None => false,
                };
                if !coherent {
                    return Err(invalid("literal_node"));
                }
            }
            DefinitionExpressionKind::Identifier { dependency: index } => {
                let input = dependency(*index)?;
                if node.expression.syntax_kind == "this"
                    && !matches!(input.resolution, DependencyResolution::Unresolved { .. })
                {
                    return Err(invalid("runtime_input"));
                }
            }
            DefinitionExpressionKind::PropertyName => {}
            DefinitionExpressionKind::Member {
                object,
                property,
                computed,
                dependency: index,
                ..
            } => {
                if !matches!(
                    dependency(*index)?.resolution,
                    DependencyResolution::Unresolved { .. }
                ) {
                    return Err(invalid("member_value"));
                }
                let property_node = arena
                    .nodes
                    .get(*property as usize)
                    .ok_or_else(|| invalid("property_index"))?;
                if *computed == matches!(property_node.kind, DefinitionExpressionKind::PropertyName)
                {
                    return Err(invalid("property_kind"));
                }
                children.extend([*object, *property]);
            }
            DefinitionExpressionKind::Parenthesized { value } => children.push(*value),
            DefinitionExpressionKind::Unary { operand, .. } => children.push(*operand),
            DefinitionExpressionKind::Binary { left, right, .. }
            | DefinitionExpressionKind::Logical { left, right, .. } => {
                children.extend([*left, *right])
            }
            DefinitionExpressionKind::Unsupported { gap_id } => {
                bounded_id(gap_id)?;
                if !gaps.contains(gap_id.as_str()) {
                    return Err(invalid("unsupported_gap"));
                }
            }
        }
        let mut previous_end = None;
        for child in &children {
            let child = arena
                .nodes
                .get(*child as usize)
                .ok_or_else(|| invalid("arena_index"))?;
            if !contains(&node.expression.source, &child.expression.source)
                || previous_end.is_some_and(|end| end > child.expression.source.byte_start)
            {
                return Err(invalid("child_span"));
            }
            previous_end = Some(child.expression.source.byte_end);
        }
        pending.push((index, depth, true));
        pending.extend(
            children
                .into_iter()
                .rev()
                .map(|index| (index as usize, depth + 1, false)),
        );
    }
    if visited.len() != arena.nodes.len() {
        return Err(invalid("unreachable_expression"));
    }
    if used_dependencies.len() != definition.dependencies.len() {
        return Err(invalid("unused_dependency"));
    }
    Ok(())
}

fn links(
    rule: &GuardedExitEvidence,
    definitions: &[LocalDefinition],
    indices: &BTreeMap<&str, usize>,
) -> Result<(), RuleValidationError> {
    let mut uses = vec![BTreeSet::new(); definitions.len()];
    let mut edges = vec![BTreeSet::new(); definitions.len()];
    let mut seeds = BTreeSet::new();
    let mut observe = |source: &EvidenceRef,
                       resolution: &DependencyResolution,
                       parent: Option<usize>|
     -> Result<(), RuleValidationError> {
        if let DependencyResolution::Binding {
            binding_id,
            declaration,
        } = resolution
            && let Some(index) = indices.get(binding_id.as_str()).copied()
        {
            if *declaration != definitions[index].declaration {
                return Err(invalid("binding_declaration"));
            }
            // Binding knowledge can survive an ineligible self/TDZ use. Only
            // the producer's admitted, independently justified use edges link
            // definitions; omitted uses must not fabricate a recovered cycle.
            if !definitions[index].uses.iter().any(|usage| usage == source) {
                return Ok(());
            }
            uses[index].insert((source.byte_start, source.byte_end));
            match parent {
                Some(parent) => {
                    edges[parent].insert(index);
                }
                None => {
                    seeds.insert(index);
                }
            }
        }
        Ok(())
    };
    for dependency in &rule.dependencies {
        if dependency.role == DependencyRole::Condition {
            observe(&dependency.source, &dependency.resolution, None)?;
        }
    }
    for (index, definition) in definitions.iter().enumerate() {
        for dependency in &definition.dependencies {
            observe(&dependency.source, &dependency.resolution, Some(index))?;
        }
    }
    for (index, definition) in definitions.iter().enumerate() {
        let actual: BTreeSet<_> = definition
            .uses
            .iter()
            .map(|source| (source.byte_start, source.byte_end))
            .collect();
        if uses[index] != actual {
            return Err(invalid("unjustified_use"));
        }
    }
    let mut seen = BTreeSet::new();
    let mut pending: Vec<_> = seeds
        .into_iter()
        .map(|index| (index, Vec::<usize>::new()))
        .collect();
    while let Some((index, mut path)) = pending.pop() {
        if path.contains(&index) {
            return Err(invalid("definition_cycle"));
        }
        path.push(index);
        if path.len() > MAX_DEFINITION_CHAIN_DEPTH {
            return Err(limit("definition_chain"));
        }
        seen.insert(index);
        pending.extend(
            edges[index]
                .iter()
                .copied()
                .map(|child| (child, path.clone())),
        );
    }
    if seen.len() != definitions.len() {
        return Err(invalid("unreachable_definition"));
    }
    Ok(())
}
