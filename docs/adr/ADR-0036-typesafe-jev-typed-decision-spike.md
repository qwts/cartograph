# ADR-0036 — TypeSafe Jev as a consented typed-decision provider (spike, deferred)

- **Status:** Deferred in #429 (spike blocked on prerequisites; not owner-reviewed yet)
- **Date:** 2026-09-24
- **Deciders:** Cartograph owner; implementation agent

## Context

The escalation ladder (ADR-0002) has an expensive gap between T0/T1 and T3:
the local T2 embedder gives similarity only, with no calibrated decision, and
a bounded T3 completion model is slow, costly per call, and self-reports its
own confidence. Several open items feel this gap directly: the #240 VSCode
gap register (275k assertions) can't afford a completion model per grouping
decision; #237's placeholder boundary classification needs internal/
external/gap among a bounded set; #244's Atlas band/cluster assignment is a
bounded-choice problem; and #258 needs a *decision* among resolved T2
candidates, not just a similarity score.

[TypeSafe Jev](https://typesafe.ai/blog/introducing-system-one-models-and-jev)
is pitched as a "System One" model: unstructured state in, typed
probabilistic decisions out, with no free-text generation. Per its public
blog post (fetched 2026-09-24, the only source available — this vendor was
not independently reachable beyond that page):

- Input/output: "unstructured data... with an emphasis on structured program
  state" in; "type-safe structured values... defined in advance" out. Outputs
  are generated in parallel rather than token-by-token, and the vendor states
  it "can't hallucinate" as a structural property of the output shape.
- Question types: the issue describes `Choice` (cardinality up to 255),
  `Score`, and `Noul`; the blog independently confirms "supports a
  cardinality up to 255" and that Jev "gives up string generation" — i.e. no
  free text out, consistent with a typed-decision rather than completion
  shape.
- Latency: stated as 70–500 ms end-to-end, "40x–200x faster" than frontier
  completion models for the same class of task.
- Cost: $0.042 / MTok input; output tokens stated as free ("too cheap to
  meter").
- Calibration: "always communicates confidence and uncertainty," with a
  claim that "higher confidence means higher accuracy" — stated, not shown.
- Availability: "early access," "bringing developers off the waitlist" as of
  the 2026-09-15 announcement — not GA.
- Deployment: cloud service ("West Coast"); no on-prem or local option is
  mentioned anywhere in the source material.

All of the above is vendor-stated. None of it is independently verified in
this spike — see "What could not be evaluated" below.

## What was evaluated

**Architecture fit**, against the actual `crates/llm` code on `main` (not
just the issue's description of it):

- `LlmProvider` (`crates/llm/src/lib.rs`) is completion/embedding-shaped:
  `embed(&self, batch: &[String])` and
  `complete(&self, _request: &ProviderCompletionRequest)`, both returning
  free text or vectors. There is no typed, enumerated-choice output shape
  anywhere in this trait. Bending Jev into it (e.g. asking it to "complete"
  with a choice id as text) would throw away the one property that makes it
  interesting — a closed, typed answer set with per-option probabilities —
  and would re-open free-text parsing failure modes Jev is designed to avoid.
  The issue's proposal to add a narrow, separate typed-decision trait rather
  than extend `LlmProvider` is correct given what's actually in this trait
  today.
- The consent/egress path (`EgressFirewall`, `EgressPolicy`, `ConsentGrant`,
  `EgressPreview`, `CloudDisclosure`) is generic over *what* is being sent,
  not over completion specifically — `EgressPreview` carries a
  `CompletionPayload` today, but the firewall's job (redact, default-deny
  cloud per tier, bind one grant to one exact hashed payload, fail closed
  with no grant) is payload-shape-agnostic in its enforcement logic even
  though its current payload type isn't. A typed-decision request (state +
  question) fits the same preview/consent/redact/fail-closed shape as a
  completion request; it would need a payload variant or a sibling type next
  to `CompletionPayload`, not a new enforcement path. This is architecturally
  sound and doesn't require touching `R-INT-1..5` enforcement itself.
- `AnalysisTier` today has exactly two variants, `Semantic` (T2) and
  `Agentic` (T3); there is no existing tier that means "cloud-backed,
  typed-output, non-local." Whichever way this resolves, it is a new
  `AnalysisTier` variant or a documented reuse of an existing one with
  different consequences attached — not a no-op.

**Product-invariant fit** against `AGENTS.md`:

- R-INT-1 (T2/T3 never overwrite/upgrade a T0/T1 fact) and R-INT-3
  (agents/T3 are propose-only) are unaffected by *this* provider's shape —
  they're enforced by what a caller is allowed to do with a tier's output,
  not by which model produced it. Nothing about Jev's typed-answer format
  changes that enforcement surface.
- R-INT-2 (tier + confidence stored, inferred never indistinguishable from
  confirmed) and R-INT-5 (`verified-only` export excludes InferredWeak) are
  exactly where the open question lives: Jev's self-reported confidence
  score is not the same thing as *this repo's* calibration, and nothing in
  the vendor material demonstrates the two coincide. Storing a Jev
  `confidence` value as if it were an already-validated Cartograph
  confidence would violate R-INT-2 in spirit even if the field name matches.
- "Deterministic tier never calls the LLM": satisfied trivially — cloud-only,
  so it is out for T0/T1 under any reading, exactly as the issue states.

## What could not be evaluated

The issue's core requirement — measure Jev's precision@1 and a calibration
curve against the #258 `LabeledPair`s set, side by side with the current
local T2 resolver, plus p50/p95 latency and cost on that same set — was
**not run**, for two independent reasons, either of which alone would block
it:

1. **No TypeSafe Jev API access.** Jev is cloud-only and in limited early
   access off a waitlist (per the vendor's own blog, 2026-09-15). This spike
   has no API key, no account, and no way to provision one from this agent
   context. Every latency/cost/calibration number above is a vendor claim,
   not a measurement.
2. **The eval substrate doesn't exist yet.** #258 ("derive LabeledPairs from
   T0-resolved edges") is still open on `main` as of this spike; there is no
   `LabeledPair` type or harness in `crates/semantic` to run *any* provider
   against, local or cloud. The comparison this issue asks for is not
   buildable until #258 lands, independent of Jev.

Because of (2), even an agent with a working API key could not have produced
the requested comparison today. This is a scheduling fact about the repo,
not a property of Jev.

## Decision

**Defer.** This spike does not adopt, and does not reject, Jev as a
provider. It records the architecture and invariant analysis above so the
eval — once it's runnable — isn't starting from zero, and it sets an
explicit gate before Jev touches any real path:

1. No production integration of Jev (#237, #240, #244, or SPEC-01 H2) may
   proceed until this ADR is superseded by one that adopts, backed by a real
   `docs/evals/` report against #258's `LabeledPair`s set — precision@1,
   calibration curve (confidence vs. observed accuracy), p50/p95 latency,
   and cost, run against Jev credentials and compared against the local T2
   resolver on the identical pairs. Vendor-claimed numbers are not a
   substitute for that report anywhere in this repo's decision record.
2. If and when that eval runs, whoever runs it should build a narrow
   typed-decision trait (not extend `LlmProvider`) that reuses
   `EgressFirewall`/`EgressPolicy`/`ConsentGrant`/`EgressPreview`/
   `CloudDisclosure` behind a new payload variant, per the architecture
   analysis above.
3. This ADR does not fold Jev into the existing `Semantic` tier without
   change: SPEC-00 defines T2 as "local embeddings + similarity"
   (`docs/SPEC-00_master.md:58`), and Jev is cloud-only, so that reuse is
   not obviously correct. Pending the eval, the default is that an adopted
   Jev needs its own bounded lane under the T3 ceiling (`InferredWeak`)
   rather than `Semantic`/`InferredStrong` — the higher bar, not the lower
   one, until data justifies otherwise. That default is exactly
   what the eval's calibration curve should settle: if Jev's confidence
   tracks observed accuracy as tightly as the local resolver's does on the
   same pairs, `Semantic` is defensible; if it doesn't, it needs
   `InferredWeak` treatment or a new tier. Deciding this without the curve
   would be exactly the failure mode this repo's integrity model exists to
   prevent — a confident inferred edge that reads as confirmed.
4. Until superseded, a Jev-sourced edge — if anyone builds even a throwaway
   adapter to poke at the API — must never be stored as `Confirmed`, and
   every call must fail closed with no `ConsentGrant`, per the existing
   `EgressFirewall` default-deny behavior. No new enforcement code is needed
   for this; it falls out of routing any Jev call through the existing
   firewall rather than around it.

## Consequences

- No code changes. `crates/llm`, `crates/semantic`, and every listed
  consumer (#237, #240, #244) are untouched by this ADR.
- #429's requirement 1 (the measured eval) is not satisfied and cannot be,
  by this or any agent, until #258 lands. That dependency should be made
  explicit wherever #429-derived follow-up work is tracked.
- The next actor on this line of work needs both a #258-derived
  `LabeledPair`s harness and provisioned Jev API access before any further
  progress is possible — this is a two-part blocker, not one.
- Vendor claims in this document (latency, cost, calibration) are dated
  2026-09-15/2026-09-24 and from a single blog post during the vendor's
  stated early-access period; they should be treated as unverified marketing
  claims until the in-repo eval either confirms or contradicts them, and
  re-checked if adoption is revisited later, since early-access pricing and
  latency are unlikely to be final.

## Alternatives

1. **Build the throwaway eval-only adapter now, without real Jev access,
   using synthetic/mocked responses.** Rejected: it would validate the
   trait shape and consent wiring (already done analytically above) but
   would produce fabricated precision/calibration numbers, which is worse
   than no numbers — this repo's own principle is "Gap over unsupported
   assertion," and a mocked eval report would be exactly an unsupported
   assertion wearing an eval report's shape.
2. **Reject Jev outright now on cloud-only grounds.** Rejected: cloud-only
   already excludes it from T0/T1 by the existing local-first rule, without
   needing a new decision, and several real candidate slots (#240, #244,
   #258-adjacent T2 candidate selection) are T2/T3-shaped, where consented
   cloud egress is already an accepted pattern (ADR-0004). Ruling it out
   before the calibration question is answered would foreclose a
   potentially real win over the current T3 fallback without evidence
   either way.
3. **Adopt provisionally pending eval, allowing a flagged/dark-launch
   integration behind a kill switch.** Rejected: R-INT-2/R-INT-5 are
   exactly the invariants at risk from an unvalidated confidence signal;
   this repo's CODEOWNERS and ADR-0029 already route confidence-tier and
   provenance-semantics changes through owner review specifically to avoid
   probationary integrations of that kind. A dark launch would still be a
   production integration in every way that matters to R-INT-2.
