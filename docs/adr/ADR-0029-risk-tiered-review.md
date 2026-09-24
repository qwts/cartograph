# ADR-0029 — Risk-tiered review: owner review for protected paths only

- **Status:** Accepted
- **Date:** 2026-09-23
- **Deciders:** Chris Kane (owner), Cartograph implementation agent

## Context

`main` requires one approving review. In practice the owner approved almost
every agent PR after the Cursor Approval Agent (ACA) had already approved it,
and the ACA was configured to enforce what the owner checks. That made the
owner's click a bottleneck without adding review. Meanwhile #443 showed that
the ACA's approval alone satisfied the ruleset, so nothing enforced where a
human review *is* required, and future untrusted collaborators could change
agent directives or workflows under an automated approval.

## Decision

Split review by risk and let GitHub enforce the split:

- The ruleset requires code-owner review. `.github/CODEOWNERS` names the owner
  only for protected paths:
  - governance and CI: `.github/`, `scripts/`
  - agent primitives, meaning every agent tool's directives and hook directories
  - ADRs
  - security boundaries: redaction, model egress, every crate handed a live
    `LlmProvider` (today `agents` and `semantic`), the plugin host, app
    capabilities, the consent UI, and the whole host source tree
    (`src-tauri/src/`), since any host module can reach the cloud provider and
    the grant APIs; #457 seals those into one module and narrows this rule
  - build entrypoints that execute code where release credentials are present
  - supply chain: manifests, lockfiles and the `cargo deny` policy
  - the changelog, which marks a release

  There is no `*` rule. A PR that adds a new agent tool directory or build
  entrypoint, or passes an `LlmProvider` into a new crate, adds it to CODEOWNERS
  in the same change.
- Any other PR merges on an ACA approval of its current head plus the existing
  gates (exact-SHA CI, Advanced CodeQL, signatures, resolved threads). The
  implementing agent merges it.
- Agents also request owner review, and do not merge on the ACA alone, for
  changes that are critical but not path-covered: fact identity or graph-hash
  changes, confidence-tier or provenance semantics (R-INT-1..5), data
  migrations or loss, and anything labelled `priority:must` that changes user
  data. They add the `owner-review` label and wait.

This is a repository-level exception to the org SOP rule that automation must
not satisfy the required review; the owner accepts it for unprotected paths.

## Consequences

- Routine fixes merge without waiting on the owner; protected paths cannot
  merge without the owner, whoever opens or approves the PR.
- Stale-review dismissal still applies, so the ACA must re-approve each push.
- Dependency bumps, including lockfile-only ones, need the owner. A
  resolved graph can point at any tarball, so it is not safe on automated
  approval.
- A PR the owner authors that touches protected paths needs a bypass or a
  second code owner, since GitHub does not allow self-approval.
- The protected list is the policy. It changes only through a PR the owner
  approves, because CODEOWNERS owns itself.

## Alternatives

1. Owner approves everything (status quo): safe, but the approval had become
   a formality and slowed delivery.
2. Stop the ACA from approving (comments only): enforces human review
   everywhere, with the same bottleneck.
3. ACA approval everywhere with no protected paths: fast, but lets untrusted
   collaborators or automation change agent directives, workflows, and
   security boundaries.
