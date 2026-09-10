//! Bounded lexical initializer evidence. Nothing here substitutes or evaluates a value.

use super::{
    FileCx, TsNode, callable,
    rule_evidence::{capture, source},
};
use core_graph::rules::*;
use std::collections::BTreeMap;

const MAX_ANCESTORS: usize = 256;
const MAX_INITIALIZER_BYTES: usize = 8 * 1024;
const MAX_WORK: usize = 512;

pub(super) fn recover<'tree>(
    cx: &FileCx<'_>,
    bindings: &callable::Bindings<'tree>,
    owner: &str,
    seeds: &[TsNode<'tree>],
    gap: &mut impl FnMut(RuleGapReason, TsNode<'tree>) -> String,
) -> (Vec<LocalDefinition>, Vec<SourceRedaction>) {
    let mut recovery = Recovery {
        cx,
        bindings,
        owner,
        gap,
        definitions: vec![],
        indices: BTreeMap::new(),
        redactions: vec![],
        work: 0,
        nodes: 0,
        ast_visits: 0,
        dependencies: 0,
    };
    for &seed in seeds {
        recovery.definition(seed, 1);
    }
    for definition in &mut recovery.definitions {
        definition
            .uses
            .sort_by_key(|source| (source.byte_start, source.byte_end));
        definition.uses.dedup();
    }
    (recovery.definitions, recovery.redactions)
}

struct Recovery<'a, 'tree, G> {
    cx: &'a FileCx<'a>,
    bindings: &'a callable::Bindings<'tree>,
    owner: &'a str,
    gap: &'a mut G,
    definitions: Vec<LocalDefinition>,
    indices: BTreeMap<String, usize>,
    redactions: Vec<SourceRedaction>,
    work: usize,
    nodes: usize,
    ast_visits: usize,
    dependencies: usize,
}

impl<'tree, G: FnMut(RuleGapReason, TsNode<'tree>) -> String> Recovery<'_, 'tree, G> {
    fn spend(&mut self, at: TsNode<'tree>) -> bool {
        if self.work >= MAX_WORK {
            (self.gap)(RuleGapReason::DefinitionLimit, at);
            false
        } else {
            self.work += 1;
            true
        }
    }

    fn definition(&mut self, site: TsNode<'tree>, chain: usize) {
        if !self.spend(site) {
            return;
        }
        let (declaration, initializer) = match self.eligible(site) {
            Ok(value) => value,
            Err(reason) => {
                (self.gap)(reason, site);
                return;
            }
        };
        let binding_id = binding_id(self.cx, declaration);
        if chain > MAX_DEFINITION_CHAIN_DEPTH {
            (self.gap)(RuleGapReason::DefinitionLimit, site);
            return;
        }
        if let Some(&index) = self.indices.get(&binding_id) {
            // Reusing an already expanded definition must not smuggle its
            // descendants past the current condition's chain-depth bound.
            if chain + self.height(index, 1) - 1 > MAX_DEFINITION_CHAIN_DEPTH {
                (self.gap)(RuleGapReason::DefinitionLimit, site);
                return;
            }
            self.definitions[index].uses.push(source(self.cx, site));
            return;
        }
        if self.definitions.len() >= MAX_LOCAL_DEFINITIONS || self.nodes >= MAX_DEFINITION_NODES {
            (self.gap)(RuleGapReason::DefinitionLimit, site);
            return;
        }
        let (initializer_expression, mut redactions) = capture(self.cx, initializer);
        self.redactions.append(&mut redactions);
        let mut arena = Arena {
            nodes: vec![],
            dependencies: vec![],
            reads: vec![],
        };
        // Failed bounded traversal is represented by an unsupported whole
        // initializer. Discard its partial tree and dependencies together.
        let root = self.expression(initializer, 1, &mut arena);
        let root = match root {
            Ok(root) => root,
            Err(()) => {
                arena = Arena {
                    nodes: vec![],
                    dependencies: vec![],
                    reads: vec![],
                };
                let gap_id = (self.gap)(RuleGapReason::DefinitionLimit, initializer);
                arena.nodes.push(DefinitionExpressionNode {
                    expression: initializer_expression.clone(),
                    kind: DefinitionExpressionKind::Unsupported { gap_id },
                });
                0
            }
        };
        self.nodes += arena.nodes.len();
        self.dependencies += arena.dependencies.len();
        let reads = arena.reads;
        self.indices
            .insert(binding_id.clone(), self.definitions.len());
        self.definitions.push(LocalDefinition {
            binding_id,
            declaration: source(self.cx, declaration),
            uses: vec![source(self.cx, site)],
            initializer: initializer_expression,
            expression: DefinitionExpression {
                root,
                nodes: arena.nodes,
            },
            dependencies: arena.dependencies,
        });
        for read in reads {
            self.definition(read, chain + 1);
        }
    }

