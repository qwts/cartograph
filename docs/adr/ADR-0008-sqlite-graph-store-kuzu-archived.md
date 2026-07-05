# ADR-0008 — SQLite recursive-CTE as primary graph store (Kuzu archived upstream)

- **Status:** Accepted
- **Date:** 2026-07-05
- **Deciders:** Chris Kane (verify-at-build executed by Claude)

## Context
SPEC-00 §4.4 picked Kuzu as the primary embedded graph store, explicitly
flagged "verify at M0: embedding fit + maintenance status" (§15), with the
SQLite recursive-CTE path as the zero-risk fallback (ADR-0006). The M0
verification found the flag was justified: Apple acquired Kùzu Inc. and the
upstream repository was archived on 2025-10-10 (final release 0.11.3).
Community forks (RyuGraph/predictable-labs, bighorn/Kineviz) exist but are
young, with unproven maintenance and no stable Rust binding story. Building a
from-scratch desktop product on an archived engine or an unproven fork
contradicts the deterministic, low-risk ethos of the integrity spine.

## Decision
- `core-graph` exposes a **`GraphStore` trait**; the primary (and only M0)
  implementation is **SQLite/WAL** — node + edge tables with recursive-CTE
  traversal — via `rusqlite` (bundled SQLite).
- The OQ-3 benchmark obligation moves to the SQLite path: if path-query
  performance at 10k+ node scale (US-0010 performance bound) is not met, a
  Kuzu-fork adapter behind the same trait is the escape hatch, adopted by a
  superseding ADR.
- The graph store and the relational/state spine (jobs, provenance log,
  config) remain **separate SQLite databases** so the graph file stays a
  disposable ingest artifact while the spine holds durable state.

## Consequences
- One embedded engine (SQLite) for both graph and spine: smaller footprint,
  one operational model, trivially deterministic re-ingest checks.
- Deep multi-hop path queries are slower than a native graph engine;
  acceptable until proven otherwise by benchmark, revisit via ADR.
- SPEC-00 §4.4/§15 amended: "Kuzu (verify)" → "SQLite-CTE primary per
  ADR-0008"; DuckDB analytical option unaffected.

## Alternatives (≤3)
- **Archived Kuzu 0.11.3** — usable today, dead upstream; unacceptable
  foundation for a new build.
- **Community fork (RyuGraph / bighorn)** — revisit once one shows sustained
  maintenance and a stable Rust binding; the trait keeps this cheap.
