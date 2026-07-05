# ADR-0004 — Pluggable LLM, local-first, per-tier cloud opt-in

- **Status:** Accepted
- **Date:** 2026-06-21
- **Deciders:** Chris Kane

## Context
Semantic (T2) and agentic (T3) tiers need models, but the app is local-first and
privacy-sensitive. Provider choice must be swappable.

## Decision
A `LlmProvider` trait abstracts all model access (`locality` = Local | Cloud).
**Ollama is the default** for embeddings and agent completions. Cloud providers
(Claude/Grok/GPT/Gemini) are **opt-in per tier and per action**, gated by an egress
consent dialog that shows the exact span-level payload. A Local-only policy makes
cloud calls **hard-fail closed** (no silent egress). Secrets are redacted from payloads.

## Consequences
- Privacy by default; provider independence; clean test seams.
- Local model quality bounds T2/T3 unless the user opts into cloud.

## Alternatives (≤3)
- **Cloud-default** — better quality, unacceptable default egress.
- **Single hard-wired local model** — simpler, no provider independence.
