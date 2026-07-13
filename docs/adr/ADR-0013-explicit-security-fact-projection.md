# ADR-0013 — Explicit security facts and deterministic finding projection

- **Status:** Accepted
- **Date:** 2026-07-13
- **Deciders:** Cartograph maintainers

## Context
US-0015 requires unauthenticated endpoints and over-broad IAM grants to appear
as findings mapped to US/AC. Treating the absence of an auth edge as proof that
an endpoint is public would violate the escalation ladder: global middleware,
gateway auth, or an unsupported framework could exist outside the recovered
facts. IAM wildcard syntax, by contrast, is explicit deterministic input and
must remain visible on `GRANTS` edges for a projection to evaluate it.

## Decision
- An unauthenticated-endpoint finding requires an explicit negative auth fact
  on the Endpoint (`authenticated: false` or an equivalent explicit auth
  state). Missing auth evidence produces no security assertion.
- T0 IAM extraction retains literal wildcard actions and resource scopes on
  `GRANTS` per policy statement; actions are joined only across statements
  governing the same resolved target, and `NotAction` is not represented as
  `Action`. A wildcard in either is an over-broad-grant finding in v1.
- The spec compiler derives disposable `Finding` nodes after R-INT-5 and
  content-hash curation have filtered supporting facts. Findings preserve the
  supporting confidence and evidence and map to US-0015 plus AC-0041/AC-0042.
- The Workbench exports a stable `security.md` artifact and displays its
  finding count. Derived findings never mutate the confirmed graph.

## Consequences
- Unknown auth posture remains unknown instead of becoming a false Confirmed
  vulnerability.
- Wildcard IAM intent stays inspectable with the exact action and scope that
  triggered the finding.
- Inferred support yields an inferred, curatable finding and cannot bypass a
  prior rejection through a new derived hash.
- Additional deterministic security rules can extend the same projection
  without weakening the explicit-input contract.

## Alternatives (≤3)
- **Flag every endpoint without an auth edge** — rejected because absence is
  not proof in an incomplete recovered graph.
- **Parse policy text only in the Workbench** — rejected because extraction is
  the span-preserving deterministic tier and must retain the source facts.
- **Persist findings in the graph** — rejected because they are compiler
  projections governed by export mode and curation.
