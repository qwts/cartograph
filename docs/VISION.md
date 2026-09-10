# Cartograph — Vision (beyond SPEC-00 v1)

> The staged roadmap below records the original ADR-0007 sequencing.
> [ADR-0019](adr/ADR-0019-domain-context-and-agent-interoperability.md) supersedes
> that sequencing for the owner's clarified domain context hub. Current work and
> delivery gates are defined in [SPEC-01](SPEC-01_context-hub.md); the integrity
> principles below continue to apply.

## The thesis

A modern agentic SDLC — specs, user stories with acceptance criteria, ADRs,
traceability, gated CI, autonomous agents doing the mechanical work — is how
software gets built from here on, including by people who are not software
engineers. Two things are missing:

1. **A guide.** Non-engineers (and engineers new to agentic work) have no
   opinionated tool that walks them through running a real SDLC with agents:
   what artifacts to keep, what gates to enforce, what to let agents do
   autonomously and what to review.
2. **An on-ramp for existing systems.** Large, fragmented, under-documented
   codebases can't join an agentic SDLC because nobody can state what they do.
   Their true specification — business flows, rules, decisions — is scattered
   across IaC, cloud topology, server code, events, and clients.

Cartograph attacks (2) first because it is the hard, differentiated problem:
**recover the specification with provenance, never hallucinating**. But the
recovered spec is not the end state — it is the entry ticket to (1).

## Staged roadmap

### Stage 1 — now (SPEC-00 v1, M0–M10)
Spec recovery only (NG2 intact). Meanwhile, **this repository dogfoods the
target SDLC**: every change traces to a US/AC, ADRs gate decisions, CI enforces
traceability, issues close from merged PRs, agents do the work. The repo's
process *is* the prototype of the guidance product.

### Stage 2 — the SDLC starter kit (v1.x)
The spec compiler learns one more output profile: arrange the recovered
artifacts as a ready-to-run agentic SDLC workspace for the *target* system —
user stories and ACs as a seeded backlog, recovered ADRs as the decision log,
the gap register as the first sprint of open questions, the traceability
matrix wired to CI checks. Still documents and process artifacts, not code
generation — NG2 survives, but the deliverable becomes *actionable* process,
not just a report.

### Stage 3 — SDLC in the core (v2)
Cartograph becomes the guide: guided workflows for non-engineers (capture
intent → US/AC → gates → agent execution → review), continuous re-ingest so
spec and system never drift apart, and a porting workflow for fragmented
codebases — recover → curate → re-specify → hand to agents — that preserves
the business use cases as executable acceptance criteria while the
implementation modernizes underneath them.

## What must stay true at every stage

- **Integrity spine first.** Inferred is never presented as confirmed
  (R-INT-1..5). This is the trust that makes agentic work on top safe.
- **Local-first, explicit egress.** Especially for enterprises with fragmented
  legacy code, nothing leaves the machine without consent.
- **Gaps are product.** The honest 5% is what a human (or an agent sprint)
  works on next.

## Revisiting NG2

ADR-0019 supersedes the starter-kit prerequisite for building shared domain
context and agent investigations. It does not authorize writing the ingested
target: future modernization execution requires a separately authorized external
checkout and re-ingestion against approved acceptance criteria.
