//! Flow tracer tests (US-0006). Graphs are built inline: the tracer is a
//! pure function over node/edge slices.

use super::*;

fn node(id: &str, label: &str, props: serde_json::Value) -> Node {
    Node {
        id: id.into(),
        label: label.into(),
        props,
    }
}

fn edge(src: &str, dst: &str, label: &str, confidence: &str) -> Edge {
    Edge {
        src: src.into(),
        dst: dst.into(),
        label: label.into(),
        props: serde_json::json!({
            "prov": {
                "tier": "Deterministic",
                "confidence_tier": confidence,
                "evidence": [{
                    "repo": "test", "path": "src/app.ts",
                    "byte_start": 10, "byte_end": 42, "commit_sha": "abc",
                }],
                "extractor_id": "t0.test",
                "content_hash": "0".repeat(64),
            }
        }),
    }
}

/// Endpoint → handler → helper → publish → channel → consumer.
fn event_chain() -> (Vec<Node>, Vec<Edge>) {
    let nodes = vec![
        node(
            "ep:POST:/orders",
            "Endpoint",
            serde_json::json!({"method": "POST", "path": "/orders"}),
        ),
        node(
            "sym:app.ts#create",
            "Symbol",
            serde_json::json!({"name": "create"}),
        ),
        node(
            "sym:app.ts#persist",
            "Symbol",
            serde_json::json!({"name": "persist"}),
        ),
        node(
            "chan:inproc-event:order.placed",
            "Channel",
            serde_json::json!({"kind": "inproc-event", "identity": "order.placed"}),
        ),
        node(
            "sym:mailer.ts#onPlaced",
            "Symbol",
            serde_json::json!({"name": "onPlaced"}),
        ),
    ];
    let edges = vec![
        edge(
            "ep:POST:/orders",
            "sym:app.ts#create",
            "HANDLES",
            "Confirmed",
        ),
        edge(
            "sym:app.ts#create",
            "sym:app.ts#persist",
            "CALLS",
            "Confirmed",
        ),
        edge(
            "sym:app.ts#create",
            "chan:inproc-event:order.placed",
            "PUBLISHES",
            "Confirmed",
        ),
        edge(
            "sym:mailer.ts#onPlaced",
            "chan:inproc-event:order.placed",
            "SUBSCRIBES",
            "Confirmed",
        ),
    ];
    (nodes, edges)
}

// AC-0015: each hop resolves at the lowest possible tier and records its
// tier — the whole T0 chain is walked end to end. (T-0015)
#[test]
fn hops_record_tier_across_the_full_chain() {
    let (nodes, edges) = event_chain();
    let flows = trace(&nodes, &edges);
    assert_eq!(flows.len(), 1, "one Endpoint trigger");
    let flow = &flows[0];
    assert_eq!(flow.trigger_name, "POST /orders");
    // Every hop carries tier + confidence (R-INT-2), all T0 at M3.
    assert_eq!(flow.hops.len(), 4);
    for hop in &flow.hops {
        assert_eq!(hop.tier, "Deterministic");
        assert_eq!(hop.confidence, "Confirmed");
        assert!(hop.evidence.as_deref().unwrap().contains("src/app.ts"));
    }
    // The chain crossed the channel out to the consumer.
    let last = flow.hops.iter().find(|h| h.label == "SUBSCRIBES").unwrap();
    assert_eq!(last.src, "chan:inproc-event:order.placed");
    assert_eq!(last.dst, "sym:mailer.ts#onPlaced");
}

