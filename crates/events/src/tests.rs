//! Channel-identity resolution tests (US-0004). Fixtures run the real
//! chain: TS source → adapter event-site detection → stitch.

use super::*;

fn extract_sites(dir: &Path) -> Vec<EventSite> {
    let id = adapters_lang_ts::SourceId {
        repo: "test",
        commit: "deadbeef",
    };
    adapters_lang_ts::extract_dir(dir, &id)
        .expect("extract")
        .event_sites
}

fn stitch_dir(dir: &Path) -> Extraction {
    let sites = extract_sites(dir);
    let cfg = ConfigIndex::from_dir(dir).expect("config scan");
    let id = SourceId {
        repo: "test",
        commit: "deadbeef",
    };
    stitch(&sites, &cfg, &id)
}

fn confidence(props: &serde_json::Value) -> &str {
    props["prov"]["confidence_tier"]
        .as_str()
        .expect("prov on every fact")
}

// AC-0010: literal channel ids on both sides match into one Channel with
// Confirmed PUBLISHES/SUBSCRIBES edges. (T-0010)
#[test]
fn literal_channel_ids_link_producer_and_consumer() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("bus.ts"),
        r#"
import { EventEmitter } from 'node:events';
const bus = new EventEmitter();
export function placeOrder() {
  bus.emit('order.placed', { id: 1 });
}
export function onOrderPlaced() {
  bus.on('order.placed', handle);
}
function handle() {}
"#,
    )
    .unwrap();

    let out = stitch_dir(dir.path());
    let channels: Vec<_> = out.nodes.iter().filter(|n| n.label == "Channel").collect();
    assert_eq!(channels.len(), 1, "both sides resolve to the same channel");
    let chan = channels[0];
    assert_eq!(chan.id, "chan:inproc-event:order.placed");
    assert_eq!(chan.props["identity"], "order.placed");
    assert_eq!(confidence(&chan.props), "Confirmed");

    let publish = out
        .edges
        .iter()
        .find(|e| e.label == "PUBLISHES")
        .expect("publish edge");
    let subscribe = out
        .edges
        .iter()
        .find(|e| e.label == "SUBSCRIBES")
        .expect("subscribe edge");
    assert_eq!(publish.src, "sym:bus.ts#placeOrder");
    assert_eq!(publish.dst, chan.id);
    assert_eq!(subscribe.src, "sym:bus.ts#onOrderPlaced");
    assert_eq!(subscribe.dst, chan.id);
    assert_eq!(confidence(&publish.props), "Confirmed");
    assert_eq!(publish.props["resolver"], "literal");
    // No Gap emitted for resolved sites.
    assert!(out.nodes.iter().all(|n| n.label != "Gap"));
}

// AC-0011: a channel id from an env file present in the repo resolves via
// the config resolver, still Confirmed. (T-0011)
#[test]
fn config_resolved_channel_is_confirmed_via_env_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".env"),
        "ORDERS_QUEUE=https://sqs.example/orders\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("send.ts"),
        r#"
import { SQSClient, SendMessageCommand } from '@aws-sdk/client-sqs';
const client = new SQSClient({});
export function enqueue(body: string) {
  return client.send(new SendMessageCommand({
    QueueUrl: process.env.ORDERS_QUEUE,
    MessageBody: body,
  }));
}
"#,
    )
    .unwrap();

    let out = stitch_dir(dir.path());
    let chan = out
        .nodes
        .iter()
        .find(|n| n.label == "Channel")
        .expect("env ref resolved to a channel");
    assert_eq!(chan.id, "chan:sqs-queue:https://sqs.example/orders");
    assert_eq!(confidence(&chan.props), "Confirmed");
    let publish = out
        .edges
        .iter()
        .find(|e| e.label == "PUBLISHES")
        .expect("publish edge");
    assert_eq!(publish.props["resolver"], "config:.env");
    assert_eq!(publish.src, "sym:send.ts#enqueue");
}

// AC-0012: a runtime-computed channel id cannot resolve at T0; the hop
// escalates (ladder above T0 is empty until M6/M7) and emits an explicit
// Gap with a reason — never a silent completion (R-INT-4). (T-0012)
#[test]
fn computed_channel_emits_gap_with_reason() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dynamic.ts"),
        r#"
