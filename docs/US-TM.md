# US-TM.md — Traceability Matrix (Cartograph)

Binds: **US ↔ AC ↔ Crate(module) ↔ Milestone ↔ Flow ↔ ADR ↔ Test**.

| US | AC range | Crate(s) | Milestone | Flow | ADR | Tests |
|----|----------|----------|-----------|------|-----|-------|
| US-0001 | AC-0001..0003, AC-0049..0050, AC-0076..0078, AC-0085, AC-0094 | ingest, core-graph, app, ui | M0–M3 | — | ADR-0001, ADR-0005 | T-0001..0003, T-0049..0050, T-0076..0078, T-0085, T-0094 |
| US-0002 | AC-0004..0006, AC-0053..0054, AC-0079..0080, AC-0095, AC-0098..0100 | adapters-lang-ts, adapters-lang-python, adapters-lang-go, adapters-lang-java, adapters-lang-kotlin, adapters-fw, core-prov, ingest, app, ui | M1, M10 | — | ADR-0003, ADR-0006 | T-0004..0006, T-0053..0054, T-0079..0080, T-0095, T-0098..0100 |
| US-0003 | AC-0007..0009, AC-0043..0048, AC-0051..0052 | adapters-lang-ts, iac, dynamic, spec, app | M2, M6 | — | ADR-0003 | T-0007..0009, T-0043..0048, T-0051..0052 |
| US-0004 | AC-0010..0012 | events, dynamic, flowtracer, app | M3, M5–M6 | F-* | ADR-0002 | T-0010..0012 |
| US-0005 | AC-0013..0014 | adapters-lang-ts(tsx), adapters-fw | M4 | F-* | ADR-0003 | T-0013..0014 |
| US-0006 | AC-0015..0017,AC-0083 | flowtracer | M3–M5 | F-* | ADR-0002 | T-0015..0017,T-0083 |
| US-0007 | AC-0018..0020,AC-0061 | core-prov, agents, ui | M0, M8 | — | ADR-0002, ADR-0006 | T-0018..0020,T-0061 |
| US-0008 | AC-0021..0022 | adapters-lang-ts, iac, semantic, llm, app | M7 | F-* | ADR-0002, ADR-0004, ADR-0010 | T-0021..0022 |
| US-0009 | AC-0023..0025,AC-0055..0056,AC-0063,AC-0087,AC-0089 | agents, llm, ingest, app, ui | M8 | — | ADR-0004 | T-0023..0025,T-0055..0056,T-0063,T-0087,T-0089 |
| US-0010 | AC-0026..0028,AC-0062,AC-0064,AC-0081,AC-0086 | core-graph, app, ui | M9 | — | ADR-0001 | T-0026..0028,T-0062,T-0064,T-0081,T-0086 |
| US-0011 | AC-0029..0031, AC-0065..0066, AC-0084 | app, flowtracer, ui | M9 | F-* | ADR-0002 | T-0029..0031, T-0065..0066, T-0084 |
| US-0012 | AC-0032..0035,AC-0057..0058,AC-0067 | core-prov, spec, agents, app, flowtracer, ui | M9–M10 | — | ADR-0002, ADR-0011 | T-0032..0035,T-0057..0058,T-0067 |
| US-0013 | AC-0036..0038,AC-0059,AC-0082,AC-0088 | spec, app, ui | M9 | F-* | ADR-0002, ADR-0012 | T-0036..0038,T-0059,T-0082,T-0088 |
| US-0014 | AC-0039..0040,AC-0060 | adapters-lang-ts, iac, core-graph, core-prov, app | M10 | — | ADR-0006, ADR-0014 | T-0039..0040,T-0060 |
| US-0015 | AC-0041..0042 | iac, spec, ui | M9 | — | ADR-0003, ADR-0013 | T-0041..0042 |
| US-0016 | AC-0071..0075 | adapters-lang-ts, adapters-fw, events, spec, app, ui | post-M10 | WebExtension flows | ADR-0003, ADR-0013 | T-0071..0075 |
| US-0017 | AC-0068..0070, AC-0093 | adapters-*, ingest, app, ui | post-M10 | — | ADR-0003, ADR-0017 | T-0068..0070, T-0093 |
| US-0018 | AC-0090..0092 | app, ui, scripts | post-M10 | — | ADR-0002 | T-0090..0092 |
| US-0019 | AC-0096..0097 | ingest, core-graph, spec, app, ui | post-M10 | — | ADR-0003 | T-0096..0097 |

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
| ADR-0011 | Content-addressed full-spec bundle and curation log |
| ADR-0012 | Explicit Markdown ADR directives and derived decision overlays |
| ADR-0013 | Explicit security facts and deterministic finding projection |
| ADR-0014 | Content-addressed delta extraction and graph reconciliation |
| ADR-0015 | Single-source semantic versioning and Changesets release intent |
| ADR-0016 | Fail-closed macOS Developer ID distribution |
| ADR-0017 | Runtime-loadable, AI-authorable adapter plugins (WASM) |
| ADR-0018 | Multi-platform port scope: Windows and mobile reader; tvOS/visionOS non-goals |
