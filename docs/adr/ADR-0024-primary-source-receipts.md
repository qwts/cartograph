# ADR-0024 — Producer receipts and explicit current source associations

- **Status:** Accepted for the staged implementation in #395
- **Date:** 2026-09-10
- **Deciders:** Cartograph owner direction; implementation agent within SPEC-01

## Context

Retained source bytes and logical source registration do not establish which input
a producer parsed. Equal complete facts can arise from different raw captures, and
the current TS cache rewrites provenance revisions. Legacy citation or graph-hash
lookup would silently conflate those inputs. Reliable captured inspection is a
prerequisite for the context hub's later versioned agent task basis.

## Decision

Parse immutable captured buffers in the production TypeScript lane and mint
versioned per-fact receipts for direct lexical rule/owner observations. Bind the
complete emitted fact, original ranges, registered ownership and producer/grammar
identity. Keep receipts outside graph properties and legacy provenance/staging.
Disable participating TS cache reuse until cache attestation is implemented.

Publish explicit current fact-to-receipt associations in the same SQLite graph
transaction as repository reconciliation. Ordinary fact mutations invalidate the
association. A current source request retains the expected receipt ID even when
its graph snapshot and semantic facts remain identical. Persist captures/immutable
receipts first and make missing retained content explicitly unavailable.

Ship private source-owned retention, bounded capacities, selected-object reads and
previewed reference-aware forgetting with the source viewer. Persistent per-source
OS guards exclude forgetting from capture/publication/read operations. Forget raw
content while retaining immutable receipt/review metadata; a later explicit recovery
may restore exactly matching content but never substitute changed bytes.

## Consequences

The app can inspect original primary source after edits/restart without asserting
complete input closure, business interpretation, freshness or review eligibility.
Parsing all participating TS files costs more than the old cache; bounds and honest
failure replace an implicit unbounded retention commitment. Additional storage and
current-association identities are needed, and raw-source retention remains local.
Earlier/later cross-layer enrichment does not acquire verification through proximity.
H2, full #385, H3 and MCP/ACP remain separate delivery work.

## Alternatives

1. Look up source by EvidenceRef or full fact hash: rejected because equal facts
   can result from different captures, including trailing-comment changes.
2. Add a capture ID after parsing live files: rejected because a later read cannot
   attest the original invocation and would misrepresent transformed inputs.
3. Verify every adapter/configuration input in one release: deferred; direct
   lexical primary-source scope is explicit and preserves unresolved coverage.
