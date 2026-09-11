# ADR-0027 — Bind agent tasks to coherent retained source selections

- **Status:** Accepted
- **Date:** 2026-09-10
- **Deciders:** Chris Kane, Cartograph implementation agent

## Context

Primary-source receipts identify the bytes participating TypeScript producers
parsed. Existing escalation still reads mutable working trees, and staging v1
binds copied text without recording the selected receipt. Equal graph facts and
equal cited text can accompany a different source capture. An agent result needs
an immutable record of its actual evidence, without pretending this establishes
complete business meaning or permitting historical records to acquire new claims.

## Decision

Implement [SPEC-09](../SPEC-09_captured-task-basis.md) under #401. Prepare bounded
fact-qualified tasks from a coherent graph/current-association transaction,
acquire sorted per-source guards outside store mutexes, and recheck the selection.
Read participating occurrences through their original complete receipt and strict
retained span. Only valid association absence permits the explicitly unverified
legacy reader. Release guards after copying evidence and before model execution.

Use a validated PreparedAgentTask wrapper and an explicit immutable staging v2
contract. New task/stage and exact consent identities bind all selected source
metadata, supplied evidence and candidates. Preserve literal v1 serialization,
IDs and review history. Stage completed results with their original basis even
when recovery, forgetting or cancellation occurs during the call. A separate
read-only assessment reports graph, association and availability dimensions.
Acceptance never upgrades T3/InferredWeak or activates curated context.

Bound metadata lookahead and attempted requests independently of supplied-item
limits; disclose skipped/unread tails. Charge complete captured file lengths to a
128 MiB per-task validation budget, including repeated reads, while retaining
existing source/output/receipt limits. A participating failure is terminal for
that task, never a fallback or substituted candidate.

## Consequences

Developers and agents can inspect which parser input a result actually used.
Equal-text recapture invalidates old consent and creates a different task basis.
Historical proposals remain interpretable after restart without copying raw task
text into proposals.sqlite. Whole-graph comparison, receipt/manifest validation,
source locks and the additional metadata have explicit costs; no global
cross-store transaction or performance claim follows.

The existing relation allowlist and adjacent-slot heuristic remain limited.
Business-rule DEPENDS_ON gaps do not automatically become resolvable. Complete
producer input closure, exact directed-slot semantics, H3 projection, domain
specialists and MCP/ACP execution remain separate tracked work.

## Alternatives

1. Continue working-tree reads: cannot bind the producing bytes and can silently
   change evidence after ingestion.
2. Retrospectively attach receipts to v1 history: changes the meaning of reviewed
   immutable records without proving which bytes the old task received.
3. Hold source and graph locks throughout model execution: blocks recovery and
   forgetting, creates long-lived contention, and still does not prove semantics.
