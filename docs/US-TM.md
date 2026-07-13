# US-TM.md — Traceability Matrix (Cartograph)

Binds: **US ↔ AC ↔ Crate(module) ↔ Milestone ↔ Flow ↔ ADR ↔ Test**.

| US | AC range | Crate(s) | Milestone | Flow | ADR | Tests |
|----|----------|----------|-----------|------|-----|-------|
| US-0001 | AC-0001..0003, AC-0049..0050 | ingest, core-graph, app, ui | M0–M3 | — | ADR-0001, ADR-0005 | T-0001..0003, T-0049..0050 |
| US-0002 | AC-0004..0006 | adapters-lang-ts, adapters-fw, core-prov | M1 | — | ADR-0003, ADR-0006 | T-0004..0006 |
| US-0003 | AC-0007..0009, AC-0043..0048 | iac, dynamic, spec, app | M2, M6 | — | ADR-0003 | T-0007..0009, T-0043..0048 |
| US-0004 | AC-0010..0012 | events, dynamic, flowtracer, app | M3, M5–M6 | F-* | ADR-0002 | T-0010..0012 |
| US-0005 | AC-0013..0014 | adapters-lang-ts(tsx), adapters-fw | M4 | F-* | ADR-0003 | T-0013..0014 |
| US-0006 | AC-0015..0017 | flowtracer | M3–M5 | F-* | ADR-0002 | T-0015..0017 |
| US-0007 | AC-0018..0020 | core-prov, agents | M0, M8 | — | ADR-0002, ADR-0006 | T-0018..0020 |
| US-0008 | AC-0021..0022 | adapters-lang-ts, iac, semantic, llm, app | M7 | F-* | ADR-0002, ADR-0004, ADR-0010 | T-0021..0022 |
| US-0009 | AC-0023..0025 | agents, llm | M8 | — | ADR-0004 | T-0023..0025 |
| US-0010 | AC-0026..0028 | app, ui | M9 | — | ADR-0001 | T-0026..0028 |
| US-0011 | AC-0029..0031 | app, flowtracer, ui | M9 | F-* | ADR-0002 | T-0029..0031 |
| US-0012 | AC-0032..0035 | spec, ui | M9–M10 | — | ADR-0002 | T-0032..0035 |
| US-0013 | AC-0036..0038 | spec, agents | M9 | — | ADR-0002 | T-0036..0038 |
| US-0014 | AC-0039..0040 | core-graph, core-prov | M10 | — | ADR-0006 | T-0039..0040 |
| US-0015 | AC-0041..0042 | iac, spec | M9 | — | ADR-0003 | T-0041..0042 |

## Coverage assertions
- Every Must-priority US is anchored to a milestone ≤ M10.
- Every AC has at least one test ID reserved (T-XXXX), to be authored alongside the AC (test-trace mapping standard).
- Integrity rules R-INT-1..5 are covered by US-0007 (AC-0019, AC-0020), US-0006 (AC-0016), US-0012 (AC-0034).

## ADR index
| ADR | Title |
|-----|-------|
| ADR-0001 | Tauri + Rust core + React web UI |
| ADR-0002 | Escalation ladder + provenance-first integrity model |
| ADR-0003 | tree-sitter deterministic extraction + adapter SPI |
| ADR-0004 | Pluggable LLM, local-first, per-tier cloud opt-in |
| ADR-0005 | GitHub App auth + git2 clone + system topology manifest |
| ADR-0006 | Kuzu graph store + SQLite/WAL spine + content-addressed determinism |
| ADR-0007 | Strict agentic SDLC from inception; dogfood v1, expand scope later |
| ADR-0008 | SQLite recursive-CTE as primary graph store (Kuzu archived upstream) |
| ADR-0009 | v1 GitHub auth ladder: env token → gh CLI; GitHub App deferred |
| ADR-0010 | USearch + Ollama semantic staging with paired-eval gating |
