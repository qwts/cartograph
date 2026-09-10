# ADR-0026 — Local definition evidence without value substitution

- **Status:** Accepted for #398
- **Date:** 2026-09-10
- **Deciders:** Cartograph owner direction; implementation agent within SPEC-01 H2

## Context

Guarded-exit evidence can identify a binding but does not show its initializer.
The Vendure stock predicate exposes this gap. Substituting initializer text into
a guard would confuse lexical source with runtime values, mutation and scope.
New citations also change complete producer receipts; historical content must
remain immutable.

## Decision

Add version-2 local const definition evidence with exact lexical eligibility,
sanitized flat expression arenas, explicit unresolved runtime values and fixed
bounds. Preserve original conditions and interpretation flags. Use closed operator
forms to capture source structure without evaluating or simplifying it.

Read v1 facts and receipts under their existing contracts. New emissions and
receipts use distinct v2 contracts, complete source traversal and complete-fact
digests. Render definitions in the existing shared inventory/context and score
the unchanged frozen benchmark independently.

## Consequences

Developers and agents can inspect supported local initializer reasoning and its
gaps. Unsupported bindings/forms remain explicit. More source references increase
payload size; hard limits and whole-receipt omission remain visible. V1 history
is correct but lacks this evidence until explicit re-ingestion.

No runtime value proof, complete business rule, input-closure certification,
review activation, model call or target write follows from this evidence.

## Alternatives

1. Substitute initializer strings into conditions: rejected because source scope,
   order, mutation and operator structure would be lost or overstated.
2. Store only initializer text: rejected because structured dependencies remain
   unavailable to human and agent consumers.
3. Rewrite v1 facts/receipts in place: rejected because their immutable producing
   evidence and historical identities must remain unchanged.
