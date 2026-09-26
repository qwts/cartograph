# ADR-0033 — Proven-external JVM import ids key on the package

- **Status:** Proposed in #464 (owner review)
- **Date:** 2026-09-24
- **Deciders:** Cartograph owner; implementation agent

## Context

ADR-0031 (#237) gave every placeholder `Module` node provenance and a proven
`external`/`internal`/`unresolved` boundary, but left ids unchanged: a JVM
import proven external still mints `mod:{full imported target}` —
`mod:jakarta.persistence.Entity` for a type, `mod:org.junit.Assert.assertEquals`
for a static member. That means two proven-external imports from the same
package (`jakarta.persistence.Entity` and `jakarta.persistence.Table`) create
two distinct `Module` nodes, one per imported name, instead of one node for
the package both came from. ADR-0031 explicitly deferred this ("`mod:` ids
that name an imported type rather than a module are an id-scheme question,
tracked in #464") because it changes the graph contract
(`GRAPH_SCHEMA_VERSION`), not just classification.

## Decision

1. **A proven-external JVM import's `Module` node and `IMPORTS` edge key on
   its package, not the imported type or static member.** The split uses the
   JVM/Kotlin capitalization convention already relied on elsewhere in the
   adapters: the first dotted segment that starts uppercase begins the
   imported type or member, everything before it is the package
   (`jakarta.persistence.Entity` → package `jakarta.persistence`). A path with
   no capitalized segment (a wildcard import already stripped to its bare
   package) keeps its full path as its id, since there is no boundary to
   split on.
2. **The edge's `specifier` still names exactly what was imported.** Re-keying
   is id-only: the existing `props.specifier` (already stored verbatim on
   every `IMPORTS` edge) is untouched, so which type or member a given import
   site actually named is never lost, only no longer part of the node id.
3. **Only a boundary already proven `external` is re-keyed.** Classification
   (ADR-0031) runs first, against the import's full path — the capitalization
   split alone cannot distinguish an in-system prefix (`com.demo.Foo.Bar`,
   internal) from a proven-external one, so `internal`/`unresolved` imports
   keep their full-path id unchanged.
4. **Two imports proven external from the same package collapse to one
   `Module` node**, via the existing repeated-relation rule (AC-0195): the
   edge keeps the last occurrence's props in deterministic extraction order
   and cites every occurrence's evidence.
5. **Graph-contract change.** This bumps `GRAPH_SCHEMA_VERSION` (4 → 5): a
   mismatched db is cleared on open (ADR-0008), so no migration is written —
   every affected graph re-ingests its proven-external JVM imports under the
   new package-keyed ids on its next recovery.

## Consequences

- Fewer, coarser `Module` nodes for proven-external JVM boundaries: a repo
  importing many names from one package (a common Spring/JUnit/Kotlin
  coroutines pattern) now gets one node per package instead of one per
  imported name, matching how the Go, Python, and JS/TS adapters already key
  external boundaries (a module/package path, not an imported symbol).
- `mod:` ids for `internal` and `unresolved` (Gap) JVM imports are unaffected
  — they keep naming the full import path, since only a proven-external
  boundary is re-keyed.
- Any exported reference to the old per-type `mod:` id (saved views, external
  tooling keyed on graph ids) breaks on re-ingest. No such export exists yet
  in-app; this is a pre-1.0 graph-contract change, consistent with the three
  prior `GRAPH_SCHEMA_VERSION` bumps.
- Existing databases are cleared and rebuilt on next open (ADR-0008's
  disposable-artifact model), not migrated in place.

## Alternatives

1. **Leave ids on the full imported path, key only presentation on the
   package:** rejected. The duplicate-node problem (one `Module` per imported
   name from the same package) would persist in the graph itself; only the UI
   would look deduplicated, and store-level queries (fact counts, collapsed
   relations) would still overcount identical external dependencies.
2. **Key on the whole import statement's declared root package from the
   manifest, not a capitalization heuristic:** rejected. JVM has no
   manifest-declared package list to consult (unlike Go's `go.mod` or npm's
   `package.json`), and the capitalization convention is exact for every
   case these adapters can observe (headers are already scanned character by
   character for the same convention in `classify_import`).
3. **Defer indefinitely, keep the status quo:** rejected. The duplicate-node
   noise compounds every time a repository imports multiple names from one
   external package, which is the common case for annotation-heavy
   frameworks (JPA, JUnit, Spring).