// AC-0016: an unresolved hop truncates the branch at an explicit Gap —
// nothing is walked past it, and the flow is Partial, never silently
// complete. (T-0016)
#[test]
fn gap_truncates_the_branch() {
    let nodes = vec![
        node(
            "ep:POST:/notify",
            "Endpoint",
            serde_json::json!({"method": "POST", "path": "/notify"}),
        ),
        node(
            "sym:app.ts#notify",
            "Symbol",
            serde_json::json!({"name": "notify"}),
        ),
        node(
            "gap:chan:app.ts@100",
            "Gap",
            serde_json::json!({
                "reason": "runtime-computed channel identity",
                "attempted_tiers": ["T0", "T1", "T2"]
            }),
        ),
        // A consumer that would be reachable if the gap were guessed at.
        node(
            "chan:inproc-event:x",
            "Channel",
            serde_json::json!({"kind": "inproc-event", "identity": "x"}),
        ),
        node(
            "sym:app.ts#onX",
            "Symbol",
            serde_json::json!({"name": "onX"}),
        ),
    ];
    let edges = vec![
        edge(
            "ep:POST:/notify",
            "sym:app.ts#notify",
            "HANDLES",
            "Confirmed",
        ),
        edge(
            "sym:app.ts#notify",
            "gap:chan:app.ts@100",
            "PUBLISHES",
            "Gap",
        ),
        edge(
            "sym:app.ts#onX",
            "chan:inproc-event:x",
            "SUBSCRIBES",
            "Confirmed",
        ),
    ];
    let flows = trace(&nodes, &edges);
    let flow = flows
        .iter()
        .find(|f| f.trigger == "ep:POST:/notify")
        .unwrap();
    assert_eq!(flow.status, FlowStatus::Partial);
    // The gap hop is present and explicit…
    let gap_hop = flow.hops.iter().find(|h| h.confidence == "Gap").unwrap();
    assert_eq!(gap_hop.dst, "gap:chan:app.ts@100");
    assert!(gap_hop.dst_name.starts_with("GAP: runtime-computed"));
    assert_eq!(
        gap_hop.gap_reason.as_deref(),
        Some("runtime-computed channel identity")
    );
    assert_eq!(gap_hop.attempted_tiers, ["T0", "T1", "T2"]);
    // …and nothing was walked past it.
    assert!(flow.hops.iter().all(|h| h.dst != "sym:app.ts#onX"));
}

// AC-0017: flow_status per the §5.3 scoring rule. (T-0017)
#[test]
fn flow_status_follows_the_scoring_rule() {
    // All Confirmed → Verified, score 1.0.
    let (nodes, edges) = event_chain();
    let flows = trace(&nodes, &edges);
    assert_eq!(flows[0].status, FlowStatus::Verified);
    assert!((flows[0].score - 1.0).abs() < f64::EPSILON);

    // Any Gap → Partial; gap hops weigh 0.
    let nodes2 = vec![
        node(
            "ep:GET:/a",
            "Endpoint",
            serde_json::json!({"method": "GET", "path": "/a"}),
        ),
        node("sym:a.ts#h", "Symbol", serde_json::json!({"name": "h"})),
        node("gap:chan:a.ts@1", "Gap", serde_json::json!({"reason": "r"})),
    ];
    let edges2 = vec![
        edge("ep:GET:/a", "sym:a.ts#h", "HANDLES", "Confirmed"),
        edge("sym:a.ts#h", "gap:chan:a.ts@1", "PUBLISHES", "Gap"),
    ];
    let flows2 = trace(&nodes2, &edges2);
    assert_eq!(flows2[0].status, FlowStatus::Partial);
    assert!((flows2[0].score - 0.5).abs() < f64::EPSILON);

    // No gap, one inferred hop → Inferred (future tiers; synthetic here).
    let nodes3 = vec![
        node(
            "ep:GET:/b",
            "Endpoint",
            serde_json::json!({"method": "GET", "path": "/b"}),
        ),
        node("sym:b.ts#h", "Symbol", serde_json::json!({"name": "h"})),
        node("sym:b.ts#g", "Symbol", serde_json::json!({"name": "g"})),
    ];
    let edges3 = vec![
        edge("ep:GET:/b", "sym:b.ts#h", "HANDLES", "Confirmed"),
        edge("sym:b.ts#h", "sym:b.ts#g", "CALLS", "InferredStrong"),
    ];
    let flows3 = trace(&nodes3, &edges3);
    assert_eq!(flows3[0].status, FlowStatus::Inferred);
    assert!((flows3[0].score - 0.8).abs() < f64::EPSILON);
}

