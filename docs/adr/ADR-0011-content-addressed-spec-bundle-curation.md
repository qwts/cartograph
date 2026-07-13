# ADR-0011 — Content-addressed full-spec bundle and curation log

- **Status:** Accepted
- **Date:** 2026-07-13
- **Deciders:** Cartograph maintainers

## Context
M9 must turn the provenance graph into the complete official artifact set and
let a human accept, reject, or annotate inferred assertions without weakening
the escalation ladder. Existing commands return topology and flow text
independently, while the durable state spine stores decisions only for bounded
T3 proposals. A Workbench needs one consistent R-INT-5 projection across every
artifact and a curation key that remains valid only while the exact assertion
survives re-ingest.

## Decision
- The `spec` crate compiles one typed, deterministic `SpecBundle`. It always
  contains user stories, US-TM, flow dossiers, resource topology, data model,
  ADR set, Gap register, Drift register, and security findings in stable order.
- Every artifact carries structured `SpecAssertion` rows. Each row retains the
  complete producing `Provenance` (tier, confidence, all evidence spans,
  extractor id, and content hash), and portable artifact text includes the same
  provenance inline.
- One `ExportMode` is applied to the entire bundle. `verified-only` includes
  Confirmed and InferredStrong, while `best-effort` additionally includes
  InferredWeak. Both retain explicit Gaps; neither hides the Gap or Drift
  registers.
- Workbench curation lives in a separate SQLite/WAL table on the durable state
  spine, keyed by the assertion's existing `content_hash`. Only cited T2/T3
  assertions are curatable. Accept and annotate preserve the original
  confidence tier; reject suppresses that exact hash from subsequent bundles.
- The Tauri API returns the typed bundle and curation records. React remains a
  presentational review/export surface and cannot mutate confirmed graph facts.

## Consequences
- All official artifacts share one auditable export policy instead of drifting
  across independent UI toggles.
- Re-ingest naturally reapplies a decision only when the assertion content hash
  is unchanged; changed evidence or identity returns to undecided.
- Human acceptance never upgrades an inference to Confirmed, preserving
  R-INT-1, R-INT-2, and R-INT-3.
- Artifact types that have no recovered facts still export an explicit empty
  artifact, so absence cannot masquerade as a compiler omission.

## Alternatives (≤3)
- **Store curation on graph facts** — rejected because the graph is disposable
  ingest output and this would blur human state with recovered truth.
- **Key decisions by display id** — rejected because an unchanged name can hide
  changed evidence; `content_hash` is the required re-ingest basis.
- **Separate endpoint per artifact** — rejected because independent filtering
  can violate R-INT-5 consistency across the official set.
