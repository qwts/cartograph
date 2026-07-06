//! Event SDK signature registry (SPEC-00 §3.4, US-0004): versioned
//! deterministic knowledge mapping producer/consumer SDK call shapes to
//! channel semantics. Like the HTTP registry, this is data, not inference —
//! a language adapter consults it while walking the AST and emits
//! [`EventSite`] facts; the `events` crate resolves identities and stitches
//! the channel graph.

/// Whether a matched call site produces to or consumes from a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelRole {
    /// Sends/publishes/emits onto the channel.
    Produces,
    /// Subscribes to / handles messages from the channel.
    Consumes,
}

/// Where the channel identity lives in the matched call's arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityArg {
    /// The call's first argument is the identity (`emit('order.placed')`).
    First,
    /// The identity is the value of this key anywhere inside the first
    /// object argument (`{ QueueUrl: … }`, `{ Entries: [{ DetailType: … }] }`).
    Key(&'static str),
}

/// How a call site is recognized as belonging to the SDK — the receiver
/// proof, mirroring the HTTP registry's factory tracking (never guessed
/// from variable names).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdkPattern {
    /// `new <Ctor>({ … })` where `<Ctor>` is imported from `module`
    /// (AWS SDK v3 command style: the command object carries the identity).
    Constructor {
        /// npm module the constructor must be imported from.
        module: &'static str,
        /// Constructor name (e.g. `SendMessageCommand`).
        ctor: &'static str,
    },
    /// `recv.<method>(…)` where `recv` is a variable bound to
    /// `new <Ctor>(…)` and `<Ctor>` is imported from `module`
    /// (AWS SDK v2 / EventEmitter style).
    Method {
        /// npm module the receiver's constructor must be imported from.
        module: &'static str,
        /// Constructor that creates the receiver (e.g. `AWS.SQS`,
        /// `EventEmitter`).
        ctor: &'static str,
        /// Method name on the receiver (e.g. `sendMessage`, `emit`).
        method: &'static str,
    },
    /// `recv.<method>(…)` where `recv` is bound to a factory *member call*
    /// on an SDK object (`kafka.producer()`), gated on the file importing
    /// `module` (kafkajs style).
    FactoryMethod {
        /// npm module whose import gates the match.
        module: &'static str,
        /// Factory member name that creates the receiver (`producer`).
        factory: &'static str,
        /// Method name on the receiver (`send`).
        method: &'static str,
    },
}

/// One registry entry: an SDK call shape that produces or consumes.
pub struct EventSdk {
    /// Channel kind this entry establishes (`sqs-queue`, `kafka-topic`, …).
    pub kind: &'static str,
    /// Producer or consumer.
    pub role: ChannelRole,
    /// How the call site is recognized (receiver proof included).
    pub pattern: SdkPattern,
    /// Where the channel identity lives.
    pub identity: IdentityArg,
}

/// Registry version — bump when entries change (versioned per SPEC-00 §3.2).
pub const EVENT_SDK_VERSION: &str = "ts-2026.07";