// A channel nothing local publishes to is a trigger (external event
// entering this repo slice); one with a local publisher is mid-flow.
#[test]
fn orphan_channel_is_a_trigger_published_channel_is_not() {
    let (nodes, edges) = event_chain();
    let mut nodes = nodes;
    let mut edges = edges;
    nodes.push(node(
        "chan:sqs-queue:external",
        "Channel",
        serde_json::json!({"kind": "sqs-queue", "identity": "external"}),
    ));
    nodes.push(node(
        "sym:worker.ts#work",
        "Symbol",
        serde_json::json!({"name": "work"}),
    ));
    edges.push(edge(
        "sym:worker.ts#work",
        "chan:sqs-queue:external",
        "SUBSCRIBES",
        "Confirmed",
    ));

    let flows = trace(&nodes, &edges);
    let triggers: Vec<&str> = flows.iter().map(|f| f.trigger.as_str()).collect();
    assert!(triggers.contains(&"chan:sqs-queue:external"));
    // order.placed has a local publisher — mid-flow, not a trigger.
    assert!(!triggers.contains(&"chan:inproc-event:order.placed"));
}

// Call cycles terminate; the traversal is bounded (US-0006 perf note).
#[test]
fn call_cycles_terminate() {
    let nodes = vec![
        node(
            "ep:GET:/c",
            "Endpoint",
            serde_json::json!({"method": "GET", "path": "/c"}),
        ),
        node("sym:c.ts#a", "Symbol", serde_json::json!({"name": "a"})),
        node("sym:c.ts#b", "Symbol", serde_json::json!({"name": "b"})),
    ];
    let edges = vec![
        edge("ep:GET:/c", "sym:c.ts#a", "HANDLES", "Confirmed"),
        edge("sym:c.ts#a", "sym:c.ts#b", "CALLS", "Confirmed"),
        edge("sym:c.ts#b", "sym:c.ts#a", "CALLS", "Confirmed"),
    ];
    let flows = trace(&nodes, &edges);
    assert_eq!(flows[0].hops.len(), 3);
    assert_eq!(flows[0].status, FlowStatus::Verified);
}

// Determinism (US-0014): same graph, same flows, regardless of input order.
#[test]
fn trace_is_deterministic_under_input_reordering() {
    let (nodes, edges) = event_chain();
    let mut rev_nodes = nodes.clone();
    rev_nodes.reverse();
    let mut rev_edges = edges.clone();
    rev_edges.reverse();

    let a = trace(&nodes, &edges);
    let b = trace(&rev_nodes, &rev_edges);
    assert_eq!(a.len(), b.len());
    for (fa, fb) in a.iter().zip(b.iter()) {
        assert_eq!(fa.trigger, fb.trigger);
        let ha: Vec<_> = fa.hops.iter().map(|h| (&h.label, &h.src, &h.dst)).collect();
        let hb: Vec<_> = fb.hops.iter().map(|h| (&h.label, &h.src, &h.dst)).collect();
        assert_eq!(ha, hb);
    }
}

