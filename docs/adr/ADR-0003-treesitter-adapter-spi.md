# ADR-0003 — tree-sitter deterministic extraction + adapter SPI

- **Status:** Accepted
- **Date:** 2026-06-21
- **Deciders:** Chris Kane

## Context
Multiple languages, IaC dialects, cloud providers, event systems, and client frameworks
must be supported without core rewrites (open/closed, plugin-based).

## Decision
Deterministic extraction is built on **tree-sitter** grammars behind a stable **adapter SPI**
(`LanguageAdapter`, framework/event registries, cloud Capability Registry). Adding a
language/cloud/event system = a new adapter crate; no core changes. First-class order:
TS/JS → Python → Go; Terraform + Pulumi; AWS → Azure → GCP; SQS/SNS/EventBridge + Kafka
+ in-proc buses; React/Next → Vue → Svelte (+ GraphQL).

## Consequences
- Uniform, incremental, multi-language parsing; clean extension surface.
- Inter-procedural call graphs are weak in dynamically typed languages — accepted; those
  hops escalate to T1/T2 and may become Gaps.

## Alternatives (≤3)
- **Native compilers/LSP per language** — highest fidelity, highest integration cost.
- **ANTLR** — flexible grammars, weaker incremental/multi-language ergonomics.
