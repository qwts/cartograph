# ADR-0020 — Source rule evidence and deterministic redaction

- **Status:** Accepted for staged implementation in #381
- **Date:** 2026-09-10
- **Deciders:** Implementation agents, within the owner's SPEC-01 direction

## Context

Rule predicates and return expressions introduce source text into persistent
context and exports. Existing LLM and toolchain redaction helpers live in higher
layers and use different policies. Reusing a free-text replacement on source can
erase boolean semantics, while copying raw AST text can expose encoded literals
or comments. A conditional return also does not by itself prove validation,
rejection, or a complete execution predicate.

## Decision

Implement [SPEC-02](../SPEC-02_rule-evidence.md) in stages: correct lexical owners,
a shared pure redaction boundary, typed source observations, then a cited rule
inventory. Move existing recognizers into `core-redact` without changing LLM or
toolchain behavior. Add source-aware policy before rule text is persisted.

Keep source observations separate from behavioral interpretation. A Confirmed
guarded-exit fact confirms source syntax and ownership; unresolved consumer,
control-flow and dependency semantics retain explicit Gap records. Shared
serializable rule types live in `core-graph::rules`, referencing `core-prov`.
No source is executed and no model is called by these passes.

Advance the disposable graph identity schema from 2 to 3 using its existing
version-mismatch invalidation mechanism. Corrected lexical identities change the
meaning of previously ambiguous symbol/call facts; waiting for users to manually
re-ingest each repository would keep those old Confirmed assertions visible.

## Consequences

The first inventory will contain useful cited evidence with visible interpretation
gaps. It does not count as complete Vendure rule recovery until the independent
benchmark's semantic requirements are met. Conservative redaction may withhold
business strings; their type and source reference survive, and the loss is explicit.
Upgrading requires re-ingestion of repositories after the old graph cache is
invalidated. Separately stored job history and target source are unaffected.

Existing model consent hashes and toolchain omission behavior need regression
coverage during relocation. Rule storage, context serialization and export tests
must prove synthetic secrets absent, including escaped values and comments.

## Alternatives

1. Depend on `llm` from the T0 adapter: couples deterministic storage to provider
   infrastructure and applies an unsuitable free-text policy to AST values.
2. Copy independent regexes into each extractor: lets policies drift and leaves
   encoded literals/comments outside a coherent tested boundary.
3. Label every conditional return as a confirmed validation: invents business
   meaning and hides missing consumer/control-flow evidence.
