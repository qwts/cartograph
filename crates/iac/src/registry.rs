//! AWS Capability Registry (SPEC-00 §3.2): versioned deterministic knowledge
//! mapping resource types to runtime semantics. This is data, not inference —
//! every entry says "when this resource type appears, the references selected
//! by `source` act on the references selected by `target` with this verb".
//! Priority per spec: AWS first; Azure/GCP entries join later.

/// Semantic verb an entry contributes to the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityKind {
    /// Event source invokes a compute target (e.g. SQS → Lambda).
    Triggers,
    /// Traffic routing (e.g. listener → target group → target).
    Routes,
    /// Subscription of an endpoint to a topic/bus.
    Subscribes,
}

impl CapabilityKind {
    /// Graph edge label for this capability.
    pub fn edge_label(self) -> &'static str {
        match self {
            CapabilityKind::Triggers => "TRIGGERS",
            CapabilityKind::Routes => "ROUTES",
            CapabilityKind::Subscribes => "SUBSCRIBES",
        }
    }
}

/// One endpoint of a deterministic capability edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointSelector {
    /// The mediating resource itself (for example a CloudFront distribution).
    Resource,
    /// References selected by a path through attributes/nested blocks.
    ///
    /// A segment prefixed with `*` matches a nested block by suffix, so
    /// `*cache_behavior` covers both `default_cache_behavior` and
    /// `ordered_cache_behavior` without matching unrelated blocks.
    Path(&'static [&'static str]),
}

/// One registry entry: a mediating resource type whose selectors establish
/// the source and target of a semantic edge.
pub struct Capability {
    /// Terraform resource type that mediates the relationship.
    pub resource_type: &'static str,
    /// Verb contributed to the graph.
    pub kind: CapabilityKind,
    /// Selector producing the edge source(s).
    pub source: EndpointSelector,
    /// Selector producing the edge target(s).
    pub target: EndpointSelector,
}

/// Registry version — bump when entries change (registry knowledge is
/// versioned per SPEC-00 §3.2).
pub const REGISTRY_VERSION: &str = "aws-2026.07.1";

/// AWS capability entries (M2 versioned set).
pub const AWS_CAPABILITIES: &[Capability] = &[
    Capability {
        resource_type: "aws_lambda_event_source_mapping",
        kind: CapabilityKind::Triggers,
        source: EndpointSelector::Path(&["event_source_arn"]),
        target: EndpointSelector::Path(&["function_name"]),
    },
    Capability {
        resource_type: "aws_cloudwatch_event_target",
        kind: CapabilityKind::Triggers,
        source: EndpointSelector::Path(&["rule"]),
        target: EndpointSelector::Path(&["arn"]),
    },
    Capability {
        resource_type: "aws_sns_topic_subscription",
        kind: CapabilityKind::Subscribes,
        source: EndpointSelector::Path(&["endpoint"]),
        target: EndpointSelector::Path(&["topic_arn"]),
    },
    Capability {
        resource_type: "aws_lb_listener",
        kind: CapabilityKind::Routes,
        source: EndpointSelector::Path(&["load_balancer_arn"]),
        // target_group_arn ref lives in the nested block.
        target: EndpointSelector::Path(&["default_action"]),
    },
    Capability {
        resource_type: "aws_lb_target_group_attachment",
        kind: CapabilityKind::Routes,
        source: EndpointSelector::Path(&["target_group_arn"]),
        target: EndpointSelector::Path(&["target_id"]),
    },
    Capability {
        resource_type: "aws_api_gateway_integration",
        kind: CapabilityKind::Routes,
        source: EndpointSelector::Path(&["rest_api_id"]),
        target: EndpointSelector::Path(&["uri"]),
    },
    Capability {
        resource_type: "aws_lambda_permission",
        kind: CapabilityKind::Triggers,
        source: EndpointSelector::Path(&["source_arn"]),
        target: EndpointSelector::Path(&["function_name"]),
    },
    Capability {
        resource_type: "aws_cloudfront_distribution",
        kind: CapabilityKind::Triggers,
        source: EndpointSelector::Resource,
        target: EndpointSelector::Path(&[
            "*cache_behavior",
            "lambda_function_association",
            "lambda_arn",
        ]),
    },
    Capability {
        resource_type: "aws_pipes_pipe",
        kind: CapabilityKind::Triggers,
        source: EndpointSelector::Path(&["source"]),
        target: EndpointSelector::Path(&["target"]),
    },
];