    fn height(&self, index: usize, depth: usize) -> usize {
        if depth > MAX_DEFINITION_CHAIN_DEPTH {
            return depth;
        }
        self.definitions[index]
            .dependencies
            .iter()
            .filter_map(|dependency| {
                let DependencyResolution::Binding { binding_id, .. } = &dependency.resolution
                else {
                    return None;
                };
                let index = *self.indices.get(binding_id)?;
                self.definitions[index]
                    .uses
                    .contains(&dependency.source)
                    .then(|| self.height(index, depth + 1))
            })
            .max()
            .unwrap_or(depth)
    }

    fn eligible(
        &self,
        site: TsNode<'tree>,
    ) -> Result<(TsNode<'tree>, TsNode<'tree>), RuleGapReason> {
        if site.kind() != "identifier" || site.byte_range().len() > MAX_INITIALIZER_BYTES {
            return Err(RuleGapReason::DefinitionBindingUnsupported);
        }
        // Bound ancestor work before the existing actual-callable resolver.
        bounded_ancestors(site)?;
        supported_owner_path(site)?;
        let (declaration, invalidated) = self
            .bindings
            .definition_binding(site, self.cx.text(&site))
            .map_err(|()| RuleGapReason::DefinitionLimit)?
            .ok_or(RuleGapReason::UnresolvedBinding)?;
        if invalidated {
            return Err(RuleGapReason::DefinitionBindingUnsupported);
        }
        bounded_ancestors(declaration)?;
        if callable::enclosing(self.cx, site).as_deref() != Some(self.owner)
            || callable::enclosing(self.cx, declaration).as_deref() != Some(self.owner)
        {
            return Err(RuleGapReason::DefinitionScopeUnsupported);
        }
        let statement = declaration
            .parent()
            .ok_or(RuleGapReason::DefinitionBindingUnsupported)?;
        if declaration.kind() != "variable_declarator"
            || statement.kind() != "lexical_declaration"
            || statement
                .child(0)
                .is_none_or(|keyword| keyword.kind() != "const")
            || declaration
                .child_by_field_name("name")
                .is_none_or(|name| name.kind() != "identifier")
            || declaration.has_error()
        {
            return Err(RuleGapReason::DefinitionBindingUnsupported);
        }
        let initializer = declaration
            .child_by_field_name("value")
            .ok_or(RuleGapReason::DefinitionBindingUnsupported)?;
        let block = statement
            .parent()
            .filter(|node| node.kind() == "statement_block")
            .ok_or(RuleGapReason::DefinitionScopeUnsupported)?;
        // Reject unsupported control regions on BOTH paths, even when they
        // contain the common block (e.g. two statements inside one loop body).
        supported_owner_path(declaration)?;
        let mut child = site;
        for _ in 0..MAX_ANCESTORS {
            let parent = child
                .parent()
                .ok_or(RuleGapReason::DefinitionOrderUnproven)?;
            if parent == block {
                return if child != statement && statement.end_byte() <= child.start_byte() {
                    Ok((declaration, initializer))
                } else {
                    Err(RuleGapReason::DefinitionOrderUnproven)
                };
            }
            if callable::is_callable(parent) {
                return Err(RuleGapReason::DefinitionOrderUnproven);
            }
            child = parent;
        }
        Err(RuleGapReason::DefinitionLimit)
    }

