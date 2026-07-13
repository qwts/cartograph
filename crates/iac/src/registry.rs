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
