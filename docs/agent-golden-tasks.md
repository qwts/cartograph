# Agent golden tasks

A small eval set for this repository's agent primitives
([ENG-0006](https://github.com/qwts/agent-sop/blob/main/docs/decisions/ENG-0006-agentic-primitives-governance.md)
item 4). A change to `AGENTS.md`, a vendor adapter (`CLAUDE.md`), a skill, or
a hook that claims to improve agent behavior here should be checked against
these tasks — "it reads better" is not evidence a change helped.

## How this set is used

Each task is a real merged issue whose correct outcome is already known, so
an attempt can be scored against the diff that actually landed and against
gates that fail deterministically:

- **Gate outcome** — does the attempt reach a state where the cited gates
  pass? Machine-checkable, and the primary signal.
- **Named trap** — does the attempt fall into the specific failure this task
  was chosen for? Green gates alone are not enough; a shortcut can satisfy a
  gate without avoiding the trap the task exists to catch.

The set stays small (kept to 3-5 tasks) so it's cheap enough to actually run.
This page defines the tasks; it does not define a runner.

A task is replaced when its trap stops being reachable — usually because a
gate started catching it mechanically, which is a strictly better outcome
than an eval catching it. Record why in the PR that replaces it.

## The set

### Rehash a recovered fact from its final identity, not an intermediate one (#475)

**Task:** the TypeScript `eval()`/`new Function()` adapter renames a
recovered fact's node/edge id into the eval site's namespace
(`#eval@<offset>.<name>`) after computing its `prov.content_hash`, so two
eval sites holding identical code produced distinct edges sharing one hash.
Fix it so every eval-recovered fact's hash derives from its final,
post-rename identity.

**Why this one:** the correct fix is a generalization of a narrower rehash a
prior PR (#451) had already applied only to nested Gaps and their
`DEPENDS_ON` edges. A shallow attempt patches just the reported case (the
`CALLS` edge into the eval entry) without routing every eval-recovered fact
through one shared rehash path — which reproduces the same class of
collision the next time a sibling case surfaces. The fix also carries a
one-time, expected hash churn on the next ingest, which a cautious attempt
might misread as a regression and try to suppress by preserving the old hash
— that would be wrong.

**Gates:** `cargo test -p adapters-lang-ts`; determinism (content-hash
equality across re-ingests of the same commit) is a CI-enforced invariant
from M10 on (AGENTS.md).

**Trap:** hashing only the newly-reported edge instead of the shared
namespaced-identity path, or "fixing" the expected one-time hash churn by
preserving stale hashes.

### Make an owned execution Gap a terminal, Partial hop (#473)

**Task:** a flow through `run() { const CODE = load(); eval(CODE); }`
reported Verified even though `run` executes code T0 could not recover. The
flow tracer decided completeness only from the hop labels it walked, and the
eval/rule-evidence Gaps the TS adapter hangs off their owning symbol via
`DEPENDS_ON` weren't one of them.

**Why this one:** it's a direct test of R-INT-4 ("a flow with an unresolved
hop is emitted partial with an explicit Gap node — never silently
completed", AGENTS.md). The real fix threads a new label into the tracer's
walked-edge set while excluding two look-alike cases: Gaps flagged
`rule_evidence_gap` (rule-evidence scope, not an execution gap) and plain
ordering `DEPENDS_ON` (Terraform `depends_on`, Pulumi `dependsOn` — sequence,
not a missing fact). A shallow "just walk every `DEPENDS_ON` edge" attempt
gets this wrong in either direction: over-counting ordering edges into false
Partial flows, or under-counting by excluding real execution Gaps too.

**Gates:** `cargo test -p flowtracer`; `node scripts/check-traceability.mjs`
— the landed PR added a user-story AC row and an ADR in the same PR as the
code, per "spec before code" (AGENTS.md).

**Trap:** reporting the flow Verified (or silently dropping the Gap) instead
of a terminal, reason-carrying Gap hop; or walking every `DEPENDS_ON` edge
indiscriminately and turning ordinary ordering dependencies into spurious
Partial flows.

### Split review by risk instead of reviewing everything (#443)

**Task:** an automated approval alone satisfied GitHub's required review, so
no PR reliably got a human look — including ones touching security
boundaries or the agent-primitive files agents themselves read as
directives. Change the review policy so routine PRs still merge on automated
approval, but a named set of protected paths and change categories require
the human owner.

**Why this one:** the tempting shortcut is either extreme. Leaving every path
owner-gated reintroduces the friction the issue exists to remove. Removing
owner review entirely reopens the exact hole the issue reports, for exactly
the files this repository's own governance needs protected: `AGENTS.md`,
`CLAUDE.md`, `.claude/`/`.codex/`/`.cursor/`/`.windsurf/`, `docs/adr/`,
security-boundary crates, and dependency manifests/lockfiles. The correct
answer names an explicit, no-wildcard path allowlist in CODEOWNERS plus a
documented `owner-review` label escape hatch for changes no path pattern can
see (fact identity, graph-hash, provenance/confidence semantics, data loss).

**Gates:** the ruleset actually enforcing `require_code_owner_review` on the
named paths (verify in the repo's branch protection / ruleset settings, not
just the CODEOWNERS file — a rule that exists but isn't required enforces
nothing); `docs/adr/` carries the ADR recording the decision.

**Trap:** a `CODEOWNERS` wildcard (`* @owner`) that looks like it satisfies
"protect the sensitive paths" while silently re-gating everything; or
narrowing protection to literal paths only, missing the critical-but-
unlocatable categories (fact identity, graph-hash changes) that need the
`owner-review` label escape hatch instead of a path rule.

### Scope a "whole-graph" invariant to what a change actually touches (#466)

**Task:** a registration's recorded content hash — meant to prove "this
exact commit under this exact registration re-ingests identically" — changed
when an *unrelated* repository elsewhere in the same multi-repo workspace
was re-ingested. The stored hash was computed over the whole graph, not the
facts this registration's own ingest actually owns.

**Why this one:** the fix has to preserve the existing whole-graph tallies
(`graph_facts`) that other call sites — including tests asserting the old,
correct whole-graph-scoped behavior for a single-repo workspace — legitimately
depend on, while narrowing only the *content-hash* determinism claim to a
per-registration scope. A shallow attempt that unconditionally scopes the
hash by `coverage_repos` breaks call sites that intentionally pass an empty
`coverage_repos` expecting whole-graph hashing (a real regression caught only
by running the full `cargo test --workspace`, not a scoped crate test). This
is also a graph-hash/determinism-semantics change under AGENTS.md's critical
category — it needs the `owner-review` label even though it touches no
CODEOWNERS path.

**Gates:** `cargo test --workspace` (not just the owning crate — the
regression here was in a caller two crates away); determinism is a
CI-enforced invariant from M10 on.

**Trap:** narrowing the hash scope unconditionally and breaking the
whole-graph callers that still need it; or fixing the symptom without
recognizing this as a graph-hash semantics change requiring owner review.