    fn expression(
        &mut self,
        node: TsNode<'tree>,
        depth: usize,
        arena: &mut Arena<'tree>,
    ) -> Result<u32, ()> {
        if !self.spend(node)
            || depth > MAX_DEFINITION_EXPRESSION_DEPTH
            || self.nodes + arena.nodes.len() >= MAX_DEFINITION_NODES
            || self.ast_visits >= MAX_DEFINITION_NODES
        {
            return Err(());
        }
        // Failed arenas consume traversal budget too: unsupported wide inputs
        // cannot repeatedly spend the full node budget and retain one leaf.
        self.ast_visits += 1;
        let (expression, mut redactions) = capture(self.cx, node);
        self.redactions.append(&mut redactions);
        if expression.capture != ExpressionCapture::CompleteSyntax {
            (self.gap)(RuleGapReason::RedactedExpression, node);
        }
        let index = arena.nodes.len() as u32;
        // Placeholder becomes reachable only after this entire bounded arena
        // succeeds. No partial arena is ever published.
        arena.nodes.push(DefinitionExpressionNode {
            expression,
            kind: DefinitionExpressionKind::Literal,
        });
        let kind = if node.byte_range().len() > MAX_INITIALIZER_BYTES || node.has_error() {
            return Err(());
        } else {
            match node.kind() {
                "true" | "false" | "null" | "number" | "string" => {
                    DefinitionExpressionKind::Literal
                }
                "identifier" | "this" => {
                    let resolution = if node.kind() == "identifier" {
                        match self.bindings.definition_binding(node, self.cx.text(&node)) {
                            Ok(Some((declaration, false))) => {
                                arena.reads.push(node);
                                DependencyResolution::Binding {
                                    binding_id: binding_id(self.cx, declaration),
                                    declaration: source(self.cx, declaration),
                                }
                            }
                            Ok(_) => DependencyResolution::Unresolved {
                                gap_id: (self.gap)(RuleGapReason::UnresolvedBinding, node),
                            },
                            Err(()) => return Err(()),
                        }
                    } else {
                        DependencyResolution::Unresolved {
                            gap_id: (self.gap)(RuleGapReason::RuntimeValueUnknown, node),
                        }
                    };
                    let dependency = self.dependency(node, resolution, arena)?;
                    DefinitionExpressionKind::Identifier { dependency }
                }
                "property_identifier" => DefinitionExpressionKind::PropertyName,
                "member_expression"
                    if node
                        .child_by_field_name("property")
                        .is_none_or(|property| property.kind() != "property_identifier") =>
                {
                    self.unsupported(node)
                }
                "member_expression" | "subscript_expression" => {
                    let object = node.child_by_field_name("object").ok_or(())?;
                    let property = node
                        .child_by_field_name("property")
                        .or_else(|| node.child_by_field_name("index"))
                        .ok_or(())?;
                    let object = self.expression(object, depth + 1, arena)?;
                    let property = self.expression(property, depth + 1, arena)?;
                    let resolution = DependencyResolution::Unresolved {
                        gap_id: (self.gap)(RuleGapReason::RuntimeValueUnknown, node),
                    };
                    let dependency = self.dependency(node, resolution, arena)?;
                    DefinitionExpressionKind::Member {
                        object,
                        property,
                        dependency,
                        computed: node.kind() == "subscript_expression",
                        optional: node.child_by_field_name("optional_chain").is_some(),
                    }
                }
                "parenthesized_expression" => {
                    let mut cursor = node.walk();
                    let value = node
                        .named_children(&mut cursor)
                        .find(|child| child.kind() != "comment")
                        .ok_or(())?;
                    DefinitionExpressionKind::Parenthesized {
                        value: self.expression(value, depth + 1, arena)?,
                    }
                }
                "unary_expression" => {
                    let operator = node
                        .child_by_field_name("operator")
                        .and_then(|operator| unary(operator.kind()));
                    if let Some(operator) = operator {
                        let operand = node.child_by_field_name("argument").ok_or(())?;
                        DefinitionExpressionKind::Unary {
                            operator,
                            operand: self.expression(operand, depth + 1, arena)?,
                        }
                    } else {
                        self.unsupported(node)
                    }
                }
                "binary_expression" => {
                    let operator = node.child_by_field_name("operator").ok_or(())?;
                    if let Some(operator) = binary(operator.kind()) {
                        let left = self.expression(
                            node.child_by_field_name("left").ok_or(())?,
                            depth + 1,
                            arena,
                        )?;
                        let right = self.expression(
                            node.child_by_field_name("right").ok_or(())?,
                            depth + 1,
                            arena,
                        )?;
                        DefinitionExpressionKind::Binary {
                            operator,
                            left,
                            right,
                        }
                    } else if let Some(operator) = logical(operator.kind()) {
                        let left = self.expression(
                            node.child_by_field_name("left").ok_or(())?,
                            depth + 1,
                            arena,
                        )?;
                        let right = self.expression(
                            node.child_by_field_name("right").ok_or(())?,
                            depth + 1,
                            arena,
                        )?;
                        DefinitionExpressionKind::Logical {
                            operator,
                            left,
                            right,
                        }
                    } else {
                        self.unsupported(node)
                    }
                }
                _ => self.unsupported(node),
            }
        };
        arena.nodes[index as usize].kind = kind;
        Ok(index)
    }

