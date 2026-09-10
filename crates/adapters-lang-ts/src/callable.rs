//! Shared lexical identities and conservative bindings for callable evidence.

use super::{FileCx, TsNode, sym_id};
use std::collections::HashMap;

pub(super) fn is_callable(node: TsNode<'_>) -> bool {
    matches!(
        node.kind(),
        "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "generator_function"
            | "arrow_function"
            | "method_definition"
    )
}

pub(super) fn walk(root: TsNode<'_>) -> Vec<TsNode<'_>> {
    let mut result = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        result.push(node);
        let mut cursor = node.walk();
        stack.extend(
            node.named_children(&mut cursor)
                .collect::<Vec<_>>()
                .into_iter()
                .rev(),
        );
    }
    result
}

fn is_scope(node: TsNode<'_>) -> bool {
    is_callable(node)
        || matches!(
            node.kind(),
            "program"
                | "statement_block"
                | "switch_body"
                | "for_statement"
                | "for_in_statement"
                | "catch_clause"
                | "class_body"
        )
}

fn nearest_scope(mut node: TsNode<'_>, function_scoped: bool) -> TsNode<'_> {
    while let Some(parent) = node.parent() {
        if is_callable(parent) && !executes_child(parent, node) {
            node = parent;
            continue;
        }
        if if function_scoped {
            is_callable(parent) || parent.kind() == "program"
        } else {
            is_scope(parent)
        } {
            return parent;
        }
        node = parent;
    }
    node
}

/// A method's computed key and decorators execute while it is created; they
/// are not execution of its body. Parameter defaults do execute in the callee.
fn executes_child(function: TsNode<'_>, child: TsNode<'_>) -> bool {
    ["body", "parameters", "parameter"]
        .into_iter()
        .any(|field| function.child_by_field_name(field) == Some(child))
}

pub(super) fn member_kind(method: TsNode<'_>) -> &'static str {
    let mut cursor = method.walk();
    let tokens: Vec<_> = method
        .children(&mut cursor)
        .map(|child| child.kind())
        .collect();
    if tokens.contains(&"static") {
        "static"
    } else if tokens.contains(&"get") {
        "getter"
    } else if tokens.contains(&"set") {
        "setter"
    } else {
        "instance"
    }
}

fn same_name_members(cx: &FileCx<'_>, method: TsNode<'_>) -> usize {
    let Some(name) = method.child_by_field_name("name") else {
        return 0;
    };
    let Some(body) = method.parent() else {
        return 0;
    };
    let mut cursor = body.walk();
    body.named_children(&mut cursor)
        .filter(|other| {
            other.kind() == "method_definition"
                && other
                    .child_by_field_name("name")
                    .is_some_and(|other| cx.text(&other) == cx.text(&name))
        })
        .count()
}

pub(super) fn instance_dispatch_candidate(cx: &FileCx<'_>, method: TsNode<'_>) -> bool {
    direct_class(method).is_some()
        && member_kind(method) == "instance"
        && same_name_members(cx, method) == 1
        && method
            .child_by_field_name("name")
            .is_some_and(|key| key.kind() == "property_identifier")
}

pub(super) fn class_name(cx: &FileCx<'_>, class: TsNode<'_>) -> String {
    let name = class
        .child_by_field_name("name")
        .map(|name| cx.text(&name).to_owned())
        .unwrap_or_else(|| "class".into());
    let scope = nearest_scope(class, false);
    if scope.kind() == "program" && class.kind() == "class_declaration" {
        name
    } else if scope.kind() == "program" {
        // Class-expression names are local to the expression. Two anonymous
        // classes, or two expressions using the same private name, are never
        // the same declaration even at module scope.
        format!("{name}@{}", class.start_byte())
    } else {
        format!("{}/{}@{}", scope_name(cx, scope), name, class.start_byte())
    }
}

fn scope_name(cx: &FileCx<'_>, scope: TsNode<'_>) -> String {
    if is_callable(scope) {
        return name(cx, scope);
    }
    if let Some(parent) = scope.parent() {
        if is_callable(parent) {
            return name(cx, parent);
        }
        if matches!(parent.kind(), "class_declaration" | "class") {
            return class_name(cx, parent);
        }
    }
    format!("scope@{}", scope.start_byte())
}

pub(super) fn direct_class(method: TsNode<'_>) -> Option<TsNode<'_>> {
    method
        .parent()
        .filter(|parent| parent.kind() == "class_body")?
        .parent()
        .filter(|parent| matches!(parent.kind(), "class_declaration" | "class"))
}

fn object_name(cx: &FileCx<'_>, object: TsNode<'_>) -> String {
    let parent = object.parent();
    let hint = parent
        .and_then(|parent| match parent.kind() {
            "variable_declarator" | "public_field_definition" => parent.child_by_field_name("name"),
            "pair" => parent.child_by_field_name("key"),
            _ => None,
        })
        .filter(|key| matches!(key.kind(), "identifier" | "property_identifier"))
        .map(|key| cx.text(&key).to_owned())
        .unwrap_or_else(|| "object".into());
    let scope = nearest_scope(object, false);
    let local = format!("{hint}@{}", object.start_byte());
    if scope.kind() == "program" {
        local
    } else {
        format!("{}/{local}", scope_name(cx, scope))
    }
}

pub(super) fn binding_name(cx: &FileCx<'_>, function: TsNode<'_>) -> Option<String> {
    match function.kind() {
        "function_declaration" | "generator_function_declaration" => {
            function.child_by_field_name("name")
        }
        "arrow_function" | "function_expression" | "generator_function" => function
            .parent()
            .filter(|parent| parent.kind() == "variable_declarator")
            .and_then(|parent| parent.child_by_field_name("name"))
            .filter(|name| name.kind() == "identifier"),
        _ => None,
    }
    .map(|name| cx.text(&name).to_owned())
}

pub(super) fn name(cx: &FileCx<'_>, function: TsNode<'_>) -> String {
    if function.kind() == "method_definition" {
        let key = function.child_by_field_name("name");
        let name = key
            .filter(|key| key.kind() != "computed_property_name")
            .map(|key| cx.text(&key).to_owned())
            .unwrap_or_else(|| "computed".into());
        if let Some(class) = direct_class(function) {
            return if key.is_some_and(|key| key.kind() == "computed_property_name") {
                format!(
                    "{}.computed@{}",
                    class_name(cx, class),
                    function.start_byte()
                )
            } else if same_name_members(cx, function) > 1 {
                format!(
                    "{}.{name}@{}:{}",
                    class_name(cx, class),
                    member_kind(function),
                    function.start_byte()
                )
            } else {
                format!("{}.{name}", class_name(cx, class))
            };
        }
        if let Some(object) = function.parent().filter(|parent| parent.kind() == "object") {
            return format!(
                "{}.{name}@{}",
                object_name(cx, object),
                function.start_byte()
            );
        }
    }
    if let Some(pair) = function.parent().filter(|parent| parent.kind() == "pair")
        && let Some(object) = pair.parent().filter(|parent| parent.kind() == "object")
    {
        let key = pair.child_by_field_name("key");
        let hint = key
            .filter(|key| key.kind() != "computed_property_name")
            .map(|key| cx.text(&key).to_owned())
            .unwrap_or_else(|| "computed".into());
        return format!(
            "{}.{hint}@{}",
            object_name(cx, object),
            function.start_byte()
        );
    }
    if let Some(name) = binding_name(cx, function) {
        let scope = nearest_scope(function, false);
        return if scope.kind() == "program" {
            name
        } else {
            format!("{}/{name}@{}", scope_name(cx, scope), function.start_byte())
        };
    }
    // Retain route-handler identities and also emit every other anonymous owner.
    format!("anon@{}", function.start_byte())
}

pub(super) fn id(cx: &FileCx<'_>, function: TsNode<'_>) -> String {
    sym_id(cx.id.repo, cx.path, &name(cx, function))
}

pub(super) fn enclosing(cx: &FileCx<'_>, mut node: TsNode<'_>) -> Option<String> {
    let mut decorator = false;
    while let Some(parent) = node.parent() {
        decorator |= node.kind() == "decorator";
        if is_callable(parent) && executes_child(parent, node) && !decorator {
            return Some(id(cx, parent));
        }
        // Initializers/static blocks are separate runtime work. Until they
        // have modeled owners they cannot borrow an enclosing method's calls.
        if parent.kind() == "class_static_block"
            || (parent.kind() == "public_field_definition"
                && parent.child_by_field_name("value") == Some(node))
        {
            return None;
        }
        if is_callable(parent) || matches!(parent.kind(), "class_declaration" | "class") {
            decorator = false;
        }
        node = parent;
    }
    None
}

/// `this` follows arrows, but never crosses an ordinary callback/object method.
pub(super) fn this_class(cx: &FileCx<'_>, mut node: TsNode<'_>) -> Option<String> {
    while let Some(parent) = node.parent() {
        // Parameter decorators sit beneath the method's parameter node but
        // execute at class creation. They cannot borrow the callee's instance
        // receiver; ambient decorator `this` is not modeled by this resolver.
        if parent.kind() == "decorator" {
            return None;
        }
        if parent.kind() == "method_definition" && executes_child(parent, node) {
            if member_kind(parent) == "static" {
                return None;
            }
            return direct_class(parent).map(|class| class_name(cx, class));
        }
        if matches!(
            parent.kind(),
            "class_body" | "class_static_block" | "public_field_definition"
        ) {
            return None;
        }
        if is_callable(parent) && parent.kind() != "arrow_function" && executes_child(parent, node)
        {
            return None;
        }
        node = parent;
    }
    None
}

#[derive(Clone)]
enum Target {
    Callable(String),
    Imported,
    Unknown,
}

#[derive(Clone)]
struct Binding<'tree> {
    declaration: TsNode<'tree>,
    target: Target,
    invalidated: bool,
}

pub(super) struct Bindings<'tree> {
    entries: HashMap<(usize, String), Binding<'tree>>,
}

fn pattern_names(cx: &FileCx<'_>, pattern: TsNode<'_>) -> Vec<String> {
    match pattern.kind() {
        "identifier" | "type_identifier" | "shorthand_property_identifier_pattern" => {
            vec![cx.text(&pattern).into()]
        }
        "pair_pattern" => pattern
            .child_by_field_name("value")
            .map(|value| pattern_names(cx, value))
            .unwrap_or_default(),
        "assignment_pattern" | "object_assignment_pattern" => pattern
            .child_by_field_name("left")
            .map(|left| pattern_names(cx, left))
            .unwrap_or_default(),
        "rest_pattern" | "object_pattern" | "array_pattern" => {
            let mut cursor = pattern.walk();
            pattern
                .named_children(&mut cursor)
                .flat_map(|child| pattern_names(cx, child))
                .collect()
        }
        _ => vec![],
    }
}

impl<'tree> Bindings<'tree> {
    pub(super) fn new(cx: &FileCx<'_>, root: TsNode<'tree>, nodes: &[TsNode<'tree>]) -> Self {
        let mut index = Self {
            entries: HashMap::new(),
        };
        for node in nodes.iter().copied() {
            let mut add = |scope: TsNode<'tree>, name: String, target: Target| {
                let key = (scope.id(), name);
                // Duplicate declarations are ambiguous, regardless of walk order.
                index
                    .entries
                    .entry(key)
                    .and_modify(|existing| {
                        existing.target = Target::Unknown;
                        existing.invalidated = true;
                    })
                    .or_insert(Binding {
                        declaration: node,
                        target,
                        invalidated: false,
                    });
            };
            match node.kind() {
                "function_declaration" | "generator_function_declaration" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        add(
                            nearest_scope(node, false),
                            cx.text(&name).into(),
                            Target::Callable(id(cx, node)),
                        );
                    }
                }
                "function_expression" | "generator_function" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        add(node, cx.text(&name).into(), Target::Callable(id(cx, node)));
                    }
                }
                "variable_declarator" => {
                    if let Some(pattern) = node.child_by_field_name("name") {
                        let is_var = node
                            .parent()
                            .is_some_and(|parent| parent.kind() == "variable_declaration");
                        let is_const = node
                            .parent()
                            .and_then(|parent| parent.child(0))
                            .is_some_and(|keyword| keyword.kind() == "const");
                        let callable = node
                            .child_by_field_name("value")
                            .filter(|value| is_callable(*value));
                        for name in pattern_names(cx, pattern) {
                            let target = if is_const && pattern.kind() == "identifier" {
                                callable
                                    .map(|function| Target::Callable(id(cx, function)))
                                    .unwrap_or(Target::Unknown)
                            } else {
                                Target::Unknown
                            };
                            add(nearest_scope(node, is_var), name, target);
                        }
                    }
                }
                "required_parameter" | "optional_parameter" => {
                    if let Some(pattern) = node.child_by_field_name("pattern") {
                        for name in pattern_names(cx, pattern) {
                            add(nearest_scope(node, true), name, Target::Unknown);
                        }
                    }
                }
                "arrow_function" => {
                    if let Some(parameter) = node.child_by_field_name("parameter") {
                        for name in pattern_names(cx, parameter) {
                            add(node, name, Target::Unknown);
                        }
                    }
                }
                "catch_clause" => {
                    if let Some(parameter) = node.child_by_field_name("parameter") {
                        for name in pattern_names(cx, parameter) {
                            add(node, name, Target::Unknown);
                        }
                    }
                }
                "for_in_statement" => {
                    if let Some(kind) = node.child_by_field_name("kind")
                        && let Some(pattern) = node.child_by_field_name("left")
                    {
                        let scope = if kind.kind() == "var" {
                            nearest_scope(node, true)
                        } else {
                            node
                        };
                        for name in pattern_names(cx, pattern) {
                            add(scope, name, Target::Unknown);
                        }
                    }
                }
                "class_declaration" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        add(
                            nearest_scope(node, false),
                            cx.text(&name).into(),
                            Target::Unknown,
                        );
                    }
                }
                "class" => {
                    // A named class expression's self name exists only inside
                    // the expression. It must shadow outer callable/imported
                    // names even when class-expression dispatch is unsupported.
                    if let Some(name) = node.child_by_field_name("name") {
                        add(node, cx.text(&name).into(), Target::Unknown);
                    }
                }
                "import_specifier" => {
                    if let Some(name) = node
                        .child_by_field_name("alias")
                        .or_else(|| node.child_by_field_name("name"))
                    {
                        add(root, cx.text(&name).into(), Target::Imported);
                    }
                }
                "import_clause" | "namespace_import" => {
                    let mut cursor = node.walk();
                    for name in node
                        .named_children(&mut cursor)
                        .filter(|child| child.kind() == "identifier")
                    {
                        add(root, cx.text(&name).into(), Target::Imported);
                    }
                }
                _ => {}
            }
        }
        // Any observed reassignment invalidates the direct target, including
        // assignments in nested closures. No execution-order guess is required.
        for node in nodes.iter().copied() {
            let pattern = match node.kind() {
                "assignment_expression" | "augmented_assignment_expression" => {
                    node.child_by_field_name("left")
                }
                "for_in_statement" if node.child_by_field_name("kind").is_none() => {
                    node.child_by_field_name("left")
                }
                "update_expression" => node.child_by_field_name("argument"),
                _ => None,
            };
            if let Some(pattern) = pattern {
                for name in pattern_names(cx, pattern) {
                    if let Some(key) = index.key(node, &name)
                        && let Some(binding) = index.entries.get_mut(&key)
                    {
                        binding.target = Target::Unknown;
                        binding.invalidated = true;
                    }
                }
            }
        }
        index
    }

    fn key(&self, mut site: TsNode<'_>, name: &str) -> Option<(usize, String)> {
        let mut child = None;
        let mut decorator = false;
        loop {
            if site.kind() == "with_statement" {
                return None;
            }
            let key = (site.id(), name.to_owned());
            let participates = !is_callable(site)
                || (child.is_none_or(|child| executes_child(site, child)) && !decorator);
            if participates && self.entries.contains_key(&key) {
                return Some(key);
            }
            if is_callable(site) || matches!(site.kind(), "class_declaration" | "class") {
                decorator = false;
            } else {
                decorator |= site.kind() == "decorator";
            }
            child = Some(site);
            site = site.parent()?;
        }
    }

    pub(super) fn local_target(&self, site: TsNode<'_>, name: &str) -> Option<String> {
        let binding = self.entries.get(&self.key(site, name)?)?;
        match &binding.target {
            Target::Callable(id) => Some(id.clone()),
            _ => None,
        }
    }

    pub(super) fn imported(&self, site: TsNode<'_>, name: &str) -> bool {
        self.key(site, name)
            .and_then(|key| self.entries.get(&key))
            .is_some_and(|binding| matches!(binding.target, Target::Imported))
    }

    pub(super) fn stable_declaration(&self, site: TsNode<'_>, name: &str) -> Option<TsNode<'tree>> {
        let binding = self.entries.get(&self.key(site, name)?)?;
        (!binding.invalidated).then_some(binding.declaration)
    }

    pub(super) fn global_unshadowed(&self, mut site: TsNode<'_>, name: &str) -> bool {
        if self.key(site, name).is_some() {
            return false;
        }
        loop {
            if site.kind() == "with_statement" {
                return false;
            }
            let Some(parent) = site.parent() else {
                return true;
            };
            site = parent;
        }
    }
}