/// Event SDK entries (M3 initial set: AWS v3 + v2, kafkajs, node events).
pub const EVENT_SDKS: &[EventSdk] = &[
    // --- AWS SDK v3: identity rides the command constructor ---------------
    EventSdk {
        kind: "sqs-queue",
        role: ChannelRole::Produces,
        pattern: SdkPattern::Constructor {
            module: "@aws-sdk/client-sqs",
            ctor: "SendMessageCommand",
        },
        identity: IdentityArg::Key("QueueUrl"),
    },
    EventSdk {
        kind: "sns-topic",
        role: ChannelRole::Produces,
        pattern: SdkPattern::Constructor {
            module: "@aws-sdk/client-sns",
            ctor: "PublishCommand",
        },
        identity: IdentityArg::Key("TopicArn"),
    },
    EventSdk {
        kind: "eventbridge-detail-type",
        role: ChannelRole::Produces,
        pattern: SdkPattern::Constructor {
            module: "@aws-sdk/client-eventbridge",
            ctor: "PutEventsCommand",
        },
        identity: IdentityArg::Key("DetailType"),
    },
    // --- AWS SDK v2: method on a receiver proven from `new AWS.SQS()` ------
    EventSdk {
        kind: "sqs-queue",
        role: ChannelRole::Produces,
        pattern: SdkPattern::Method {
            module: "aws-sdk",
            ctor: "AWS.SQS",
            method: "sendMessage",
        },
        identity: IdentityArg::Key("QueueUrl"),
    },
    EventSdk {
        kind: "sns-topic",
        role: ChannelRole::Produces,
        pattern: SdkPattern::Method {
            module: "aws-sdk",
            ctor: "AWS.SNS",
            method: "publish",
        },
        identity: IdentityArg::Key("TopicArn"),
    },
    // --- sqs-consumer: the standard TS-side SQS consume shape --------------
    EventSdk {
        kind: "sqs-queue",
        role: ChannelRole::Consumes,
        pattern: SdkPattern::Constructor {
            module: "sqs-consumer",
            ctor: "Consumer",
        },
        identity: IdentityArg::Key("queueUrl"),
    },
    // --- kafkajs ------------------------------------------------------------
    EventSdk {
        kind: "kafka-topic",
        role: ChannelRole::Produces,
        pattern: SdkPattern::FactoryMethod {
            module: "kafkajs",
            factory: "producer",
            method: "send",
        },
        identity: IdentityArg::Key("topic"),
    },
    EventSdk {
        kind: "kafka-topic",
        role: ChannelRole::Consumes,
        pattern: SdkPattern::FactoryMethod {
            module: "kafkajs",
            factory: "consumer",
            method: "subscribe",
        },
        identity: IdentityArg::Key("topic"),
    },
    // --- in-proc bus (node:events) ------------------------------------------
    EventSdk {
        kind: "inproc-event",
        role: ChannelRole::Produces,
        pattern: SdkPattern::Method {
            module: "events",
            ctor: "EventEmitter",
            method: "emit",
        },
        identity: IdentityArg::First,
    },
    EventSdk {
        kind: "inproc-event",
        role: ChannelRole::Consumes,
        pattern: SdkPattern::Method {
            module: "events",
            ctor: "EventEmitter",
            method: "on",
        },
        identity: IdentityArg::First,
    },
];

/// How a site's channel-identity expression was classified at T0
/// (US-0004: literal, config-resolvable, or runtime-computed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityExpr {
    /// A string literal (or a local `const` bound to one) — AC-0010.
    Literal(String),
    /// `process.env.<KEY>` — resolvable from a present env file (AC-0011).
    EnvRef(String),
    /// Anything else: runtime-computed; escalates, Gap at T0 (AC-0012).
    /// Carries the raw source text as the Gap reason's evidence.
    Computed(String),
}

/// A producer/consumer call site emitted by a language adapter — the
/// language-independent fact the `events` crate stitches into the graph.
#[derive(Debug, Clone)]
pub struct EventSite {
    /// Channel kind from the registry entry (`sqs-queue`, …).
    pub kind: String,
    /// Producer or consumer.
    pub role: ChannelRole,
    /// The channel-identity expression, classified.
    pub identity: IdentityExpr,
    /// Symbol id of the enclosing function, if any (edge endpoint).
    pub symbol: Option<String>,
    /// File the site was found in (repo-relative).
    pub path: String,
    /// Byte span of the matched call.
    pub byte_start: u64,
    /// End of the matched call's byte span.
    pub byte_end: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_covers_the_spec_sdk_list() {
        // SPEC-00 §3.4 names the M3 shapes: sqs.sendMessage, sns.publish,
        // eventBridge.putEvents, Kafka producer.send, in-proc emit/on.
        let kinds: Vec<&str> = EVENT_SDKS.iter().map(|e| e.kind).collect();
        for expected in [
            "sqs-queue",
            "sns-topic",
            "eventbridge-detail-type",
            "kafka-topic",
            "inproc-event",
        ] {
            assert!(kinds.contains(&expected), "missing kind {expected}");
        }
        // Both roles are represented — a producer-only registry cannot stitch.
        assert!(EVENT_SDKS.iter().any(|e| e.role == ChannelRole::Produces));
        assert!(EVENT_SDKS.iter().any(|e| e.role == ChannelRole::Consumes));
    }
}
