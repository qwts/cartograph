# ADR-0002 — Escalation ladder + provenance-first integrity model

- **Status:** Accepted
- **Date:** 2026-06-21
- **Deciders:** Chris Kane

## Context
Spec recovery fails when inferred facts are presented as confirmed. Pure-static misses
semantics; pure-LLM hallucinates. We need trustworthy, reproducible output.

## Decision
Organize all extraction as a four-tier ladder — **Deterministic (T0) > Dynamic (T1) >
Semantic (T2) > Agentic (T3)** — where a fact is produced by the lowest tier that can
establish it. Every node/edge carries provenance {tier, confidence_tier, evidence,
extractor_id, content_hash}. Unresolved hops become explicit **Gap** nodes
(Score=0 preference). Hard rules R-INT-1..5 govern tier interaction; agents are
propose-only and cannot write or upgrade T0/T1 facts.

## Consequences
- Output is auditable and reproducible; inferred ≠ confirmed everywhere.
- More engineering than a single-strategy tool; tiers are independently testable.
- Enables `verified-only` vs `best-effort` export modes.

## Alternatives (≤3)
- **Static-only** — fast, reproducible, but misses cross-layer business flows.
- **LLM-first** — high coverage, low trust; unacceptable for an "official spec".
