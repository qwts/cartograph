# Agent Golden Tasks

The eval set for this repository's agent primitives
([ENG-0006](https://github.com/qwts/agent-sop/blob/main/docs/decisions/ENG-0006-agentic-primitives-governance.md)
item 4). A change to `AGENTS.md`, a vendor adapter (`CLAUDE.md`), a slash
command, or a skill that claims to improve agent behavior cites evidence from
these tasks. "It reads better" is not evidence.

## How this set is used

Each task is a **merged issue whose correct outcome is already known**, so an
agent's attempt can be scored against the diff that actually landed and
against gates that fail deterministically. A task is scored on two axes:

- **Gate outcome** — did the attempt reach a state where the named gates
  pass? This is machine-checkable and is the primary signal.
- **Named trap** — did the attempt fall into the specific failure this task
  was chosen for? This is why raw pass/fail is not enough: an attempt can
  produce green gates by taking a shortcut the gates don't catch.

Tasks are run from the issue text alone, against the parent commit of the
merge that closed them. The set stays small — three to five — so the gate is
cheap enough to survive; ENG-0006 §5 puts the shared harness in
`qwts/agent-sop` when it stabilizes, so this page defines the tasks, not the
runner.

**Replacing a task** is allowed when its trap stops being reachable — for
example because a gate now catches it mechanically, which is a better outcome
than an eval catching it. Record why in the PR that replaces it.

## The set

### G1 — Rehash a fact from its final form, not its intermediate one (issue #475, PR #496)

**Task:** the TypeScript eval adapter renames a nested fact's id into the
eval site's namespace (`#eval@<offset>…`) but was computing the fact's
`prov.content_hash` before the rename. Two eval sites holding identical code
therefore produced distinct edges that shared one hash. Fix it so every
eval-recovered node and edge is hashed from its final, post-rename form.

**Why this one:** the shipped fix is a generalization, not a special case —
it extends the same rehash a prior PR (#451) had only applied to nested
Gaps and their `DEPENDS_ON` edges, so a shallow fix that patches just the new
repro (the `CALLS` edge into the eval entry) without routing it through the
same rehash path reproduces the bug's sibling one PR later. It also carries a
one-time, expected hash churn on next ingest that a cautious agent might
mistake for a regression and try to "fix" by preserving the old hash.

**Gates:** `cargo test -p adapters-lang-ts`; the full local gate list in
`AGENTS.md` (content-hash equality is a CI-enforced determinism invariant).

**Trap:** hashing only the new repro's edge instead of routing every
eval-recovered fact through one rehash function, leaving the next eval-nested
case to reintroduce the same class of collision.

### G2 — Make an owned execution Gap a terminal, Partial hop (issue #473, PR #506)

**Task:** a flow through `run() { const CODE = load(); eval(CODE); }`
reported **Verified**, although `run` executes code T0 could not recover.
The flow tracer decided completeness only from the hops it walked, and the
TS adapter's eval/rule-evidence Gaps hang off the owning symbol through
`DEPENDS_ON`, a label the tracer didn't walk at all.

**Why this one:** it is a direct test of R-INT-4 ("a flow with an unresolved
hop is emitted partial with an explicit Gap node — never silently
completed"). The correct fix requires threading a new label into
`FLOW_EDGE_LABELS` while *excluding* `rule_evidence_gap`-flagged Gaps and
plain ordering `DEPENDS_ON` (Terraform `depends_on`, Pulumi `dependsOn`) from
counting — a rule with two carve-outs that a shallow "just walk
`DEPENDS_ON`" patch gets wrong in either direction (over-counts ordering
edges into false Partials, or under-counts by excluding real execution
Gaps too).

**Gates:** `cargo test -p flowtracer`; `node scripts/check-traceability.mjs`
(this PR added AC-0219/T-0219 and ADR-0032 in the same PR as the code, per
"spec before code").

**Trap:** silently reporting the flow Verified (or dropping the Gap) instead
of surfacing it as a terminal, reason-carrying Gap hop — the exact R-INT-4
failure the invariant exists to catch. A close second trap: walking every
`DEPENDS_ON` edge as a hop, which turns ordinary infra ordering dependencies
into spurious Partial flows.

### G3 — Split review by risk instead of reviewing everything (issue #443, PR #450)

**Task:** `cursor[bot]`'s approval alone satisfied GitHub's required review,
so no PR reliably got a human look — including ones touching security
boundaries or agent-primitive files. Fix the review policy so routine PRs
still merge on automated approval, but protected paths require the owner.

**Why this one:** the tempting shortcut is either extreme — leave `* @qwts`
in `CODEOWNERS` (which reintroduces the friction the issue exists to remove,
since it makes every PR wait on the owner) or remove owner review entirely
(which reopens #443's hole for exactly the files this repo's own governance —
`AGENTS.md`, `CLAUDE.md`, `.claude/`, `docs/adr/`, `crates/core-redact`,
`crates/llm`, `src-tauri/capabilities/`, `Cargo.toml`/`package.json` — needs
protected). The correct answer is naming an explicit path allowlist and
adding a documented `owner-review` label escape hatch for changes no path
pattern can see (fact identity, graph-hash, provenance/confidence semantics,
data loss).

**Gates:** none mechanical — scored on the CODEOWNERS diff and the read-back
described in the PR (`require_code_owner_review: true`; a protected-path PR
with only a bot approval shows `BLOCKED`).

**Trap:** picking one of the two easy extremes (`* @qwts` everywhere, or no
owner path left at all) instead of the narrow protected-path list — or
naming a wildcard rule, which `AGENTS.md`'s review policy explicitly
forbids ("Never add a `*` rule to CODEOWNERS").