// The depth bound never silently completes a flow (AC-0016, R-INT-4): a
// chain longer than the bound is emitted Partial and flagged truncated.
#[test]
fn depth_bound_marks_the_flow_partial_not_verified() {
    let mut nodes = vec![node(
        "ep:GET:/deep",
        "Endpoint",
        serde_json::json!({"method": "GET", "path": "/deep"}),
    )];
    let mut edges = vec![edge("ep:GET:/deep", "sym:d.ts#f0", "HANDLES", "Confirmed")];
    for i in 0..80 {
        nodes.push(node(
            &format!("sym:d.ts#f{i}"),
            "Symbol",
            serde_json::json!({"name": format!("f{i}")}),
        ));
        edges.push(edge(
            &format!("sym:d.ts#f{i}"),
            &format!("sym:d.ts#f{}", i + 1),
            "CALLS",
            "Confirmed",
        ));
    }
    nodes.push(node(
        "sym:d.ts#f80",
        "Symbol",
        serde_json::json!({"name": "f80"}),
    ));

    let flows = trace(&nodes, &edges);
    let flow = &flows[0];
    assert!(flow.depth_limited, "the 81-hop chain exceeds the bound");
    assert_eq!(
        flow.status,
        FlowStatus::Partial,
        "a bounded trace must not report Verified"
    );

    // A short chain stays unflagged and Verified.
    let (nodes, edges) = event_chain();
    let flows = trace(&nodes, &edges);
    assert!(!flows[0].depth_limited);
    assert_eq!(flows[0].status, FlowStatus::Verified);
}

// M4 exit gate: flows anchor at the Screen — the user action leads the
// trace through RENDERS/FETCHES into the server chain, and the fetched
// endpoint stops being its own trigger.
#[test]
fn screen_anchored_flow_walks_the_full_chain() {
    let (mut nodes, mut edges) = event_chain();
    nodes.push(node(
        "screen:/checkout",
        "Screen",
        serde_json::json!({"route": "/checkout"}),
    ));
    nodes.push(node(
        "sym:client.tsx#Checkout",
        "Component",
        serde_json::json!({"name": "Checkout"}),
    ));
    edges.push(edge(
        "screen:/checkout",
        "sym:client.tsx#Checkout",
        "RENDERS",
        "Confirmed",
    ));
    edges.push(edge(
        "sym:client.tsx#Checkout",
        "ep:POST:/orders",
        "FETCHES",
        "Confirmed",
    ));

    let flows = trace(&nodes, &edges);
    // One flow: the screen. The fetched endpoint is mid-flow, not a trigger.
    assert_eq!(flows.len(), 1);
    let flow = &flows[0];
    assert_eq!(flow.trigger, "screen:/checkout");
    assert_eq!(flow.trigger_kind, "Screen");
    assert_eq!(flow.trigger_name, "Screen /checkout");
    // Screen -> component -> endpoint -> handler -> ... -> consumer: the
    // whole chain under one anchor, all hops Confirmed.
    let labels: Vec<&str> = flow.hops.iter().map(|h| h.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "RENDERS",
            "FETCHES",
            "HANDLES",
            "CALLS",
            "PUBLISHES",
            "SUBSCRIBES"
        ]
    );
    assert_eq!(flow.status, FlowStatus::Verified);
}

// An endpoint nothing fetches keeps its own flow (external API consumers
// exist); a fetched one does not double-report.
#[test]
fn unfetched_endpoints_remain_triggers() {
    let (mut nodes, mut edges) = event_chain();
    nodes.push(node(
        "ep:GET:/health",
        "Endpoint",
        serde_json::json!({"method": "GET", "path": "/health"}),
    ));
    nodes.push(node(
        "screen:/checkout",
        "Screen",
        serde_json::json!({"route": "/checkout"}),
    ));
    nodes.push(node(
        "sym:c.tsx#C",
        "Component",
        serde_json::json!({"name": "C"}),
    ));
    edges.push(edge(
        "screen:/checkout",
        "sym:c.tsx#C",
        "RENDERS",
        "Confirmed",
    ));
    edges.push(edge(
        "sym:c.tsx#C",
        "ep:POST:/orders",
        "FETCHES",
        "Confirmed",
    ));

    let flows = trace(&nodes, &edges);
    let triggers: Vec<&str> = flows.iter().map(|f| f.trigger.as_str()).collect();
    assert!(triggers.contains(&"screen:/checkout"));
    assert!(triggers.contains(&"ep:GET:/health"), "unfetched endpoint");
    assert!(
        !triggers.contains(&"ep:POST:/orders"),
        "fetched endpoint is mid-flow"
    );
}

