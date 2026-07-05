# ADR-0007 — Strict agentic SDLC from inception; dogfood v1, expand scope later

- **Status:** Accepted
- **Date:** 2026-07-05
- **Deciders:** Chris Kane

## Context
Cartograph's long-term vision (see `docs/VISION.md`) is bigger than SPEC-00's
locked scope: the SDLC itself should live in Cartograph's core — a guide for
agentic development by non-engineers and a tool for porting large fragmented
codebases onto a modern agentic SDLC while preserving their business use cases.
SPEC-00 v0.1 deliberately forbids scaffolding/regeneration (NG2). Building the
expanded product before the recovery engine exists would be building Rome in
one session. Meanwhile, this repository itself needs a development process, and
that process is itself product research.

## Decision
1. **Product scope:** SPEC-00 stands as written for v1, including NG2. The
   deliverable of v1 is the recovered specification, full stop. The expanded
   SDLC-in-core direction is recorded in `docs/VISION.md` and revisited via a
   future ADR once M0–M10 exit gates prove the recovery engine.
2. **Process scope:** this repository runs a strict, agent-first SDLC from the
   first pull request, and that process is the *reference implementation* of
   what Cartograph will one day teach and export. Process automation is adopted
   as early as it is applicable — CI gates, traceability checks, issue
   automation exist before any product code does.
3. **Canonical tracker:** GitHub Issues + a GitHub Project, scriptable via
   `gh`, are the single source of truth for work items. `docs/archive/
   tracker.csv` is a frozen pre-import snapshot. Milestones M0–M10 mirror
   SPEC-00 §14.
4. **Docs split:** specs, user stories, traceability matrix, and ADRs live
   in-repo (they gate PRs); narrative contributor/process docs live in the
   GitHub wiki, with `CONTRIBUTING.md` as a pointer stub (image-trail pattern).
5. **Every change is traceable:** a PR must reference the US/AC or issue it
   serves; CI enforces spec/traceability consistency (`scripts/
   check-traceability.mjs`); merged PRs close their linked issues
   automatically.

## Consequences
- The repo's own history becomes a worked example of agentic SDLC — reusable
  as product content later.
- Process overhead lands before code exists; the first PRs are slower, every
  later PR is cheaper and auditable.
- NG2 stays intact, so v1 cannot silently grow porting/scaffolding features;
  expansion requires a deliberate ADR superseding this one.

## Alternatives (≤3)
- **Revise SPEC-00 now** — add SDLC-guidance/porting as v1 capabilities.
  Rejected: expands surface before any exit gate is proven.
- **Two-track specs (SPEC-01 now)** — a parallel spec for the SDLC layer.
  Deferred: becomes attractive once the recovery engine is real; VISION.md
  holds the material until then.
