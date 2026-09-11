# ADR-0028 — Separate durable investigations from edge-resolution proposals

- **Status:** Accepted for implementation in #404
- **Date:** 2026-09-11
- **Deciders:** Chris Kane (product direction), Cartograph implementation agent

## Context

The owner wants to ask a domain context hub about business behavior, designs and
gaps, with multiple specialists and eventual MCP/ACP interoperability. Existing
AgentTask is a closed edge-resolution contract requiring a Gap, source and target
candidates. It cannot truthfully represent a general investigation. Shared context,
retained source receipts and execution ownership now provide reusable foundations,
but there is no durable in-app investigation/coordinator path.

## Decision

Implement [SPEC-10](../SPEC-10_specialist-investigations.md) with separate versioned
investigation actions, input ledgers and findings. A host coordinator owns bounded
query/read/finish steps, frozen graph context and cumulative coherent source
selection. Ship two honest T3 specialists and keep every finding InferredWeak.
Use a closed JSON action protocol over a separately bounded provider API; do not
pretend this is native provider tools or an ACP runtime.

Treat specialist prompt improvements as new immutable definitions. The first
real-local-provider observation returned an insufficient-evidence finish before
any discovery. New @2 defaults therefore explain that initial empty input requires
host-executed query/read actions; @1 definitions and saved task identities remain
unchanged. Keep the actual failed observation as evidence and evaluate @2 in a
separate run. The host does not manufacture a tool action or reject an otherwise
valid insufficient-evidence finish to force a successful benchmark.

For bounded Ollama actions, explicitly request `think: false` alongside JSON
format in transport @2. [Ollama enables thinking by default](https://docs.ollama.com/capabilities/thinking)
for supported models, while this protocol needs one bounded JSON action per turn.
The @2 specialist trial reached the existing 180-second request limit before an
admitted response; it provides no evidence about whether the guidance worked.
Requesting a concise mode is the next measured change, with no deadline or output
budget increase. Models may ignore this setting (for example GPT-OSS expects
reasoning levels), so the host never treats it as proof that reasoning is absent.
Record the immutable bounded transport version in new provider descriptors and
consent identity; preserve absent versions in older records as unknown.

Size query pages with their evidence inventories included. The actual model's
documented example request exposed a core page that fit its budget but failed
after host metadata was attached. Preserve a fitting ordered prefix and the
original selection cursor; never enlarge the budget or omit an oversized fact.

Use durable idempotent task identity and ordered events, exact per-step cloud
consent, private execution fencing and explicit unknown outcomes. Preserve complete
admitted findings against their original transient input even after cancellation.
Do not recreate lost frozen context or replay uncertain calls on restart. The app
and future protocol adapters share coordinator records and policy.

Keep raw source/tool payloads transient and store bounded redacted question/history,
references, fingerprints and admitted findings. Historical source inspection remains
receipt-pinned. Use state.db transactions for investigation identity/lifecycle,
results, receipt references and initial Job linkage. Attach execution identity
atomically with the Job claim; history survives job cleanup.

## Consequences

A developer can ask an actual scoped question and inspect a specialist's evidence
and findings without inventing graph edges or upgrading recovered facts. Tool,
byte, output, time and consent boundaries are explicit. The new bounded provider
transport also needs real TLS and body-size/stop-condition validation.

Verified TLS adds rustls-platform-verifier 0.7.0. Its wasm32 dependency
webpki-root-certs 1.0.9 contains certificate data under
[CDLA-Permissive-2.0](https://cdla.dev/permissive-2-0/), whose sharing condition
requires including the agreement text. Add a cargo-deny exception scoped to that
exact package/version for #404; keep the general license allow-list and advisory
gates intact. The pinned macOS distribution targets use platform trust and do not
include this wasm32 package, so their generated notices omit it. Adding a target
that distributes these certificates requires carrying the agreement with that
distribution and updating the target-specific notices before shipping.

Replay admission distinguishes complete source excerpts from individual graph
metadata scalars: both use the 48-scalar window, while only source excerpts and
supplied prior-finding text reject complete short items. Short graph identifiers
and literals remain usable in ordinary answers; secret scanning is unchanged.
This is exact-copy protection, not semantic declassification.

The implementation adds a substantive coordinator and app surface. Whole-graph
copying remains costly, cancellation cannot promise remote inference termination,
and RAM-only inputs constrain restart to explicit interruption/reconciliation.
H4 cross-ingress acceptance still requires real MCP; H3 activation, H5 ACP/delegation,
H6 metrics and the market pilot remain separate obligations.

## Alternatives

1. Reuse edge proposals with synthetic Gap/target records: misrepresents questions
   and findings and introduces false graph semantics.
2. Add only one-shot free-text chat: does not investigate through tools, establish
   a durable evidence ledger or support controlled cancellation/consent.
3. Persist raw prompts/source and automatically resume after restart: broadens
   retention and disclosure, can reconstruct the wrong basis and duplicate calls.