    fn unsupported(&mut self, node: TsNode<'tree>) -> DefinitionExpressionKind {
        DefinitionExpressionKind::Unsupported {
            gap_id: (self.gap)(RuleGapReason::LocalDefinitionUnsupported, node),
        }
    }

    fn dependency(
        &self,
        node: TsNode<'tree>,
        resolution: DependencyResolution,
        arena: &mut Arena<'tree>,
    ) -> Result<u32, ()> {
        if self.dependencies + arena.dependencies.len() >= MAX_INITIALIZER_DEPENDENCIES {
            return Err(());
        }
        let index = arena.dependencies.len() as u32;
        arena.dependencies.push(DefinitionDependency {
            source: source(self.cx, node),
            resolution,
        });
        Ok(index)
    }
}

struct Arena<'tree> {
    nodes: Vec<DefinitionExpressionNode>,
    dependencies: Vec<DefinitionDependency>,
    reads: Vec<TsNode<'tree>>,
}

fn binding_id(cx: &FileCx<'_>, declaration: TsNode<'_>) -> String {
    format!(
        "binding:{}@{}#declaration@{}",
        cx.id.repo,
        cx.path,
        declaration.start_byte()
    )
}

fn bounded_ancestors(mut node: TsNode<'_>) -> Result<(), RuleGapReason> {
    for _ in 0..MAX_ANCESTORS {
        let Some(parent) = node.parent() else {
            return Ok(());
        };
        node = parent;
    }
    Err(RuleGapReason::DefinitionLimit)
}

fn supported_owner_path(mut node: TsNode<'_>) -> Result<(), RuleGapReason> {
    for _ in 0..MAX_ANCESTORS {
        let parent = node
            .parent()
            .ok_or(RuleGapReason::DefinitionScopeUnsupported)?;
        if callable::is_callable(parent) {
            return Ok(());
        }
        if matches!(
            parent.kind(),
            "for_statement"
                | "for_in_statement"
                | "while_statement"
                | "do_statement"
                | "switch_statement"
                | "switch_body"
                | "switch_case"
                | "switch_default"
                | "try_statement"
                | "catch_clause"
                | "finally_clause"
                | "with_statement"
                | "class"
                | "class_declaration"
                | "class_body"
                | "class_static_block"
                | "public_field_definition"
        ) {
            return Err(RuleGapReason::DefinitionScopeUnsupported);
        }
        node = parent;
    }
    Err(RuleGapReason::DefinitionLimit)
}

fn unary(operator: &str) -> Option<UnaryOperator> {
    Some(match operator {
        "!" => UnaryOperator::Not,
        "+" => UnaryOperator::Plus,
        "-" => UnaryOperator::Minus,
        "~" => UnaryOperator::BitwiseNot,
        "typeof" => UnaryOperator::Typeof,
        "void" => UnaryOperator::Void,
        _ => return None,
    })
}

fn logical(operator: &str) -> Option<LogicalOperator> {
    Some(match operator {
        "&&" => LogicalOperator::And,
        "||" => LogicalOperator::Or,
        "??" => LogicalOperator::Nullish,
        _ => return None,
    })
}

fn binary(operator: &str) -> Option<BinaryOperator> {
    Some(match operator {
        "==" => BinaryOperator::LooseEqual,
        "!=" => BinaryOperator::LooseNotEqual,
        "===" => BinaryOperator::StrictEqual,
        "!==" => BinaryOperator::StrictNotEqual,
        "<" => BinaryOperator::LessThan,
        "<=" => BinaryOperator::LessThanOrEqual,
        ">" => BinaryOperator::GreaterThan,
        ">=" => BinaryOperator::GreaterThanOrEqual,
        "+" => BinaryOperator::Add,
        "-" => BinaryOperator::Subtract,
        "*" => BinaryOperator::Multiply,
        "/" => BinaryOperator::Divide,
        "%" => BinaryOperator::Remainder,
        "**" => BinaryOperator::Exponent,
        "<<" => BinaryOperator::LeftShift,
        ">>" => BinaryOperator::RightShift,
        ">>>" => BinaryOperator::UnsignedRightShift,
        "&" => BinaryOperator::BitwiseAnd,
        "|" => BinaryOperator::BitwiseOr,
        "^" => BinaryOperator::BitwiseXor,
        "in" => BinaryOperator::In,
        "instanceof" => BinaryOperator::Instanceof,
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
