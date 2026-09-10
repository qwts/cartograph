# SPEC-01 — Domain context hub and agent interoperability

Status: implementation in progress. Parent issues: #250, #255; first slice: #380.
Decision: [ADR-0019](adr/ADR-0019-domain-context-and-agent-interoperability.md).
This specification extends SPEC-00; the provenance and escalation invariants remain binding.

## Product contract

Cartograph is a persistent, local-first context hub composed of business domains.
Each domain connects features, business rules, flows, data, designs, decisions,
tests, and gaps to source evidence. Humans and agents use the same context.
Language and infrastructure adapters supply evidence; folders and languages do
not by themselves establish business-domain boundaries.

The first user is the owner investigating and modernizing existing repositories.
The initial external proving ground is Vendure's cart-readiness transition, with
a pinned revision, explicit configuration, and an independently reviewed expected
set. Support for arbitrary repositories is an eventual coverage objective, not a
claim that every framework or business rule is already recovered.

## Knowledge and review

Keep four meanings distinct: implemented behavior, documented intent, inferred
interpretation, and proposed future design. A source document proves that an
intention was written; it does not prove the code implements it. Missing design
evidence means unknown, not proof that no design exists. Feature inventories name
their scope, source revision, uncovered portions, and confidence.

Domain/feature membership and proposed rules require typed, cited relationships.
T3 can propose interpretations, designs, and changes; human acceptance records a
review decision without upgrading their producing tier or confidence ceiling.
Accepted proposals must enter a shared curated projection used by the UI, exports,
and agent reads. Re-ingestion marks unsupported or changed citations stale and
requires reconciliation. The recovered graph remains separately addressable.

## Shared reads — first implementation slice

The `context-hub` crate provides an immutable recovered-graph snapshot and a
transport-independent query contract. The app exposes it through a Tauri command
on a blocking worker, with the graph lock held only while copying the graph.
Nodes and edges are copied within one SQLite read transaction so another app
process cannot interleave a commit and create a snapshot that never existed.
It makes no model, network, filesystem-evidence, or target-write calls.

- A snapshot identifies the canonical complete node/edge content with a versioned
  content hash. Input ordering does not change it; changing any property does.
  Duplicate node identities or duplicate directed edge identities are errors.
- Fact references are a tagged node id or a typed `(source, label, destination)`
  edge identity, not an ambiguous concatenated string. Responses identify the
  `recovered_graph` view; they do not claim to contain accepted proposals yet.
- Queries select all facts or a bounded undirected neighborhood of a known node,
  and optionally filter by fact kind and exact labels. Nodes sort by id, edges by
  `(source, label, destination)`, with nodes preceding edges. A neighborhood
  includes induced edges; missing anchors are errors, not fabricated facts.
- A request has item and serialized-response byte budgets, with hard caps of
  500 facts, 1 MiB, and three neighborhood hops. Zero or excessive limits fail
  explicitly. The service never silently skips an oversized fact to fit later
  facts. If even the next fact and envelope cannot fit, it returns a budget error.
- Paging cursors bind the snapshot and normalized selection. Reusing a cursor
  after source changes or for another selection is an error. Every response
  includes the snapshot id, total selected count, returned facts, and an explicit
  next cursor when more remain. Changing only budgets is allowed between pages.
- Each fact carries validated provenance or an explicit provenance problem and
  `Gap` confidence. Raw `prov` properties are removed from returned properties;
  only validated provenance is exposed as authoritative metadata. Inference never
  becomes confirmed. Evidence references remain available through that metadata.
- Label filtering for domain concepts with no producer returns an empty result;
  the service must not manufacture a domain or feature from directory names.