// A fetch that could not resolve truncates the screen's flow at its Gap —
// the client-side analog of the channel Gap (R-INT-4).
#[test]
fn unresolved_fetch_truncates_the_screen_flow() {
    let nodes = vec![
        node(
            "screen:/admin",
            "Screen",
            serde_json::json!({"route": "/admin"}),
        ),
        node(
            "sym:a.tsx#Admin",
            "Component",
            serde_json::json!({"name": "Admin"}),
        ),
        node(
            "gap:fetch:a.tsx@30",
            "Gap",
            serde_json::json!({"reason": "runtime-computed fetch URL"}),
        ),
    ];
    let edges = vec![
        edge("screen:/admin", "sym:a.tsx#Admin", "RENDERS", "Confirmed"),
        edge("sym:a.tsx#Admin", "gap:fetch:a.tsx@30", "FETCHES", "Gap"),
    ];
    let flows = trace(&nodes, &edges);
    assert_eq!(flows[0].status, FlowStatus::Partial);
    let gap_hop = flows[0]
        .hops
        .iter()
        .find(|h| h.confidence == "Gap")
        .unwrap();
    assert_eq!(gap_hop.label, "FETCHES");
    assert!(
        gap_hop
            .dst_name
            .starts_with("GAP: runtime-computed fetch URL")
    );
}

// A FETCHES edge from a component no screen renders must not suppress the
// endpoint's own flow — coverage comes from actual screen traversal, not
// the mere existence of an edge.
#[test]
fn endpoint_fetched_only_by_an_unrendered_component_keeps_its_flow() {
    let (mut nodes, mut edges) = event_chain();
    nodes.push(node(
        "screen:/about",
        "Screen",
        serde_json::json!({"route": "/about"}),
    ));
    nodes.push(node(
        "sym:about.tsx#About",
        "Component",
        serde_json::json!({"name": "About"}),
    ));
    nodes.push(node(
        "sym:dead.tsx#Unused",
        "Component",
        serde_json::json!({"name": "Unused"}),
    ));
    edges.push(edge(
        "screen:/about",
        "sym:about.tsx#About",
        "RENDERS",
        "Confirmed",
    ));
    // The only fetch of the endpoint sits in a component nothing renders.
    edges.push(edge(
        "sym:dead.tsx#Unused",
        "ep:POST:/orders",
        "FETCHES",
        "Confirmed",
    ));

    let flows = trace(&nodes, &edges);
    let triggers: Vec<&str> = flows.iter().map(|f| f.trigger.as_str()).collect();
    assert!(
        triggers.contains(&"ep:POST:/orders"),
        "no screen reaches the fetch, so the server flow must not disappear"
    );
}

/// WebExtension dogfood shape: background context → entry file → its
/// symbols → an internally published chrome-message channel → the content
/// script consumer.
fn webext_chain() -> (Vec<Node>, Vec<Edge>) {
    let nodes = vec![
        node(
            "extctx:acme/ext@manifest.json:service-worker:background.js",
            "ExtensionContext",
            serde_json::json!({"kind": "service-worker", "entries": ["src/background.ts"]}),
        ),
        node(
            "file:acme/ext@src/background.ts",
            "File",
            serde_json::json!({"path": "src/background.ts"}),
        ),
        node(
            "sym:background.ts#onSave",
            "Symbol",
            serde_json::json!({"name": "onSave"}),
        ),
        node(
            "chan:chrome-message:SAVE_SNAPSHOT",
            "Channel",
            serde_json::json!({"kind": "chrome-message", "identity": "SAVE_SNAPSHOT"}),
        ),
        node(
            "sym:content.ts#onMessage",
            "Symbol",
            serde_json::json!({"name": "onMessage"}),
        ),
    ];
    let edges = vec![
        edge(
            "extctx:acme/ext@manifest.json:service-worker:background.js",
            "file:acme/ext@src/background.ts",
            "ENTRY",
            "Confirmed",
        ),
        // Stored symbol → file; the tracer walks it file → symbol.
        edge(
            "sym:background.ts#onSave",
            "file:acme/ext@src/background.ts",
            "DEFINED_IN",
            "Confirmed",
        ),
        edge(
            "sym:background.ts#onSave",
            "chan:chrome-message:SAVE_SNAPSHOT",
            "PUBLISHES",
            "Confirmed",
        ),
        edge(
            "sym:content.ts#onMessage",
            "chan:chrome-message:SAVE_SNAPSHOT",
            "SUBSCRIBES",
            "Confirmed",
        ),
    ];
    (nodes, edges)
}

