# ADR-0028 — Separate durable investigations from edge-resolution proposals

- **Status:** Accepted for implementation in #404
- **Date:** 2026-09-11
- **Deciders:** Chris Kane (product direction), Cartograph implementation agent

## Context

The owner wants to ask a domain context hub about business behavior, designs and
gaps, with multiple specialists and eventual MCP/ACP interoperability. Existing
AgentTask is a closed edge-resolution contract requiring a Gap, source and target
candidates. It cannot truthfully represent a general investigation. Shared context,
retained source receipts and execution ownership now provide reusable foundations,
but there is no durable in-app investigation/coordinator path.

## Decision

Implement [SPEC-10](../SPEC-10_specialist-investigations.md) with separate versioned
investigation actions, input ledgers and findings. A host coordinator owns bounded
query/read/finish steps, frozen graph context and cumulative coherent source
selection. Ship two honest T3 specialists and keep every finding InferredWeak.
Use a closed JSON action protocol over a separately bounded provider API; do not
pretend this is native provider tools or an ACP runtime.

Use durable idempotent task identity and ordered events, exact per-step cloud
consent, private execution fencing and explicit unknown outcomes. Preserve complete
admitted findings against their original transient input even after cancellation.
Do not recreate lost frozen context or replay uncertain calls on restart. The app
and future protocol adapters share coordinator records and policy.

Keep raw source/tool payloads transient and store bounded redacted question/history,
references, fingerprints and admitted findings. Historical source inspection remains
receipt-pinned. Use state.db transactions for investigation identity/lifecycle,
results, receipt references and initial Job linkage. Attach execution identity
atomically with the Job claim; history survives job cleanup.

## Consequences

A developer can ask an actual scoped question and inspect a specialist's evidence
and findings without inventing graph edges or upgrading recovered facts. Tool,
byte, output, time and consent boundaries are explicit. The new bounded provider
transport also needs real TLS and body-size/stop-condition validation.

Replay admission distinguishes complete source excerpts from individual graph
metadata scalars: both use the 48-scalar window, while only source excerpts and
supplied prior-finding text reject complete short items. Short graph identifiers
and literals remain usable in ordinary answers; secret scanning is unchanged.
This is exact-copy protection, not semantic declassification.

The implementation adds a substantive coordinator and app surface. Whole-graph
copying remains costly, cancellation cannot promise remote inference termination,
and RAM-only inputs constrain restart to explicit interruption/reconciliation.
H4 cross-ingress acceptance still requires real MCP; H3 activation, H5 ACP/delegation,
H6 metrics and the market pilot remain separate obligations.

## Alternatives

1. Reuse edge proposals with synthetic Gap/target records: misrepresents questions
   and findings and introduces false graph semantics.
2. Add only one-shot free-text chat: does not investigate through tools, establish
   a durable evidence ledger or support controlled cancellation/consent.
3. Persist raw prompts/source and automatically resume after restart: broadens
   retention and disclosure, can reconstruct the wrong basis and duplicate calls.