This content identity is a query consistency contract. It does not fix upstream
location-dependent extraction identities (#342). Rebuilding snapshots per query
is the initial app implementation; reusable snapshot caching/index invalidation
needs a measured large-repository gate before broad rollout.

The Tauri command is `query_context` with a `request` argument. For example:

```json
{
  "request": {
    "scope": { "type": "neighborhood", "anchor": "symbol:example", "hops": 1 },
    "kind": null,
    "labels": [],
    "max_facts": 100,
    "max_bytes": 65536,
    "cursor": null
  }
}
```

Use an actual recovered node identity for the anchor. Continue with the returned
`next_cursor` and the same selection. Typed references, validated provenance,
`confidence_tier`, and `provenance_problem` accompany each fact. Cursor identities
are consistency tokens, not authorization credentials. No MCP endpoint is implied
by the existence of this Tauri command.

## MCP ingress, ACP execution

External agents use Cartograph's MCP server to read context and request work.
The app and MCP ingress call one durable task coordinator. That coordinator can
run named internal specialists and launch compatible agent runtimes through an
ACP client. ACP workers can receive Cartograph MCP context tools in their session.
The MCP server and ACP client are separate protocol adapters, not separate copies
of domain state or orchestration policy.

Tasks bind a project, graph revision, scope, agent identity/version, permitted
tools, tier ceiling, input references, budget, origin, and parent task. Starting
returns a durable id promptly. Progress, cancellation, failure, proposals, and
results remain queryable after reconnect/restart. Unknown runtime outcomes after
a crash require reconciliation rather than automatic duplicate execution.

Negotiate and pin protocol/runtime capabilities. MCP Tasks support is optional;
explicit start/status/result/cancel tools provide the compatibility baseline.
ACP permissions are not a filesystem sandbox. The coordinator enforces tool
allowlists and per-tier egress, and the runtime boundary must enforce read-only
access to source plus explicitly scoped output storage. No cloud launch under
local-only policy. No permissions accepted automatically from repository text.
Parent ancestry, delegation-depth limits, deduplication, and shared budgets stop
MCP → ACP → MCP delegation loops and unbounded recursive work.

## Architecture evaluation and modernization

Evaluate a selected feature or domain. Compute structural evidence deterministically
(size, dependencies, cycles, fan-in/out, cohesion proxies and change history when
available). Specialists interpret the measurements against responsibilities,
documented intent, and exceptions. A large file alone is not a god-file finding.
Each finding states its evidence, scope, uncertainty, impact, and proposed remedy.
Missing dependencies or history reduce coverage explicitly.

Modernization connects recovered behavior to approved future design and executable
acceptance criteria. An external coding agent can implement an approved change
in a separately authorized checkout; Cartograph re-ingests it to compare behavior
and intent. This architecture does not authorize target writes or automatic code
generation in the initial implementation.

## Delivery gates

| Gate | Observable result | Current state |
|---|---|---|
| H1 Shared reads | App/core contract passes revision, budget, provenance and paging tests | Implementing in #380 |
| H2 Domain truth | Pinned Vendure feature yields cited rules, feature membership and honest coverage against independent expected cases | Planned; guarded-exit evidence starts in #381; [benchmark](evals/vendure-cart-readiness.md) awaits execution |
| H3 Curated context | Accepted proposals appear consistently in UI, exports and agent context; stale evidence reconciles | Planned; existing decision log alone is insufficient |
| H4 Agent investigation | Durable specialist task reads context and returns cited, reviewable findings in app | Planned; #256 |
| H5 MCP + ACP | A real external MCP caller starts an ACP investigation, observes progress, cancels/reconnects, and retrieves its result | Planned; protocol adapters not shipped by H1 |
| H6 Feature evaluation | Structural measurements and contextual findings distinguish justified large files from responsibility hotspots | Planned |
| H7 Market pilot | New user independently ingests a repo, asks the core domain questions, and completes an evidence-backed improvement with measured time/cost | Planned |

Do not treat these gates as elapsed-time estimates or mark the product ready from
test counts. The first market hypothesis is developers responsible for unfamiliar
or under-documented projects. Validate the workflow with the owner first, then
several external users; packaging, onboarding, recovery latency, credentials,
coverage explanations, and trustworthy output are launch requirements.

Protocol references: [ACP overview](https://agentclientprotocol.com/protocol/v1/overview),
[ACP sessions](https://agentclientprotocol.com/protocol/v1/session-setup),
[MCP Tasks](https://modelcontextprotocol.io/extensions/tasks/overview).
