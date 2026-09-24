# ADR-0032 — Owned execution Gaps make a flow Partial

- **Status:** Proposed in #473 (owner review)
- **Date:** 2026-09-24
- **Deciders:** Cartograph owner; implementation agent

## Context

The flow tracer classified completeness only from the hops it walked
(`RENDERS`, `FETCHES`, `HANDLES`, `ENTRY`, `DEFINED_IN`, `CALLS`,
`PUBLISHES`, `SUBSCRIBES`). Some adapters record a Gap against its *owner*
instead of a hop: the TS adapter hangs every eval-related and rule-evidence
Gap off the owning symbol (or file) through `DEPENDS_ON`. The tracer never
saw them. A flow through `run() { const CODE = load(); eval(CODE); }`
reported **Verified** although `run` executes code T0 could not recover —
a silent completion that R-INT-4 forbids (#473).

Not every owned Gap means the flow is incomplete. The `DEPENDS_ON` Gaps in
the graph today are of two kinds:

- **Execution Gaps.** `gap:…#eval-unproven@N`: the owner runs code whose
  content is unknown, so what it calls is unknown.
- **Rule-evidence scope Gaps**, flagged `rule_evidence_gap: true`:
  `gap:…#rule-analysis@N` and rule-owned Gaps state where source-rule
  evidence stops, and `gap:…#eval-rules@N` (`eval_source_mapping_unknown`)
  defers rules found in *proven* eval code until their spans map to the
  original source. In each case the owner's calls were recovered; only rule
  evidence is incomplete.

`DEPENDS_ON` also carries ordering intent between recovered facts
(Terraform `depends_on`, Pulumi `dependsOn`), which is not a flow hop.

## Decision

1. The tracer queries `DEPENDS_ON` with its other flow edges, but walks a
   `DEPENDS_ON` edge only into a Gap (a `Gap` node, or a `gap:` id when the
   node is not in the slice) that is not flagged `rule_evidence_gap` on the
   node or the edge.
2. Such a Gap is an ordinary terminal hop: the hop keeps the edge's tier and
   confidence plus the Gap's `reason` and `attempted_tiers`, and the flow is
   **Partial** (AC-0219). Nothing is walked past it.
3. The rule is fail-closed. Any future Gap an adapter hangs off its owner
   through `DEPENDS_ON` counts against the flow unless the adapter flags it
   as rule-evidence scope.

## Consequences

- Flows through a symbol with an unproven eval turn Partial, score lower,
  and name the Gap in the Inspector, the dossier, and exports. Flows whose
  symbols only carry rule-evidence Gaps stay Verified.
- The flow graph slice (`list_flows`, `export_flows`, anchors, the semantic
  preview's input) now includes `DEPENDS_ON` edges. The semantic resolver
  ignores that label, and the spec bundle already traced the full snapshot,
  so the UI and exported dossiers now agree on these flows.
- Graph content and fact identity are unchanged; only flow classification
  changes.

## Alternatives

1. Give completeness-relevant Gaps an execution edge label (for example a
   `CALLS` into the Gap): rejected. It changes the TS adapter's graph
   contract and content hashes, and every other adapter would have to repeat
   the choice.
2. Count every owned Gap: rejected. Rule-evidence scope Gaps would mark
   nearly every flow through business logic Partial although execution was
   recovered, drowning the real gaps.
3. Annotate the flow without a hop: rejected. A hop reuses the existing Gap
   rendering (reason, attempted tiers, provenance) on every surface.
