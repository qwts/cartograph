# ADR-0014 — Content-addressed delta extraction and graph reconciliation

- **Status:** Accepted
- **Date:** 2026-07-13
- **Deciders:** Cartograph maintainers

## Context
US-0014 requires identical T0 graph hashes for identical input and bounded
work when a source file changes. Whole-tree reparsing wastes deterministic
work, while upsert-only graph loading leaves facts from renamed or deleted
sources behind. Cross-file calls, Terraform modules/policy documents, channel
identity, fetch resolution, and found ADR links still require a complete and
current repository view.

## Decision
- TS/TSX extraction caches each physical file's pre-join facts by BLAKE3 source
  hash. Terraform does the same per physical file plus module-address context.
  Unchanged entries are cloned and their evidence commit is retargeted without
  reparsing; new or byte-changed entries alone invoke the parser.
- Deterministic repository-wide projections rerun over cached plus changed
  facts. This refreshes explicit dependents without pretending that a file is
  isolated from imports, module expansion, policy-document resolution, config,
  event/fetch stitching, or ADR links.
- Graph loading reconciles repo-owned facts by stable node/edge identity:
  absent facts are deleted, byte-identical facts are not rewritten, and changed
  or new facts are upserted. Shared non-namespaced nodes are retained while
  another repo still has an incident fact.
- The M10 invariant compares the sorted T0 node/edge identity and provenance
  content-hash snapshot. Timestamps and insertion order never participate.
- The in-process cache is disposable with the graph and is cleared by the
  clear-graph action. Persistent/watch scheduling is post-v1; correctness never
  depends on cache survival.

## Consequences
- Unchanged re-ingest performs zero parsing and zero graph writes while yielding
  an identical deterministic hash snapshot.
- A local delta scales with changed extraction contexts plus deterministic join
  cost, not parser work over the whole repository.
- Removed source facts no longer survive as graph zombies.
- Repository-wide joins remain intentionally conservative and reproducible.

## Alternatives (≤3)
- **Cache final graph facts only** — rejected because it cannot safely refresh
  cross-file dependents or prove that parsing work scales with the delta.
- **Persist tree-sitter trees and HCL ASTs immediately** — deferred because the
  in-process content cache meets v1 re-ingest requirements with less schema and
  migration surface.
- **Skip global joins for unchanged files** — rejected because imports, module
  targets, configuration, and shared identities can make them affected.