// AC-0083: an ExtensionContext is a user-action anchor — the flow enters
// through ENTRY, fans from the entry file into its symbols via reverse
// DEFINED_IN, and crosses the internally published channel to its
// consumer, deterministically under input reordering. (T-0083)
#[test]
fn extension_context_flow_walks_entry_and_defined_in_reverse() {
    let (nodes, edges) = webext_chain();
    let flows = trace(&nodes, &edges);

    // The context is the only trigger: the chrome-message channel has a
    // local publisher, so it is mid-flow, never its own anchor.
    assert_eq!(flows.len(), 1);
    let flow = &flows[0];
    assert_eq!(flow.trigger_kind, "ExtensionContext");
    assert_eq!(flow.trigger_name, "service-worker · src/background.ts");
    let labels: Vec<&str> = flow.hops.iter().map(|h| h.label.as_str()).collect();
    assert_eq!(labels, ["ENTRY", "DEFINED_IN", "PUBLISHES", "SUBSCRIBES"]);
    // The reverse walk records traversal direction: file → symbol.
    let defined = flow.hops.iter().find(|h| h.label == "DEFINED_IN").unwrap();
    assert_eq!(defined.src, "file:acme/ext@src/background.ts");
    assert_eq!(defined.dst, "sym:background.ts#onSave");
    assert_eq!(flow.status, FlowStatus::Verified);

    // Determinism (US-0014): reversed input, identical hops.
    let mut rev_nodes = nodes.clone();
    rev_nodes.reverse();
    let mut rev_edges = edges.clone();
    rev_edges.reverse();
    let again = trace(&rev_nodes, &rev_edges);
    let a: Vec<_> = flow
        .hops
        .iter()
        .map(|h| (&h.label, &h.src, &h.dst))
        .collect();
    let b: Vec<_> = again[0]
        .hops
        .iter()
        .map(|h| (&h.label, &h.src, &h.dst))
        .collect();
    assert_eq!(a, b);
}

// AC-0083: a manifest Command anchors its own flow and reaches the
// handling context through HANDLES, then the context's chain. (T-0083)
#[test]
fn command_flow_reaches_the_background_handler_chain() {
    let (mut nodes, mut edges) = webext_chain();
    nodes.push(node(
        "extcmd:acme/ext@manifest.json:save-snapshot",
        "Command",
        serde_json::json!({"name": "save-snapshot", "description": "Save it"}),
    ));
    edges.push(edge(
        "extcmd:acme/ext@manifest.json:save-snapshot",
        "extctx:acme/ext@manifest.json:service-worker:background.js",
        "HANDLES",
        "Confirmed",
    ));

    let flows = trace(&nodes, &edges);
    let command = flows
        .iter()
        .find(|f| f.trigger_kind == "Command")
        .expect("the command anchors a flow");
    assert_eq!(command.trigger_name, "Command save-snapshot");
    let labels: Vec<&str> = command.hops.iter().map(|h| h.label.as_str()).collect();
    assert_eq!(
        labels,
        ["HANDLES", "ENTRY", "DEFINED_IN", "PUBLISHES", "SUBSCRIBES"]
    );
    // The context stays an anchor of its own — both entries are real.
    assert!(flows.iter().any(|f| f.trigger_kind == "ExtensionContext"));
}