import { EventEmitter } from 'events';
const bus = new EventEmitter();
export function notify(tenant: string) {
  bus.emit(channelFor(tenant), {});
}
function channelFor(tenant: string): string { return 'tenant.' + tenant; }
"#,
    )
    .unwrap();

    let out = stitch_dir(dir.path());
    assert!(
        out.nodes.iter().all(|n| n.label != "Channel"),
        "computed identity must not fabricate a channel"
    );
    let gap = out
        .nodes
        .iter()
        .find(|n| n.label == "Gap")
        .expect("explicit Gap node");
    assert_eq!(gap.props["reason"], "runtime-computed channel identity");
    assert_eq!(gap.props["raw"], "channelFor(tenant)");
    assert_eq!(gap.props["attempted_tiers"], serde_json::json!(["T0"]));
    assert_eq!(confidence(&gap.props), "Gap");
    // The branch is truncated at the Gap, not dropped: the edge still exists.
    let publish = out
        .edges
        .iter()
        .find(|e| e.label == "PUBLISHES")
        .expect("edge to the Gap");
    assert_eq!(publish.dst, gap.id);
    assert_eq!(confidence(&publish.props), "Gap");
}

// AC-0012 (env-ref miss): an env ref with no matching key in any env file
// is unresolved — Gap with the key named in the reason, not a guess.
#[test]
fn missing_env_key_emits_gap_naming_the_key() {
    let dir = tempfile::tempdir().unwrap();
    // No .env file at all.
    std::fs::write(
        dir.path().join("send.ts"),
        r#"
import { SNSClient, PublishCommand } from '@aws-sdk/client-sns';
const sns = new SNSClient({});
export function announce() {
  return sns.send(new PublishCommand({ TopicArn: process.env.ANNOUNCE_TOPIC }));
}
"#,
    )
    .unwrap();

    let out = stitch_dir(dir.path());
    let gap = out
        .nodes
        .iter()
        .find(|n| n.label == "Gap")
        .expect("gap node");
    assert_eq!(
        gap.props["reason"],
        "env key ANNOUNCE_TOPIC not found in any env file in the repo"
    );
}

// Config resolver mechanics: parsing, precedence, determinism (US-0014).
#[test]
fn config_index_parses_env_files_deterministically() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".env"),
        "# comment\nORDERS_TOPIC=orders\nexport QUOTED='q-topic'\nBAD LINE\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join(".env.local"),
        "ORDERS_TOPIC=local-override\nEXTRA=x\n",
    )
    .unwrap();

    let cfg = ConfigIndex::from_dir(dir.path()).unwrap();
    // Sorted file order, first definition wins: `.env` < `.env.local`.
    assert_eq!(cfg.resolve("ORDERS_TOPIC"), Some(("orders", ".env")));
    assert_eq!(cfg.resolve("QUOTED"), Some(("q-topic", ".env")));
    assert_eq!(cfg.resolve("EXTRA"), Some(("x", ".env.local")));
    assert_eq!(cfg.resolve("MISSING"), None);
}

// Kafka topics stitch across files: producer and consumer in different
// modules, both literal, one channel.
#[test]
fn kafka_producer_and_consumer_stitch_across_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("producer.ts"),
        r#"
import { Kafka } from 'kafkajs';
const kafka = new Kafka({ clientId: 'a' });
const producer = kafka.producer();
export async function publishOrder() {
  await producer.send({ topic: 'orders', messages: [] });
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("consumer.ts"),
        r#"
import { Kafka } from 'kafkajs';
const kafka = new Kafka({ clientId: 'b' });
const consumer = kafka.consumer({ groupId: 'g' });
export async function listen() {
  await consumer.subscribe({ topic: 'orders' });
}
"#,
    )
    .unwrap();

    let out = stitch_dir(dir.path());
    let channels: Vec<_> = out.nodes.iter().filter(|n| n.label == "Channel").collect();
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0].id, "chan:kafka-topic:orders");
    assert!(
        out.edges
            .iter()
            .any(|e| e.label == "PUBLISHES" && e.src == "sym:producer.ts#publishOrder")
    );
    assert!(
        out.edges
            .iter()
            .any(|e| e.label == "SUBSCRIBES" && e.src == "sym:consumer.ts#listen")
    );
}

// --- FETCHES resolution (US-0005, AC-0014) ---------------------------------

use adapters_fw::client::FetchSite;

