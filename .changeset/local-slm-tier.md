---
'cartograph': minor
---

Managed local SLM tier: a versioned model catalog pins every
LLM-touching action (embedding, triage, proposal) to a local model so
provenance attributes work to an exact mapping; Ollama health probing
reports missing models and unreachable endpoints as explicit,
remediable states — never a silent failure and never a cloud fallback;
and local completions validate against the caller's schema with a
bounded retry.
