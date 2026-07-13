# ADR-0010 — USearch + Ollama semantic staging with paired-eval gating

- **Status:** Accepted
- **Date:** 2026-07-12
- **Deciders:** Cartograph maintainers

## Context
M7 must fill unresolved channel/call hops semantically without weakening the
escalation ladder. SPEC-00 selected Ollama by default, tentatively selected
USearch, and required the current Rust bindings for USearch/FastEmbed to be
verified before adoption. Semantic similarity is model-dependent, so a raw
score cannot by itself authorize an inferred link.

The M7 verification found maintained Rust releases and current APIs:

- USearch 2.26 provides a supported Rust `Index` with cosine search and F32
  vectors; it builds and runs in this workspace without OpenMP or unsafe code
  in Cartograph ([upstream Rust guide](https://docs.rs/crate/usearch/latest/source/rust/README.md)).
- FastEmbed 5.17 provides local ONNX embeddings, but downloads and owns a
  second model runtime/cache ([crate docs](https://docs.rs/fastembed/latest/fastembed/)).
- Ollama's current batch endpoint is `POST /api/embed` with `model` + `input`
  and normalized vectors ([official API](https://github.com/ollama/ollama/blob/main/docs/api.md#generate-embeddings)).

## Decision
- Implement the `LlmProvider` SPI in `llm`; the M7 provider is Ollama and its
  local implementation rejects every non-loopback URL. It never pulls a model
  silently.
- Use USearch 2.26 with cosine distance and F32 vectors behind the `semantic`
  crate's `AnnIndex` wrapper. Keep default features off for a portable desktop
  build with no OpenMP runtime requirement.
- T2 reads only explicit Gap slots and existing evidenced candidates. It emits
  staged `SemanticProposal`s with Semantic/InferredStrong provenance citing
  both sides; it never writes to the confirmed graph.
- Calibrate a similarity threshold from labeled positive/negative pairs. A
  best-effort overlay admits the best proposal per Gap only when the held-out
  operating point clears the configured precision floor. Failed or malformed
  evals fail closed and export no T2 link.
- FastEmbed remains a possible future local provider, not an automatic
  fallback: silently changing embedding models would invalidate the calibrated
  eval threshold.

## Consequences
- M7 can resolve channel/call gaps while R-INT-1 remains structurally true.
- Eval/model identity must travel with preview/export results; changing the
  model requires re-running the paired eval.
- Ollama availability and model installation are explicit local prerequisites;
  provider errors surface rather than causing cloud egress or model download.
- The preview overlay is ephemeral. Human accept/reject persistence belongs to
  the M9 Workbench story.

## Alternatives (≤3)
- **FastEmbed as an automatic fallback** — local and viable, rejected because
  an implicit model change invalidates eval calibration and duplicates runtime
  ownership.
- **Brute-force cosine only** — simpler, rejected because M7 explicitly verifies
  the ANN path needed for project-scale candidate sets.
- **Write passing proposals directly into the graph** — rejected because it
  collapses staging into confirmed state and makes R-INT-1 harder to audit.
