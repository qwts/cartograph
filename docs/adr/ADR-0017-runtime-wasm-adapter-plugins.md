# ADR-0017 — runtime-loadable, AI-authorable adapter plugins (WASM)

- **Status:** Accepted
- **Date:** 2026-07-14
- **Deciders:** Chris Kane

## Context

ADR-0003 makes adding a language/framework/event system a new **compiled-in
adapter crate**. That keeps the core open/closed but makes the answer to an
unsupported pattern at preflight a permanent Unsupported-pattern finding until
the next release. The unsupported lane (#104/#109, US-0016) should instead be
able to end in "generate and install an adapter" — including adapters authored
by an AI agent — while Cartograph is running, without weakening the T0
determinism and provenance invariants (R-INT-1..5, M10 determinism CI).

## Decision

Adapters become **WASM components executed under wasmtime**, loaded at
runtime; the ADR-0003 SPI shape (`LanguageAdapter`, framework/event
registries, cloud Capability Registry) and the compiled-in crates for
first-class languages **stay**. What this supersedes in ADR-0003 is only the
consequence "adding a language = a new adapter crate": the long tail becomes
plugin artifacts instead.

1. **Packaging & execution.** One WASM component per adapter bundling the
   tree-sitter grammar (grammars already compile to WASM) and extraction
   logic, run under wasmtime with an epoch/fuel deadline and a memory cap.
   The component exports the existing SPI (parse → facts with evidence
   spans); the host passes read-only source bytes and receives fact batches.
2. **Determinism & provenance.** Every plugin-emitted fact carries
   `extractor_id@version` **plus the plugin artifact's BLAKE3 content hash**
   pinned in provenance, so a swapped plugin can never masquerade as the same
   extractor. Re-ingest of the same commit with the same adapter set must
   remain whole-graph content-hash identical (the M10 CI invariant extends to
   plugin facts). WASM's sandboxed determinism (no ambient clock/network/fs
   unless granted) makes this hold by construction.
3. **Trust & safety.** Plugins are generated code running against private
   repos: **no network, no filesystem writes, read-only source access only
   through host calls, bounded fuel/memory — fail closed**, mirroring the
   egress-firewall posture (ADR-0004). T0 plugins never invoke an LLM at
   extraction time: the LLM authors the adapter; it does not run inside it.
4. **Conformance gate.** An adapter activates only after passing, in a
   durable job: (a) SPI contract tests, (b) a golden corpus the generator
   must supply (sample sources + expected facts, exact evidence spans),
   (c) a double-run determinism check (equal fact-set hashes). Failed or
   ungated adapters stay **proposed** — the same propose/accept curation
   semantics as T2/T3 facts (ADR-0011); analogous to the T2 paired-eval gate
   (ADR-0010).
5. **Lifecycle & UX.** Discovery from project-local `.cartograph/adapters/`
   then a user-level directory (project wins on id conflict); adapters are
   versioned by content hash, enabled/disabled per project in Settings. The
   runtime loop: Preflight flags an unsupported pattern → user or agent
   requests an adapter → generation runs as a durable job (#111 spine) →
   conformance gate → user accepts → re-ingest picks it up and the
   unsupported finding closes.
6. **v1 scope.** Runtime **loading** of pre-built, gated adapters first;
   in-app AI **generation** second (it can start as an external agent
   workflow that produces the plugin artifact and golden corpus).

## Consequences

- The unsupported-patterns lane gains a resolution path that keeps every
  invariant: sandboxed-deterministic T0 facts, pinned provenance, propose
  before activate.
- wasmtime and a component-model SPI binding become core dependencies; the
  SPI needs a versioned WIT contract, and grammar-in-component packaging adds
  build tooling for adapter authors.
- Plugin extraction is slower than native (interpreted/jitted WASM, host-call
  overhead) — accepted for long-tail languages; first-class languages stay
  compiled in.
- A malicious or wrong adapter can still emit false facts *within* its gate;
  the golden corpus bounds this but does not eliminate it — acceptance stays
  a human decision.

## Alternatives (≤3)

- **Dynamic libraries** (`libloading` + tree-sitter native dylib loading) —
  fastest, but unsandboxed native code in-process; unacceptable for
  AI-authored code against private repos.
- **Subprocess adapters over JSON/IPC** — simplest authoring, but weakest
  performance on large repos, and still needs a full OS-level sandboxing
  policy per platform; WASM gives the sandbox for free.
- **Stay compiled-in** (status quo) — no new attack surface, but unsupported
  patterns remain permanent findings between releases; rejected as the whole
  point of the spike.
