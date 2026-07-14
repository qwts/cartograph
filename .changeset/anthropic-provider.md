---
'cartograph': minor
---

Anthropic Claude API provider: the first cloud reasoning lane behind
the fail-closed egress firewall, with three pinned model lanes (Haiku
for triage, Opus as the default T3 reasoning lane, Fable opt-in for the
hardest escalations with server-side refusal fallback to Opus).
Embeddings remain local-only; consent disclosures carry provider,
model, endpoint, pricing, and the Fable retention requirement; safety
refusals are a typed outcome.
