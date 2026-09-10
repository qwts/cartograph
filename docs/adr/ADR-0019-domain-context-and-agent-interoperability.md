# ADR-0019 — Domain context hub with shared reads and MCP/ACP orchestration

- **Status:** Accepted direction; implementation staged by SPEC-01
- **Date:** 2026-09-10
- **Deciders:** Chris Kane (product direction), implementation agents

## Context

The owner clarified that Cartograph is a persistent context hub composed of
business domains, shared by humans, in-app specialists and external development
agents. The current recovery engine is its evidence foundation. SPEC-00's
specification-only scope and VISION's deferred agent workflows no longer describe
the intended product. An incoming MCP request may require an ACP-backed agent
investigation rather than a passive graph lookup.

## Decision

Adopt [SPEC-01](../SPEC-01_context-hub.md) as the staged post-M10 product contract.
Use one context service and curated projection across application and agent
transports. Separate implemented facts, documented intent, interpretations, and
future designs. Keep provenance, explicit gaps, tier ceilings and per-tier egress
unchanged. Introduce bounded, revision-bound recovered-graph reads first (#380).

MCP is the external context/task interface; ACP is a compatible runtime execution
interface behind a durable coordinator shared with the app. Long-running work
has task identity, progress, cancellation, budgets, ancestry and recoverable results.
Protocols do not confer trust or sandbox permissions. Runtime versions/capabilities
must be negotiated and tested before claiming integration.

This supersedes ADR-0007's sequencing of the context/agent product behind a
starter-kit export. It does not authorize writing the ingested target repository:
modernization implementation requires a separately authorized external checkout.
No automatic regeneration or target writes ship with the context-read foundation.

## Consequences

The recovery and provenance implementation is retained. Domain/feature recovery,
curation projection, orchestration, interoperability, and architecture evaluation
have separate observable delivery gates; a graph-only query API fulfills none of
those later gates by itself. Vendure provides a bounded business-domain benchmark.

The first context snapshot copies and canonicalizes the recovered graph. This is
simple to verify but needs caching and scale measurements before general rollout.
The content hash detects changes, including malformed provenance, without claiming
to repair location-dependent identities in upstream extraction.

## Alternatives

1. Keep exported specification files as the primary product: does not fulfill the
   owner's persistent, shared domain-context workflow.
2. Expose the database directly to agents: bypasses a bounded, typed contract and
   makes transport-specific policy and interpretation likely.
3. Let each protocol adapter orchestrate its own agents: duplicates budgets,
   permissions and task state, and makes recursive delegation harder to control.