fn fetch_site(method: &str, url: IdentityExpr) -> FetchSite {
    FetchSite {
        method: method.into(),
        url,
        symbol: Some("sym:app.tsx#Orders".into()),
        path: "app.tsx".into(),
        byte_start: 10,
        byte_end: 40,
    }
}

fn eps(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

// AC-0014: a resolvable fetch URL matching one endpoint is a Confirmed
// FETCHES edge — exact and :param-template routes both match. (T-0014)
#[test]
fn resolvable_fetch_urls_confirm_against_endpoints() {
    let endpoints = eps(&["ep:GET:/api/users/:id", "ep:POST:/api/orders"]);
    let cfg = ConfigIndex::default();
    let id = SourceId {
        repo: "test",
        commit: "deadbeef",
    };
    let out = stitch_fetches(
        &[
            fetch_site("POST", IdentityExpr::Literal("/api/orders".into())),
            // Query string stripped; :id template matches a concrete segment.
            fetch_site(
                "GET",
                IdentityExpr::Literal("https://api.example.com/api/users/42?full=1".into()),
            ),
        ],
        &endpoints,
        &cfg,
        &id,
    );
    let fetches: Vec<(&str, &str)> = out
        .edges
        .iter()
        .filter(|e| e.label == "FETCHES")
        .map(|e| (e.src.as_str(), e.dst.as_str()))
        .collect();
    assert_eq!(
        fetches,
        [
            ("sym:app.tsx#Orders", "ep:POST:/api/orders"),
            ("sym:app.tsx#Orders", "ep:GET:/api/users/:id"),
        ]
    );
    for e in out.edges.iter().filter(|e| e.label == "FETCHES") {
        assert_eq!(confidence(&e.props), "Confirmed");
    }
    assert!(out.nodes.iter().all(|n| n.label != "Gap"));
}

// AC-0014: everything unresolvable escalates to an explicit Gap with a
// reason — computed URL, no matching endpoint, ambiguous match. (T-0014)
#[test]
fn unresolvable_fetches_emit_gaps_with_reasons() {
    let endpoints = eps(&["ep:GET:/api/users/:id", "ep:GET:/api/:section/42"]);
    let cfg = ConfigIndex::default();
    let id = SourceId {
        repo: "test",
        commit: "deadbeef",
    };
    let out = stitch_fetches(
        &[
            fetch_site("GET", IdentityExpr::Computed("buildUrl()".into())),
            fetch_site("DELETE", IdentityExpr::Literal("/api/nothing".into())),
            // Matches both template endpoints — ambiguous is not Confirmed.
            fetch_site("GET", IdentityExpr::Literal("/api/users/42".into())),
        ],
        &endpoints,
        &cfg,
        &id,
    );
    let reasons: Vec<&str> = out
        .nodes
        .iter()
        .filter(|n| n.label == "Gap")
        .map(|n| n.props["reason"].as_str().unwrap())
        .collect();
    assert_eq!(reasons.len(), 3);
    assert_eq!(reasons[0], "runtime-computed fetch URL");
    assert_eq!(
        reasons[1],
        "no recovered endpoint matches DELETE /api/nothing"
    );
    assert!(reasons[2].starts_with("ambiguous endpoint match for GET /api/users/42"));
    // Every gap edge is Gap-confidence and the site is never dropped.
    let gap_edges = out.edges.iter().filter(|e| e.label == "FETCHES").count();
    assert_eq!(gap_edges, 3);
}

// An env-resolved base URL confirms through the config resolver, same as
// channel identities (AC-0011 discipline applied to AC-0014).
#[test]
fn env_resolved_fetch_url_confirms() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".env"),
        "ORDERS_API=https://api.example.com/api/orders\n",
    )
    .unwrap();
    let cfg = ConfigIndex::from_dir(dir.path()).unwrap();
    let id = SourceId {
        repo: "test",
        commit: "deadbeef",
    };
    let out = stitch_fetches(
        &[fetch_site(
            "POST",
            IdentityExpr::EnvRef("ORDERS_API".into()),
        )],
        &eps(&["ep:POST:/api/orders"]),
        &cfg,
        &id,
    );
    let edge = out.edges.iter().find(|e| e.label == "FETCHES").unwrap();
    assert_eq!(edge.dst, "ep:POST:/api/orders");
    assert_eq!(edge.props["resolver"], "config:.env");
    assert_eq!(confidence(&edge.props), "Confirmed");
}