// AC-0084: the anchor probes name every kind sought with the count found —
// the zero-flow surface reports what recovery looked for, deterministically.
// (T-0084)
#[test]
fn anchor_probes_count_every_kind_sought() {
    let (nodes, edges) = webext_chain();
    let probes = anchor_probes(&nodes, &edges);
    let as_pairs: Vec<(&str, usize)> = probes.iter().map(|p| (p.kind.as_str(), p.found)).collect();
    assert_eq!(
        as_pairs,
        [
            ("screens (routes/pages)", 0),
            ("extension contexts (popup, background, content scripts)", 1),
            ("extension commands (keyboard shortcuts)", 0),
            ("HTTP endpoints", 0),
            // SAVE_SNAPSHOT is published locally — not an external channel.
            ("externally published event channels", 0),
        ]
    );

    // An empty slice still names every kind, all zero.
    let none = anchor_probes(&[], &[]);
    assert_eq!(none.len(), 5);
    assert!(none.iter().all(|p| p.found == 0));

    // A consumed-but-never-published channel counts as external.
    let (mut nodes, mut edges) = webext_chain();
    edges.retain(|e| e.label != "PUBLISHES");
    nodes.push(node(
        "screen:/x",
        "Screen",
        serde_json::json!({"route": "/x"}),
    ));
    let probes = anchor_probes(&nodes, &edges);
    let find = |kind: &str| probes.iter().find(|p| p.kind.contains(kind)).unwrap().found;
    assert_eq!(find("screens"), 1);
    assert_eq!(find("externally published"), 1);
}

/// `Endpoint → HANDLES → sym:run`, where `run` owns TS-adapter-shaped Gaps
/// through `DEPENDS_ON` (#473): `const CODE = load(); eval(CODE);` yields an
/// `eval-unproven` Gap; `extra` adds rule-evidence or ordering dependencies.
fn eval_owner_chain(extra_nodes: Vec<Node>, extra_edges: Vec<Edge>) -> (Vec<Node>, Vec<Edge>) {
    let mut nodes = vec![
        node(
            "ep:POST:/run",
            "Endpoint",
            serde_json::json!({"method": "POST", "path": "/run"}),
        ),
        node(
            "sym:app.ts#run",
            "Symbol",
            serde_json::json!({"name": "run"}),
        ),
        node(
            "sym:app.ts#load",
            "Symbol",
            serde_json::json!({"name": "load"}),
        ),
    ];
    let mut edges = vec![
        edge("ep:POST:/run", "sym:app.ts#run", "HANDLES", "Confirmed"),
        edge("sym:app.ts#run", "sym:app.ts#load", "CALLS", "Confirmed"),
    ];
    nodes.extend(extra_nodes);
    edges.extend(extra_edges);
    (nodes, edges)
}

fn flow_for<'a>(flows: &'a [Flow], trigger: &str) -> &'a Flow {
    flows.iter().find(|f| f.trigger == trigger).unwrap()
}