/// Entries applying to `resource_type`, if any.
pub fn capabilities_for(resource_type: &str) -> impl Iterator<Item = &'static Capability> {
    AWS_CAPABILITIES
        .iter()
        .filter(move |c| c.resource_type == resource_type)
}

fn snake_case(name: &str) -> String {
    let chars = name.chars().collect::<Vec<_>>();
    let mut out = String::new();
    for (index, ch) in chars.iter().copied().enumerate() {
        if ch.is_ascii_uppercase() {
            let previous = index.checked_sub(1).and_then(|i| chars.get(i)).copied();
            let next = chars.get(index + 1).copied();
            if index > 0
                && (previous.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                    || next.is_some_and(|c| c.is_ascii_lowercase()))
            {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn pulumi_parts(module_spec: &str, constructor: &str) -> Option<(String, Vec<String>, String)> {
    let package = module_spec.strip_prefix("@pulumi/")?;
    let mut package_parts = package.split('/');
    let provider = package_parts.next()?;
    if provider.is_empty() {
        return None;
    }
    let direct_modules = package_parts.map(str::to_string).collect::<Vec<_>>();
    let mut constructor_parts = constructor.split('.');
    let local_binding = constructor_parts.next()?;
    let mut tail = constructor_parts.map(str::to_string).collect::<Vec<_>>();
    let class = tail.pop().or_else(|| {
        // A direct service package may be imported as the constructor itself:
        // `import { Bucket } from "@pulumi/aws/s3"; new Bucket(...)`.
        (!direct_modules.is_empty()).then(|| local_binding.to_string())
    })?;
    let modules = if direct_modules.is_empty() {
        tail
    } else {
        direct_modules.into_iter().chain(tail).collect::<Vec<_>>()
    };
    if modules.is_empty() || class.is_empty() {
        return None;
    }
    Some((provider.to_string(), modules, class))
}

/// Canonical Pulumi provider token for an import-proven constructor.
pub fn pulumi_token_for_constructor(module_spec: &str, constructor: &str) -> Option<String> {
    let (provider, modules, class) = pulumi_parts(module_spec, constructor)?;
    let resource_module = format!(
        "{}/{}",
        modules.join("/"),
        class
            .chars()
            .next()
            .map(|first| format!(
                "{}{}",
                first.to_ascii_lowercase(),
                &class[first.len_utf8()..]
            ))
            .unwrap_or_default()
    );
    Some(format!("{provider}:{resource_module}:{class}"))
}

/// Normalize an import-proven Pulumi provider constructor to the Terraform
/// resource-type spelling used by the shared Capability Registry.
///
/// `@pulumi/aws` + `aws.lambda.EventSourceMapping` becomes
/// `aws_lambda_event_source_mapping`; direct service imports such as
/// `@pulumi/aws/s3` + `s3.Bucket` are supported as well. The small API Gateway
/// alias is an explicit deterministic provider naming difference, not an
/// inferred match.
pub fn terraform_type_for_pulumi(module_spec: &str, constructor: &str) -> Option<String> {
    let (provider, modules, class) = pulumi_parts(module_spec, constructor)?;
    let service = modules
        .into_iter()
        .map(|module| snake_case(&module))
        .collect::<Vec<_>>()
        .join("_");
    let mut resource_type = format!("{provider}_{service}_{}", snake_case(&class));
    if let Some(rest) = resource_type.strip_prefix("aws_apigateway_") {
        resource_type = format!("aws_api_gateway_{rest}");
    }
    Some(resource_type)
}
