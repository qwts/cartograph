//! AWS Capability Registry (SPEC-00 §3.2): versioned deterministic knowledge
//! mapping resource types to runtime semantics. This is data, not inference —
//! every entry says "when this resource type appears, the reference in
//! `source_attr` acts on the reference in `target_attr` with this verb".
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

/// One registry entry: a mediating resource type whose attributes reference
/// the source and target of a semantic edge.
pub struct Capability {
    /// Terraform resource type that mediates the relationship.
    pub resource_type: &'static str,
    /// Verb contributed to the graph.
    pub kind: CapabilityKind,
    /// Attribute whose reference is the edge source.
    pub source_attr: &'static str,
    /// Attribute whose reference is the edge target.
    pub target_attr: &'static str,
}

/// Registry version — bump when entries change (registry knowledge is
/// versioned per SPEC-00 §3.2).
pub const REGISTRY_VERSION: &str = "aws-2026.07";

/// AWS capability entries (M2 initial set).
pub const AWS_CAPABILITIES: &[Capability] = &[
    Capability {
        resource_type: "aws_lambda_event_source_mapping",
        kind: CapabilityKind::Triggers,
        source_attr: "event_source_arn",
        target_attr: "function_name",
    },
    Capability {
        resource_type: "aws_cloudwatch_event_target",
        kind: CapabilityKind::Triggers,
        source_attr: "rule",
        target_attr: "arn",
    },
    Capability {
        resource_type: "aws_sns_topic_subscription",
        kind: CapabilityKind::Subscribes,
        source_attr: "endpoint",
        target_attr: "topic_arn",
    },
    Capability {
        resource_type: "aws_lb_listener",
        kind: CapabilityKind::Routes,
        source_attr: "load_balancer_arn",
        target_attr: "default_action", // target_group_arn ref lives in the nested block
    },
    Capability {
        resource_type: "aws_lb_target_group_attachment",
        kind: CapabilityKind::Routes,
        source_attr: "target_group_arn",
        target_attr: "target_id",
    },
];

/// Entries applying to `resource_type`, if any.
pub fn capabilities_for(resource_type: &str) -> impl Iterator<Item = &'static Capability> {
    AWS_CAPABILITIES
        .iter()
        .filter(move |c| c.resource_type == resource_type)
}