// AC-0219: a flow symbol that DEPENDS_ON an execution Gap (an eval whose
// code T0 could not prove) makes the flow Partial, and the Gap is an
// explicit, terminal hop — never a silent Verified (R-INT-4). (T-0219)
#[test]
fn depends_on_execution_gap_makes_the_flow_partial() {
    let gap = "gap:r@src/app.ts#eval-unproven@120";
    let reason = "const-shaped code argument could not be proven to a literal";
    let (nodes, edges) = eval_owner_chain(
        vec![node(
            gap,
            "Gap",
            serde_json::json!({
                "construct": "eval",
                "reason": reason,
                "attempted_tiers": ["T0"],
            }),
        )],
        vec![edge("sym:app.ts#run", gap, "DEPENDS_ON", "Gap")],
    );
    let flows = trace(&nodes, &edges);
    let flow = flow_for(&flows, "ep:POST:/run");
    assert_eq!(flow.status, FlowStatus::Partial);
    let hop = flow.hops.iter().find(|h| h.label == "DEPENDS_ON").unwrap();
    assert_eq!(hop.src, "sym:app.ts#run");
    assert_eq!(hop.dst, gap);
    assert_eq!(hop.confidence, "Gap");
    assert_eq!(hop.gap_reason.as_deref(), Some(reason));
    assert_eq!(hop.attempted_tiers, ["T0"]);
    // The confirmed call is still walked; only the Gap is terminal.
    assert!(flow.hops.iter().any(|h| h.dst == "sym:app.ts#load"));
    assert!((flow.score - 2.0 / 3.0).abs() < 1e-9);

    // Without the Gap the same flow is Verified: the Gap alone decides.
    let (nodes, edges) = eval_owner_chain(Vec::new(), Vec::new());
    assert_eq!(
        flow_for(&trace(&nodes, &edges), "ep:POST:/run").status,
        FlowStatus::Verified
    );

    // A Gap the node set does not carry still counts by its `gap:` id.
    let (_, edges) = eval_owner_chain(
        Vec::new(),
        vec![edge("sym:app.ts#run", gap, "DEPENDS_ON", "Gap")],
    );
    let (nodes, _) = eval_owner_chain(Vec::new(), Vec::new());
    assert_eq!(
        flow_for(&trace(&nodes, &edges), "ep:POST:/run").status,
        FlowStatus::Partial
    );
}

// AC-0219: rule-evidence scope Gaps (`rule_evidence_gap`, including the
// `eval-rules` source-mapping Gap) do not make a flow Partial, a
// DEPENDS_ON between non-Gap facts is not a flow hop, and a Gap owned by a
// symbol the flow never reaches does not touch it. (T-0219)
#[test]
fn rule_evidence_and_ordering_dependencies_leave_the_flow_verified() {
    let rules = "gap:r@src/app.ts#eval-rules@120";
    let scope = "gap:r@src/app.ts#rule-analysis@40";
    let elsewhere = "gap:r@src/other.ts#eval-unproven@9";
    let (nodes, edges) = eval_owner_chain(
        vec![
            node(
                rules,
                "Gap",
                serde_json::json!({
                    "rule_evidence_gap": true,
                    "reason_code": "eval_source_mapping_unknown",
                    "reason": "awaits an exact mapping",
                }),
            ),
            node(
                scope,
                "Gap",
                serde_json::json!({"rule_evidence_gap": true, "reason": "scope"}),
            ),
            node(
                "sym:other.ts#unused",
                "Symbol",
                serde_json::json!({"name": "unused"}),
            ),
            node(elsewhere, "Gap", serde_json::json!({"reason": "unproven"})),
        ],
        vec![
            edge("sym:app.ts#run", rules, "DEPENDS_ON", "Gap"),
            edge("sym:app.ts#run", scope, "DEPENDS_ON", "Gap"),
            // Ordering intent between two recovered facts: never walked.
            edge(
                "sym:app.ts#run",
                "sym:other.ts#unused",
                "DEPENDS_ON",
                "Confirmed",
            ),
            edge("sym:other.ts#unused", elsewhere, "DEPENDS_ON", "Gap"),
        ],
    );
    let flows = trace(&nodes, &edges);
    let flow = flow_for(&flows, "ep:POST:/run");
    assert_eq!(flow.status, FlowStatus::Verified);
    assert!(flow.hops.iter().all(|h| h.label != "DEPENDS_ON"));
    assert_eq!(flow.hops.len(), 2);
}
