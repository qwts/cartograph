# ADR-0021 — Durable host-owned proposal staging before curated projection

- **Status:** Accepted for the staged implementation in #384
- **Date:** 2026-09-10
- **Deciders:** Cartograph owner direction; implementation agent within SPEC-01

## Context

H3 requires consistent reviewed context. Current escalation results are transient,
and the decision command accepts complete caller-supplied proposal bodies. Their
existing edge fact hash excludes annotation and citation selection. Existing
decision rows therefore cannot establish the exact host-produced material reviewed
by the human, nor restore pending work after restart.

## Decision

Persist immutable broker results with complete-content identities, producing job
and snapshot, and bounded versioned task-basis manifests before reporting success.
Store source fingerprints rather than raw task evidence. Before persistence,
reject annotation replay under the bounded normalized-text policy in SPEC-03;
retain accepted annotations unchanged. This admission check cannot establish
arbitrary generated-text confidentiality or retrospectively certify prototype
records from hashes alone. Review resolves a staged
ID on the host and atomically checks the expected review revision. Retain legacy
rows as historical records. Mark current source capture as working-tree evidence
whose binding to cited revisions is unverified.

Implement this prerequisite before the shared curated projection. Acceptance in
this slice records review and explicitly awaits context reconciliation; it never
promotes T3, mutates recovered facts, or silently certifies evidence freshness.

## Consequences

Pending and reviewed work survives restart and job cleanup, and annotation or
citation changes cannot reuse the identity of earlier reviewed content. Storage
failure prevents reporting a successful staged result. The app must update its
review transport and explain the current delivery boundary. Fingerprints bind what
the host supplied but do not certify that current checkout bytes match the cited
revision or the original parse. Per-file source capture and reconciliation for
local and Git inputs are a separate prerequisite for activating curated facts.

This does not finish H3. Directed-slot proof, source/candidate reconciliation,
conflict handling and a single UI/export/agent projection remain required.

## Alternatives

1. Project current decision rows directly: rejected because they do not bind the
   complete reviewed material or an immutable host-produced task basis.
2. Store full source prompts for replay: rejected because fingerprints and source
   references retain the comparison basis without a new raw-source archive.
3. Build staging and full projection together: deferred to keep this prerequisite
   reviewable while preserving the explicit H3 acceptance gate.
